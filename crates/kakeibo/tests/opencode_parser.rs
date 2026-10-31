//! Integration, SQLite WAL write-lock invariance, and CLI tests for OpenCode & Gemini CLI (`PLAN.md` §5, §10 T10).

mod common;

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime};

use common::{ExpectedBlock, ExpectedHints, ExpectedTotals, ExpectedUsage, TestEnv};
use kakeibo_core::{AgentId, BlockKind, Confidence, TurnRecord};
use kakeibo_parse::hash::hash_cwd;
use kakeibo_parse::{
    extract_opencode_session_cwds, parse_opencode_legacy_storage, parse_opencode_root,
    parse_opencode_sqlite,
};
use kakeibo_report::ndjson::write_ledger;
use rusqlite::{params, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn block_kind_slug(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::UserPrompt => "user_prompt",
        BlockKind::AssistantText => "assistant_text",
        BlockKind::AssistantThinking => "assistant_thinking",
        BlockKind::ToolUse => "tool_use",
        BlockKind::ToolResult => "tool_result",
        BlockKind::SubagentResult => "subagent_result",
        BlockKind::CompactionSummary => "compaction_summary",
        BlockKind::SystemContext => "system_context",
    }
}

fn confidence_slug(c: Confidence) -> &'static str {
    match c {
        Confidence::Measured => "measured",
        Confidence::Estimated => "estimated",
        Confidence::NotRecorded => "not_recorded",
    }
}

fn sha256_and_mtime(path: &Path) -> (String, SystemTime) {
    let bytes = fs::read(path).expect("read db bytes for sha256");
    let meta = fs::metadata(path).expect("stat db metadata");
    let mtime = meta.modified().expect("db modified mtime");
    let digest = Sha256::digest(&bytes);
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    (hex, mtime)
}

fn assert_session_matches_ground_truth(
    records: &[TurnRecord],
    exp_session: &common::ExpectedSession,
) {
    assert_eq!(
        records.len(),
        exp_session.turns.len(),
        "turn count mismatch for session {}",
        exp_session.session_id
    );

    let mut sum_prompt = 0_u64;
    let mut sum_fresh = 0_u64;
    let mut sum_write = 0_u64;
    let mut sum_read = 0_u64;
    let mut sum_out = 0_u64;
    let mut sum_reasoning = 0_u64;

    for (tr, exp) in records.iter().zip(&exp_session.turns) {
        assert_eq!(tr.agent, AgentId::Opencode);
        assert_eq!(tr.session_id, exp_session.session_id);
        assert_eq!(tr.turn, exp.turn);
        assert_eq!(tr.ts.to_string(), exp.ts);
        assert_eq!(tr.model.as_deref(), Some(exp.model.as_str()));
        assert_eq!(tr.usage.prompt_tokens(), exp.prompt_tokens);

        let actual_usage = ExpectedUsage {
            fresh_in: tr.usage.fresh_in,
            cache_write: tr.usage.cache_write,
            cache_read: tr.usage.cache_read,
            output: tr.usage.output,
            reasoning: tr.usage.reasoning,
            confidence: confidence_slug(tr.usage.confidence).to_string(),
        };
        assert_eq!(
            actual_usage, exp.usage,
            "usage mismatch on session {} turn {}",
            exp_session.session_id, exp.turn
        );

        let actual_blocks: Vec<ExpectedBlock> = tr
            .added
            .iter()
            .map(|b| ExpectedBlock {
                kind: block_kind_slug(b.kind).to_string(),
                bytes: b.bytes,
                tool: b.tool.clone(),
            })
            .collect();
        assert_eq!(
            actual_blocks, exp.added_blocks,
            "added_blocks mismatch on session {} turn {}",
            exp_session.session_id, exp.turn
        );

        let actual_hints = ExpectedHints {
            resume_start: tr.hints.resume_start,
            compaction: tr.hints.compaction,
            sidechain: tr.hints.sidechain,
            api_error: tr.hints.api_error,
        };
        assert_eq!(
            actual_hints, exp.hints,
            "hints mismatch on session {} turn {}",
            exp_session.session_id, exp.turn
        );

        sum_prompt += tr.usage.prompt_tokens();
        sum_fresh += tr.usage.fresh_in;
        sum_write += tr.usage.cache_write;
        sum_read += tr.usage.cache_read;
        sum_out += tr.usage.output;
        sum_reasoning += tr.usage.reasoning.unwrap_or(0);
    }

    assert_eq!(
        ExpectedTotals {
            turns: u32::try_from(records.len()).expect("turns len"),
            prompt_tokens: sum_prompt,
            fresh_in: sum_fresh,
            cache_write: sum_write,
            cache_read: sum_read,
            output: sum_out,
            reasoning: sum_reasoning,
        },
        exp_session.totals
    );
}

