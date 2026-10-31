//! Integration, SQLite WAL write-lock invariance, `"not recorded"` rendering,
//! and end-to-end CLI tests for Cursor (`state.vscdb`) and Aider (`PLAN.md` §5, §10 T11).

mod common;

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

use common::{ExpectedBlock, ExpectedHints, ExpectedTotals, ExpectedUsage, TestEnv};
use kakeibo_core::{AgentId, BlockKind, Confidence, TurnRecord};
use kakeibo_parse::hash::hash_cwd;
use kakeibo_parse::{extract_cursor_session_cwds, parse_cursor_vscdb, PathCatalog};
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

fn assert_cursor_session_matches_ground_truth(
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
        assert_eq!(tr.agent, AgentId::Cursor);
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

#[test]
fn cursor_zero_and_estimated_fixtures_match_ground_truth_and_drop_raw_text() {
    let env = TestEnv::new();
    let (staged_zero, gt_zero) = env.stage_fixture("cursor", "zero-tokens");
    let (staged_est, gt_est) = env.stage_fixture("cursor", "estimated-tokens");
    assert_eq!(staged_zero, staged_est);
    let vscdb_path = &staged_zero[0];

    let _ro_guard = env.make_logs_readonly();

    let catalog = PathCatalog::new(Some(env.home_dir().to_path_buf()), None, None);
    let discovered = catalog
        .discover_cursor_state_dbs()
        .expect("discover cursor state.vscdb");
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].agent, AgentId::Cursor);
    assert_eq!(&discovered[0].path, vscdb_path);

    let out = parse_cursor_vscdb(vscdb_path).expect("parse cursor state.vscdb");
    assert!(out.diagnostics.issues.is_empty());
    assert_eq!(out.diagnostics.ignored_records, 1);
    assert_eq!(out.sessions.len(), 2);

    let zero_sid = &gt_zero.sessions[0].session_id;
    let est_sid = &gt_est.sessions[0].session_id;

    let zero_batch = out
        .sessions
        .iter()
        .find(|s| &s.session_id == zero_sid)
        .expect("zero-tokens composer batch");
    let est_batch = out
        .sessions
        .iter()
        .find(|s| &s.session_id == est_sid)
        .expect("estimated-tokens composer batch");

    assert_cursor_session_matches_ground_truth(&zero_batch.records, &gt_zero.sessions[0]);
    assert_cursor_session_matches_ground_truth(&est_batch.records, &gt_est.sessions[0]);

    for tr in &zero_batch.records {
        assert_eq!(tr.usage.confidence, Confidence::NotRecorded);
        assert_eq!(tr.usage.estimation_method, None);
        assert_eq!(
            tr.cwd_hash.as_deref(),
            Some(hash_cwd("/synthetic/workspace/cursor-zero").as_str())
        );
    }
    for tr in &est_batch.records {
        assert_eq!(tr.usage.confidence, Confidence::Estimated);
        assert_eq!(
            tr.usage.estimation_method.as_deref(),
            Some("cursor_bubble_token_count")
        );
        assert_eq!(
            tr.cwd_hash.as_deref(),
            Some(hash_cwd("/synthetic/workspace/cursor-est").as_str())
        );
    }

    let cwds = extract_cursor_session_cwds(vscdb_path).expect("extract opt-in cursor cwds");
    assert_eq!(
        cwds.get(zero_sid).map(String::as_str),
        Some("/synthetic/workspace/cursor-zero")
    );
    assert_eq!(
        cwds.get(est_sid).map(String::as_str),
        Some("/synthetic/workspace/cursor-est")
    );

    // Ensure NDJSON ledger contains zero raw text from cursor_kv.json.
    let mut ndjson = Vec::new();
    write_ledger(&mut ndjson, &out.all_records()).expect("write NDJSON ledger");
    let ledger_str = String::from_utf8(ndjson).expect("UTF-8 NDJSON");
    assert!(!ledger_str.contains("Synthetic zero-token composer"));
    assert!(!ledger_str.contains("Synthetic estimated-token composer"));
    assert!(!ledger_str.contains("relativeWorkspacePath"));
}

#[test]
fn cursor_vscdb_handles_blob_values_and_preserves_sha256_and_mtime_under_wal_write_lock() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("state.vscdb");

    {
        let writer = Connection::open(&db_path).expect("create WAL state.vscdb");
        writer
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);",
            )
            .expect("init WAL cursorDiskKV with BLOB column");

        let comp_json = br#"{"_v":3,"composerId":"comp-wal-1","createdAt":1793455200000,"modelConfig":{"modelName":"claude-sonnet-4-5"},"fullConversationHeadersOnly":[{"bubbleId":"b1","type":1},{"bubbleId":"b2","type":2}]}"#;
        let b1_json = br#"{"_v":3,"bubbleId":"b1","type":1,"createdAt":1793455201000,"text":"Prompt inside BLOB value"}"#;
        let b2_json = br#"{"_v":3,"bubbleId":"b2","type":2,"createdAt":1793455205000,"text":"Reply inside BLOB value","tokenCount":{"inputTokens":3200,"outputTokens":140}}"#;

        writer
            .execute(
                "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                params!["composerData:comp-wal-1", comp_json.as_slice()],
            )
            .expect("insert composer BLOB");
        writer
            .execute(
                "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                params!["bubbleId:comp-wal-1:b1", b1_json.as_slice()],
            )
            .expect("insert b1 BLOB");
        writer
            .execute(
                "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                params!["bubbleId:comp-wal-1:b2", b2_json.as_slice()],
            )
            .expect("insert b2 BLOB");
        writer
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .expect("checkpoint WAL before snapshot");
    }

    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer_db_path = db_path.clone();

    let writer_handle = thread::spawn(move || {
        let conn = Connection::open(&writer_db_path).expect("open concurrent writer");
        conn.execute_batch("BEGIN IMMEDIATE;")
            .expect("acquire WAL write lock");
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            params![
                "bubbleId:comp-uncommitted:u1",
                br#"{"type":2,"text":"uncommitted","tokenCount":{"inputTokens":99,"outputTokens":11}}"#
                    .as_slice()
            ],
        )
        .expect("stage uncommitted WAL row");
        locked_tx.send(()).expect("signal write lock held");
        release_rx.recv().expect("wait for reader completion");
        conn.execute_batch("ROLLBACK;").expect("rollback writer");
    });

    locked_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("writer acquired BEGIN IMMEDIATE lock");
    thread::sleep(Duration::from_millis(20));

    let (sha_before, mtime_before) = sha256_and_mtime(&db_path);
    let parsed = parse_cursor_vscdb(&db_path).expect("read state.vscdb while WAL write lock held");
    let (sha_after, mtime_after) = sha256_and_mtime(&db_path);

    release_tx.send(()).expect("release writer");
    writer_handle.join().expect("join writer thread");

    assert_eq!(
        sha_before, sha_after,
        "state.vscdb SHA-256 bytes must remain identical during read-only WAL decode"
    );
    assert_eq!(
        mtime_before, mtime_after,
        "state.vscdb mtime must remain identical during read-only WAL decode"
    );
    assert_eq!(parsed.sessions.len(), 1);
    assert_eq!(parsed.sessions[0].session_id, "comp-wal-1");
    assert_eq!(parsed.sessions[0].records.len(), 1);
    assert_eq!(
        parsed.sessions[0].records[0].usage.confidence,
        Confidence::Estimated
    );
    assert_eq!(parsed.sessions[0].records[0].usage.prompt_tokens(), 3_200);
}