/// Populates a live WAL `opencode.db` directly from a staged legacy `storage/` tree.
fn populate_sqlite_from_staged_storage(db_path: &Path, storage_dir: &Path) {
    let conn = Connection::open(db_path).expect("create test opencode.db");
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE session (
             id TEXT PRIMARY KEY,
             project_id TEXT NOT NULL,
             parent_id TEXT,
             slug TEXT,
             directory TEXT NOT NULL,
             title TEXT NOT NULL,
             version TEXT NOT NULL,
             time_created INTEGER NOT NULL,
             time_updated INTEGER NOT NULL
         );
         CREATE TABLE message (
             id TEXT PRIMARY KEY,
             session_id TEXT NOT NULL,
             time_created INTEGER NOT NULL,
             time_updated INTEGER NOT NULL,
             data TEXT NOT NULL
         );
         CREATE TABLE part (
             id TEXT PRIMARY KEY,
             message_id TEXT NOT NULL,
             session_id TEXT NOT NULL,
             time_created INTEGER NOT NULL,
             time_updated INTEGER NOT NULL,
             data TEXT NOT NULL
         );",
    )
    .expect("initialize opencode sqlite schema");

    let session_root = storage_dir.join("session");
    for proj_entry in fs::read_dir(&session_root).expect("read storage/session") {
        let proj_dir = proj_entry.expect("proj entry").path();
        if !proj_dir.is_dir() {
            continue;
        }
        for ses_entry in fs::read_dir(&proj_dir).expect("read project session dir") {
            let ses_path = ses_entry.expect("ses entry").path();
            if ses_path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let ses_val: Value =
                serde_json::from_slice(&fs::read(&ses_path).expect("read ses json"))
                    .expect("parse ses json");
            let sid = ses_val["id"].as_str().expect("ses id");
            let project_id = ses_val["projectID"].as_str().unwrap_or("prj_default");
            let parent_id = ses_val["parentID"].as_str();
            let slug = ses_val["slug"].as_str().unwrap_or("");
            let directory = ses_val["directory"].as_str().unwrap_or("");
            let title = ses_val["title"].as_str().unwrap_or("");
            let version = ses_val["version"].as_str().unwrap_or("1.2.4");
            let t_created = ses_val["time"]["created"].as_i64().unwrap_or(0);
            let t_updated = ses_val["time"]["updated"].as_i64().unwrap_or(t_created);

            conn.execute(
                "INSERT INTO session (id, project_id, parent_id, slug, directory, title, version, time_created, time_updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![sid, project_id, parent_id, slug, directory, title, version, t_created, t_updated],
            )
            .expect("insert session row");

            let msg_dir = storage_dir.join("message").join(sid);
            if msg_dir.is_dir() {
                let mut msg_paths: Vec<_> = fs::read_dir(&msg_dir)
                    .expect("read msg dir")
                    .map(|e| e.expect("msg entry").path())
                    .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                    .collect();
                msg_paths.sort();

                for msg_path in msg_paths {
                    let raw_msg = fs::read_to_string(&msg_path).expect("read msg json");
                    let msg_val: Value = serde_json::from_str(&raw_msg).expect("parse msg json");
                    let mid = msg_val["id"].as_str().expect("msg id");
                    let m_created = msg_val["time"]["created"].as_i64().unwrap_or(t_created);
                    let m_updated = msg_val["time"]["completed"].as_i64().unwrap_or(m_created);

                    conn.execute(
                        "INSERT INTO message (id, session_id, time_created, time_updated, data)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![mid, sid, m_created, m_updated, raw_msg],
                    )
                    .expect("insert message row");

                    let prt_dir = storage_dir.join("part").join(mid);
                    if prt_dir.is_dir() {
                        let mut prt_paths: Vec<_> = fs::read_dir(&prt_dir)
                            .expect("read part dir")
                            .map(|e| e.expect("part entry").path())
                            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                            .collect();
                        prt_paths.sort();

                        for (idx, prt_path) in prt_paths.into_iter().enumerate() {
                            let raw_prt = fs::read_to_string(&prt_path).expect("read part json");
                            let prt_val: Value =
                                serde_json::from_str(&raw_prt).expect("parse part json");
                            let pid = prt_val["id"].as_str().expect("part id");
                            let p_ts = m_created + i64::try_from(idx).unwrap_or(0);
                            conn.execute(
                                "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                                params![pid, mid, sid, p_ts, p_ts, raw_prt],
                            )
                            .expect("insert part row");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn opencode_legacy_tree_and_subagent_cost_match_ground_truth() {
    // 1. `legacy-tree`: disjoint cache.{read,write}, reasoning, tool I/O, and zero `cost` -> None.
    {
        let env = TestEnv::new();
        let (_, expected) = env.stage_fixture("opencode", "legacy-tree");
        let _ro = env.make_logs_readonly();

        let opencode_root = env.data_dir().join("opencode");
        let parsed = parse_opencode_legacy_storage(&opencode_root.join("storage"))
            .expect("parse legacy storage");
        assert_eq!(parsed.sessions.len(), 1);
        assert_eq!(parsed.diagnostics.ignored_records, 0);
        assert!(parsed.diagnostics.issues.is_empty());

        let batch = &parsed.sessions[0];
        assert!(batch.parent_session_id.is_none());
        assert_session_matches_ground_truth(&batch.records, &expected.sessions[0]);

        let expected_hash = hash_cwd("/synthetic/workspace/opencode-alpha");
        for tr in &batch.records {
            assert_eq!(tr.cwd_hash.as_deref(), Some(expected_hash.as_str()));
            assert_eq!(
                tr.usage.agent_reported_cost_usd, None,
                "zero-initialized `cost: 0` must map to None, never Some(0.0)"
            );
        }

        let mut ledger_buf = Vec::new();
        write_ledger(&mut ledger_buf, &batch.records).expect("write ledger");
        let ledger_str = String::from_utf8(ledger_buf).expect("utf-8 ledger");
        assert!(!ledger_str.contains("/synthetic/workspace/opencode-alpha"));
    }

    // 2. `subagent-cost`: parent + child `parentID` linking, positive `cost`, compaction marker.
    {
        let env = TestEnv::new();
        let (_, expected) = env.stage_fixture("opencode", "subagent-cost");
        let _ro = env.make_logs_readonly();

        let opencode_root = env.data_dir().join("opencode");
        let parsed = parse_opencode_root(&opencode_root).expect("parse opencode root");
        assert_eq!(parsed.sessions.len(), 2);
        assert_session_matches_ground_truth(&parsed.sessions[0].records, &expected.sessions[0]);
        assert_session_matches_ground_truth(&parsed.sessions[1].records, &expected.sessions[1]);

        let parent_sid = &expected.sessions[0].session_id;
        assert_eq!(
            parsed.sessions[1].parent_session_id.as_deref(),
            Some(parent_sid.as_str())
        );
        assert_eq!(
            parsed.sessions[1].records[0].parent_session_id.as_deref(),
            Some(parent_sid.as_str())
        );
        assert_eq!(
            parsed.sessions[0].records[0].usage.agent_reported_cost_usd,
            Some(0.0425)
        );
        assert_eq!(
            parsed.sessions[0].records[1].usage.agent_reported_cost_usd,
            Some(0.0185)
        );
        assert_eq!(
            parsed.sessions[1].records[0].usage.agent_reported_cost_usd,
            Some(0.0125)
        );

        let cwds =
            extract_opencode_session_cwds(&opencode_root).expect("extract opt-in opencode cwds");
        assert_eq!(
            cwds.get(parent_sid).map(String::as_str),
            Some("/synthetic/workspace/opencode-beta")
        );
    }
}

#[test]
fn opencode_sqlite_matches_legacy_storage_and_deduplicates_dual_installs() {
    let env = TestEnv::new();
    let (_, expected) = env.stage_fixture("opencode", "subagent-cost");
    let opencode_root = env.data_dir().join("opencode");
    let storage_dir = opencode_root.join("storage");
    let db_path = opencode_root.join("opencode.db");

    let legacy_parsed =
        parse_opencode_legacy_storage(&storage_dir).expect("parse legacy storage baseline");

    populate_sqlite_from_staged_storage(&db_path, &storage_dir);
    let sqlite_parsed = parse_opencode_sqlite(&db_path).expect("parse sqlite opencode.db");

    assert_eq!(sqlite_parsed.sessions, legacy_parsed.sessions);
    assert_eq!(sqlite_parsed.sessions.len(), expected.sessions.len());

    // Add a legacy-only session in `storage/` while keeping duplicate `subagent-cost` sessions in both;
    // `parse_opencode_root` must deduplicate shared session IDs in favor of `opencode.db` and keep the legacy-only session.
    let extra_ses_dir = storage_dir.join("session/prj_legacy_only");
    let extra_msg_dir = storage_dir.join("message/ses_legacy_only_9999");
    let extra_prt_dir = storage_dir.join("part/msg_legacy_only_a1");
    fs::create_dir_all(&extra_ses_dir).expect("create extra ses dir");
    fs::create_dir_all(&extra_msg_dir).expect("create extra msg dir");
    fs::create_dir_all(&extra_prt_dir).expect("create extra prt dir");

    fs::write(
        extra_ses_dir.join("ses_legacy_only_9999.json"),
        r#"{"id":"ses_legacy_only_9999","projectID":"prj_legacy_only","parentID":null,"directory":"/synthetic/workspace/opencode-legacy","title":"legacy","version":"1.1.0","time":{"created":1793450000000,"updated":1793450005000}}"#,
    )
    .expect("write extra ses");
    fs::write(
        extra_msg_dir.join("msg_legacy_only_a1.json"),
        r#"{"id":"msg_legacy_only_a1","sessionID":"ses_legacy_only_9999","role":"assistant","modelID":"gpt-5-codex","providerID":"openai","cost":0,"time":{"created":1793450001000,"completed":1793450005000},"tokens":{"input":500,"output":60,"reasoning":10,"cache":{"read":200,"write":0}}}"#,
    )
    .expect("write extra msg");
    fs::write(
        extra_prt_dir.join("prt_legacy_only_1.json"),
        r#"{"id":"prt_legacy_only_1","sessionID":"ses_legacy_only_9999","messageID":"msg_legacy_only_a1","type":"text","text":"legacy reply"}"#,
    )
    .expect("write extra prt");

    let combined = parse_opencode_root(&opencode_root).expect("parse combined opencode root");
    assert_eq!(
        combined.sessions.len(),
        3,
        "shared sessions must be deduplicated while legacy-only sessions remain visible"
    );
}

#[test]
fn opencode_sqlite_unchanged_hash_and_mtime_under_active_concurrent_write_lock() {
    let env = TestEnv::new();
    let (_, expected) = env.stage_fixture("opencode", "legacy-tree");
    let opencode_root = env.data_dir().join("opencode");
    let storage_dir = opencode_root.join("storage");
    let db_path = opencode_root.join("opencode.db");

    populate_sqlite_from_staged_storage(&db_path, &storage_dir);
    // Remove legacy `storage/` so CLI reads strictly from `opencode.db`.
    fs::remove_dir_all(&storage_dir).expect("remove legacy storage copy");

    // Checkpoint committed frames into `opencode.db` before opening the concurrent writer.
    {
        let setup_conn = Connection::open(&db_path).expect("open db for checkpoint");
        setup_conn
            .execute_batch("PRAGMA wal_checkpoint(FULL);")
            .expect("checkpoint committed pages");
    }

    // Hold an active `BEGIN IMMEDIATE` write lock with uncommitted writes on a second connection!
    let writer_conn = Connection::open(&db_path).expect("open concurrent writer connection");
    writer_conn
        .execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO session (id, project_id, parent_id, slug, directory, title, version, time_created, time_updated)
             VALUES ('ses_uncommitted_lock', 'prj_lock', NULL, 'uncommitted', '/synthetic/uncommitted', 'uncommitted', '1.2.4', 1793460000000, 1793460000000);",
        )
        .expect("acquire active BEGIN IMMEDIATE write lock");

    let (hash_before, mtime_before) = sha256_and_mtime(&db_path);
    thread::sleep(Duration::from_millis(25));

    // 1. Direct library read while write lock is held.
    let parsed =
        parse_opencode_sqlite(&db_path).expect("read opencode.db while write lock is held");
    assert_eq!(parsed.sessions.len(), 1);
    assert_session_matches_ground_truth(&parsed.sessions[0].records, &expected.sessions[0]);

    // 2. Subprocess CLI read (`sessions` + `export`) while write lock is STILL held.
    let cli_sessions = String::from_utf8(
        env.kakeibo_cmd()
            .args(["sessions", "--agent", "opencode"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8 sessions stdout");
    assert!(cli_sessions.contains("opencode"));
    assert!(cli_sessions.contains(&expected.sessions[0].session_id[..8]));
    assert!(!cli_sessions.contains("ses_unco"));

    let export_json: Value = serde_json::from_slice(
        &env.kakeibo_cmd()
            .args([
                "export",
                "--since",
                "2026-10-31T00:00:00Z",
                "--format",
                "json",
            ])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .expect("parse exported json under write lock");
    assert_eq!(export_json["sessions"].as_array().map(Vec::len), Some(1));

    let (hash_after, mtime_after) = sha256_and_mtime(&db_path);
    assert_eq!(
        hash_after, hash_before,
        "opencode.db SHA-256 hash must remain completely unchanged under concurrent write lock"
    );
    assert_eq!(
        mtime_after, mtime_before,
        "opencode.db nanosecond mtime must remain completely unchanged under concurrent write lock"
    );

    writer_conn
        .execute_batch("ROLLBACK;")
        .expect("rollback concurrent writer cleanly");
}

#[test]
fn cli_end_to_end_gemini_and_opencode_under_readonly_permissions() {
    let env = TestEnv::new();
    let (_, gemini_rewind_gt) = env.stage_fixture("gemini-cli", "rewind-patch");
    let (_, gemini_legacy_gt) = env.stage_fixture("gemini-cli", "legacy-json");
    let (_, opencode_gt) = env.stage_fixture("opencode", "subagent-cost");

    // Materialize a checkpointed `opencode.db` and remove `storage/` + WAL sidecars so
    // `make_logs_readonly()` (`0555` directory, `0444` DB file) exercises the immutable read-only fallback too!
    let opencode_root = env.data_dir().join("opencode");
    let storage_dir = opencode_root.join("storage");
    let db_path = opencode_root.join("opencode.db");
    populate_sqlite_from_staged_storage(&db_path, &storage_dir);
    {
        let conn = Connection::open(&db_path).expect("open db to truncate wal");
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;")
            .expect("checkpoint and finalize db file before chmod 0555");
    }
    fs::remove_dir_all(&storage_dir).expect("remove storage dir");

    let _ro = env.make_logs_readonly();
    let (db_hash_before, db_mtime_before) = sha256_and_mtime(&db_path);

    // 1. `sessions --agent gemini-cli`
    let gem_sessions = String::from_utf8(
        env.kakeibo_cmd()
            .args(["sessions", "--agent", "gemini-cli"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8 gemini sessions");
    assert!(gem_sessions.contains("gemini-cli"));
    assert!(gem_sessions.contains(&gemini_rewind_gt.sessions[0].session_id[..8]));
    assert!(gem_sessions.contains(&gemini_legacy_gt.sessions[0].session_id[..8]));
    assert!(gem_sessions.contains("unpriced"));
    assert!(!gem_sessions.contains("$0"));

    // 2. `sessions --agent opencode` (subagent folded into parent, agent-reported cost `$0.0735`)
    let oc_sessions = String::from_utf8(
        env.kakeibo_cmd()
            .args(["sessions", "--agent", "opencode"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8 opencode sessions");
    assert!(oc_sessions.contains("opencode"));
    assert!(oc_sessions.contains("ses_0001"));
    assert!(
        !oc_sessions.contains("ses_0002"),
        "child subagent session must fold into parent rather than appearing as a separate primary row:\n{oc_sessions}"
    );
    assert!(
        oc_sessions.contains("$0.0735 (agent-reported)"),
        "expected summed parent + subagent agent-reported cost:\n{oc_sessions}"
    );

    // 3. `export` without and with `--show-paths` for both Gemini CLI and OpenCode
    let gem_prefix = &gemini_rewind_gt.sessions[0].session_id[..8];
    let gem_json_default: Value = serde_json::from_slice(
        &env.kakeibo_cmd()
            .args(["export", gem_prefix, "--format", "json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .expect("parse gemini default json");
    assert!(gem_json_default["sessions"][0]["project_path"].is_null());

    let gem_json_paths: Value = serde_json::from_slice(
        &env.kakeibo_cmd()
            .args(["export", gem_prefix, "--format", "json", "--show-paths"])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .expect("parse gemini --show-paths json");
    assert_eq!(
        gem_json_paths["sessions"][0]["project_path"].as_str(),
        Some("/synthetic/workspace/gemini-alpha")
    );

    let oc_json_paths: Value = serde_json::from_slice(
        &env.kakeibo_cmd()
            .args(["export", "ses_0001", "--format", "json", "--show-paths"])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .expect("parse opencode --show-paths json");
    assert_eq!(
        oc_json_paths["sessions"][0]["project_path"].as_str(),
        Some("/synthetic/workspace/opencode-beta")
    );
    assert_eq!(
        oc_json_paths["sessions"][0]["subagents"][0]["session_id"].as_str(),
        Some(opencode_gt.sessions[1].session_id.as_str())
    );

    // 4. `doctor` reports all 4 sources without leaking raw `$HOME` / `$XDG_DATA_HOME` paths
    let doctor_out = String::from_utf8(
        env.kakeibo_cmd()
            .arg("doctor")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8 doctor");
    assert!(doctor_out.contains("claude-code"));
    assert!(doctor_out.contains("codex"));
    assert!(doctor_out.contains("gemini-cli"));
    assert!(doctor_out.contains("opencode"));
    assert!(doctor_out.contains("1.2.4"));
    assert!(!doctor_out.contains(&env.home_dir().to_string_lossy().to_string()));
    assert!(!doctor_out.contains(&env.data_dir().to_string_lossy().to_string()));

    let (db_hash_after, db_mtime_after) = sha256_and_mtime(&db_path);
    assert_eq!(db_hash_after, db_hash_before);
    assert_eq!(db_mtime_after, db_mtime_before);
}