#[test]
fn cli_renders_zero_cursor_counts_as_not_recorded_never_zero_and_labels_estimated() {
    let env = TestEnv::new();
    let (_, gt_zero) = env.stage_fixture("cursor", "zero-tokens");
    let (_, gt_est) = env.stage_fixture("cursor", "estimated-tokens");
    let (_, gt_aider_hist) = env.stage_fixture("aider", "chat-history");
    let (_, gt_aider_ana) = env.stage_fixture("aider", "analytics-log");

    let zero_sid = &gt_zero.sessions[0].session_id;
    let est_sid = &gt_est.sessions[0].session_id;
    let zero_proj = format!("proj:{}", hash_cwd("/synthetic/workspace/cursor-zero"));

    let _ro_guard = env.make_logs_readonly();

    // 1. `kakeibo sessions --agent cursor`
    let sessions_cursor_out = env
        .kakeibo_cmd()
        .args(["sessions", "--agent", "cursor"])
        .output()
        .expect("run sessions --agent cursor");
    assert!(sessions_cursor_out.status.success());
    let sessions_cursor_stdout =
        String::from_utf8(sessions_cursor_out.stdout).expect("UTF-8 stdout");
    let zero_row = sessions_cursor_stdout
        .lines()
        .find(|l| l.contains(&zero_sid[..8]))
        .expect("zero-tokens cursor row in sessions table");
    assert!(
        zero_row.contains("not recorded"),
        "zero Cursor counts must render as `not recorded` in sessions table: {zero_row}"
    );
    assert!(
        !zero_row.contains("0/0/0"),
        "zero Cursor counts must never render as `0/0/0`: {zero_row}"
    );
    let est_row = sessions_cursor_stdout
        .lines()
        .find(|l| l.contains(&est_sid[..8]))
        .expect("estimated-tokens cursor row in sessions table");
    assert!(
        est_row.contains("(estimated)"),
        "recorded Cursor counts must be labelled `(estimated)` in sessions table: {est_row}"
    );

    // 2. `kakeibo sessions --agent aider`
    let sessions_aider_out = env
        .kakeibo_cmd()
        .args(["sessions", "--agent", "aider"])
        .output()
        .expect("run sessions --agent aider");
    assert!(sessions_aider_out.status.success());
    let sessions_aider_stdout = String::from_utf8(sessions_aider_out.stdout).expect("UTF-8 stdout");
    for exp_s in gt_aider_hist
        .sessions
        .iter()
        .chain(gt_aider_ana.sessions.iter())
    {
        let row = sessions_aider_stdout
            .lines()
            .find(|l| l.contains(&exp_s.session_id[..8]))
            .unwrap_or_else(|| panic!("missing Aider session {} in table", exp_s.session_id));
        assert!(
            row.contains("(estimated)"),
            "every Aider session row must be labelled `(estimated)`: {row}"
        );
        assert!(
            row.contains("(agent-reported)"),
            "Aider session with positive message cost must show `(agent-reported)`: {row}"
        );
    }

    // 3. `kakeibo autopsy <zero_sid> --turns`
    let autopsy_zero_out = env
        .kakeibo_cmd()
        .args(["autopsy", zero_sid, "--turns"])
        .output()
        .expect("run autopsy on zero-tokens cursor session");
    assert!(autopsy_zero_out.status.success());
    let autopsy_zero_stdout = String::from_utf8(autopsy_zero_out.stdout).expect("UTF-8 stdout");
    assert!(autopsy_zero_stdout.contains("Totals:         not recorded"));
    assert!(autopsy_zero_stdout.contains("Fixed overhead: not recorded"));
    assert!(autopsy_zero_stdout.contains("Categories (not recorded):"));
    assert!(autopsy_zero_stdout.contains("Turns (not recorded):"));
    assert!(
        !autopsy_zero_stdout.contains("0 prompt"),
        "autopsy must never render `0 prompt` for unrecorded Cursor counts:\n{autopsy_zero_stdout}"
    );
    for line in autopsy_zero_stdout.lines() {
        if line.starts_with("fixed_overhead")
            || line.starts_with("user_input")
            || line.starts_with("assistant_output")
            || line.trim_start().starts_with("1 ")
            || line.trim_start().starts_with("2 ")
        {
            assert!(
                line.contains("not recorded"),
                "category/turn line must render `not recorded`: {line}"
            );
            assert!(
                !line.split_whitespace().any(|tok| tok == "0"),
                "category/turn line must never render `0` for unrecorded tokens: {line}"
            );
        }
    }

    // 4. `kakeibo autopsy <est_sid> --turns`
    let autopsy_est_out = env
        .kakeibo_cmd()
        .args(["autopsy", est_sid, "--turns"])
        .output()
        .expect("run autopsy on estimated-tokens cursor session");
    assert!(autopsy_est_out.status.success());
    let autopsy_est_stdout = String::from_utf8(autopsy_est_out.stdout).expect("UTF-8 stdout");
    assert!(autopsy_est_stdout.contains("tokens (estimated)"));
    assert!(autopsy_est_stdout.contains("Turns (estimated):"));
    assert!(autopsy_est_stdout.contains("read_file"));

    // 5. `kakeibo top --by project`
    let top_proj_out = env
        .kakeibo_cmd()
        .args(["top", "--by", "project"])
        .output()
        .expect("run top --by project");
    assert!(top_proj_out.status.success());
    let top_proj_stdout = String::from_utf8(top_proj_out.stdout).expect("UTF-8 stdout");
    let zero_proj_row = top_proj_stdout
        .lines()
        .find(|l| l.contains(&zero_proj))
        .expect("zero-tokens cursor project row in top --by project");
    assert!(
        zero_proj_row.contains("not recorded"),
        "project with only unrecorded Cursor sessions must render `not recorded`: {zero_proj_row}"
    );

    // 6. `kakeibo export <zero_sid> --format md` and `--format json --show-paths`
    let md_zero_out = env
        .kakeibo_cmd()
        .args(["export", zero_sid, "--format", "md"])
        .output()
        .expect("run export --format md on zero-tokens cursor session");
    assert!(md_zero_out.status.success());
    let md_zero_stdout = String::from_utf8(md_zero_out.stdout).expect("UTF-8 md");
    assert!(md_zero_stdout.contains("- **Totals:** not recorded"));
    assert!(md_zero_stdout.contains("- **Fixed Overhead:** not recorded"));
    assert!(
        !md_zero_stdout.contains("| 0 |"),
        "Markdown export must never render `| 0 |` for unrecorded Cursor counts:\n{md_zero_stdout}"
    );
    assert!(
        !md_zero_stdout.contains("/synthetic/workspace/cursor-zero"),
        "default Markdown export must stay pathless without --show-paths"
    );

    let json_zero_out = env
        .kakeibo_cmd()
        .args(["export", zero_sid, "--format", "json", "--show-paths"])
        .output()
        .expect("run export --format json --show-paths on zero-tokens cursor session");
    assert!(json_zero_out.status.success());
    let json_doc: Value = serde_json::from_slice(&json_zero_out.stdout).expect("valid JSON export");
    assert_eq!(json_doc["schemaVersion"], 1);
    assert_eq!(
        json_doc["sessions"][0]["project_path"],
        "/synthetic/workspace/cursor-zero"
    );
    assert_eq!(
        json_doc["sessions"][0]["totals"]["confidence"],
        "not_recorded"
    );
    assert_eq!(
        json_doc["sessions"][0]["turns"][0]["measured_confidence"],
        "not_recorded"
    );

    // 7. `kakeibo doctor` lists all 6 v1 agent sources and leaks zero host/TestEnv paths
    let doctor_out = env
        .kakeibo_cmd()
        .arg("doctor")
        .output()
        .expect("run doctor");
    assert!(doctor_out.status.success());
    let doctor_stdout = String::from_utf8(doctor_out.stdout).expect("UTF-8 doctor stdout");
    for slug in [
        "claude-code",
        "codex",
        "gemini-cli",
        "opencode",
        "aider",
        "cursor",
    ] {
        assert!(
            doctor_stdout.contains(slug),
            "doctor must report source `{slug}`:\n{doctor_stdout}"
        );
    }
    assert!(
        !doctor_stdout.contains(env.home_dir().to_str().expect("home_dir str")),
        "doctor output must remain strictly pathless"
    );
}
