//! TDD integration, privacy, hostile-input, zstd, fork-history, and property tests
//! for the OpenAI Codex CLI discovery and streaming rollout parser (`PLAN.md` §5, §7, §9, §10 T05).

use std::fs;
use std::io::{BufReader, Cursor, Write};
use std::path::{Path, PathBuf};

use kakeibo_core::ledger::encode_ndjson;
use kakeibo_core::{AgentId, BlockKind, Confidence};
use kakeibo_parse::hash::hash_codex_tool_arguments;
use kakeibo_parse::{
    parse_codex_file, parse_codex_reader, ParseDiagnosticKind, PathCatalog, DEFAULT_MAX_LINE_BYTES,
};
use proptest::prelude::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must resolve")
}

fn sha256_file(path: &Path) -> [u8; 32] {
    let bytes = fs::read(path).expect("read file for sha256");
    Sha256::digest(&bytes).into()
}

struct ReadOnlyTreeGuard {
    root: PathBuf,
}

impl ReadOnlyTreeGuard {
    fn lock(root: &Path) -> Self {
        set_tree_mode_postorder(root, true);
        Self {
            root: root.to_path_buf(),
        }
    }
}

impl Drop for ReadOnlyTreeGuard {
    fn drop(&mut self) {
        set_tree_mode_preorder(&self.root, false);
    }
}

fn set_tree_mode_postorder(path: &Path, readonly: bool) {
    if path.is_dir() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                set_tree_mode_postorder(&entry.path(), readonly);
            }
        }
        apply_permissions(path, true, readonly);
    } else {
        apply_permissions(path, false, readonly);
    }
}

fn set_tree_mode_preorder(path: &Path, readonly: bool) {
    if path.is_dir() {
        apply_permissions(path, true, readonly);
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                set_tree_mode_preorder(&entry.path(), readonly);
            }
        }
    } else {
        apply_permissions(path, false, readonly);
    }
}

#[cfg(unix)]
fn apply_permissions(path: &Path, is_dir: bool, readonly: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = match (is_dir, readonly) {
        (true, true) => 0o555,
        (true, false) => 0o755,
        (false, true) => 0o444,
        (false, false) => 0o644,
    };
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn apply_permissions(path: &Path, _is_dir: bool, readonly: bool) {
    if let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_readonly(readonly);
        let _ = fs::set_permissions(path, perms);
    }
}

#[derive(Debug, Deserialize)]
struct ExpectedGroundTruth {
    agent: String,
    log_files: Vec<String>,
    sessions: Vec<ExpectedSession>,
}

#[derive(Debug, Deserialize)]
struct ExpectedSession {
    session_id: String,
    model: String,
    ignored_records: u64,
    turns: Vec<ExpectedTurn>,
    totals: ExpectedTotals,
}

#[derive(Debug, Deserialize)]
struct ExpectedTurn {
    turn: u32,
    ts: String,
    model: String,
    prompt_tokens: u64,
    rate_limit_used_percent: Option<f64>,
    usage: ExpectedUsage,
    added_blocks: Vec<ExpectedBlock>,
    hints: ExpectedHints,
}

#[derive(Debug, Deserialize)]
struct ExpectedUsage {
    fresh_in: u64,
    cache_write: u64,
    cache_read: u64,
    output: u64,
    reasoning: Option<u64>,
    confidence: Confidence,
}

#[derive(Debug, Deserialize)]
struct ExpectedBlock {
    kind: BlockKind,
    bytes: u64,
    tool: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExpectedHints {
    resume_start: bool,
    compaction: bool,
    sidechain: bool,
    api_error: bool,
}

#[derive(Debug, Deserialize)]
struct ExpectedTotals {
    turns: usize,
    prompt_tokens: u64,
    fresh_in: u64,
    cache_write: u64,
    cache_read: u64,
    output: u64,
    reasoning: u64,
}

#[test]
fn codex_basic_and_dual_usage_match_ground_truth_exactly() {
    let root = workspace_root();

    for scenario in ["basic", "codex-dual-usage"] {
        let fixture_dir = root.join("tests/fixtures/codex").join(scenario);
        let expected_raw =
            fs::read_to_string(fixture_dir.join("expected.json")).expect("read expected.json");
        let expected: ExpectedGroundTruth =
            serde_json::from_str(&expected_raw).expect("parse expected.json");
        assert_eq!(expected.agent, "codex");
        assert_eq!(expected.log_files.len(), 1);
        assert_eq!(expected.sessions.len(), 1);

        let temp = TempDir::new().expect("tempdir");
        let codex_home = temp.path().join("custom-codex-home");
        let rel_log = &expected.log_files[0];
        let staged_path = codex_home.join(rel_log);
        fs::create_dir_all(staged_path.parent().unwrap()).unwrap();
        let src_path = fixture_dir.join(rel_log);
        fs::copy(&src_path, &staged_path).unwrap();

        let hash_before = sha256_file(&staged_path);
        let _ro = ReadOnlyTreeGuard::lock(&codex_home);

        let catalog = PathCatalog::new(None, None, Some(codex_home.clone()));
        let discovered = catalog
            .discover_codex_rollouts()
            .expect("discover codex rollouts");
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].agent, AgentId::Codex);
        assert_eq!(discovered[0].path, staged_path);

        let parsed = parse_codex_file(&discovered[0].path)
            .unwrap_or_else(|e| panic!("failed to parse codex/{scenario}: {e}"));
        let hash_after = sha256_file(&staged_path);
        assert_eq!(
            hash_before, hash_after,
            "parser must never mutate rollout files"
        );
        assert!(
            parsed.diagnostics.issues.is_empty(),
            "unexpected diagnostics on clean fixture {scenario}: {:?}",
            parsed.diagnostics.issues
        );
        assert!(
            parsed.diagnostics.cli_versions.contains("0.165.0"),
            "expected cli_version 0.165.0 in diagnostics"
        );

        let exp_session = &expected.sessions[0];
        assert_eq!(
            parsed.diagnostics.ignored_records, exp_session.ignored_records,
            "ignored_records mismatch in codex/{scenario}"
        );
        assert_eq!(
            parsed.records.len(),
            exp_session.turns.len(),
            "turn count mismatch in codex/{scenario} (must never double-count)"
        );

        let mut tot_prompt = 0_u64;
        let mut tot_fresh = 0_u64;
        let mut tot_write = 0_u64;
        let mut tot_read = 0_u64;
        let mut tot_out = 0_u64;
        let mut tot_reasoning = 0_u64;

        for (actual, exp) in parsed.records.iter().zip(exp_session.turns.iter()) {
            assert_eq!(actual.agent, AgentId::Codex);
            assert_eq!(actual.session_id, exp_session.session_id);
            assert_eq!(actual.parent_session_id, None);
            assert_eq!(actual.turn, exp.turn);
            assert_eq!(actual.ts.to_string(), exp.ts);
            assert_eq!(actual.model.as_deref(), Some(exp.model.as_str()));
            assert_eq!(actual.model.as_deref(), Some(exp_session.model.as_str()));
            assert!(
                actual
                    .cwd_hash
                    .as_deref()
                    .is_some_and(|h| h.len() == 16 && h.chars().all(|c| c.is_ascii_hexdigit())),
                "cwd_hash must be a 16-char lowercase hex SHA-256 prefix"
            );

            assert_eq!(actual.usage.fresh_in, exp.usage.fresh_in);
            assert_eq!(actual.usage.cache_write, exp.usage.cache_write);
            assert_eq!(actual.usage.cache_read, exp.usage.cache_read);
            assert_eq!(actual.usage.output, exp.usage.output);
            assert_eq!(actual.usage.reasoning, exp.usage.reasoning);
            assert_eq!(actual.usage.confidence, exp.usage.confidence);
            assert_eq!(
                actual.usage.prompt_tokens(),
                exp.prompt_tokens,
                "cached tokens must never be added on top of inclusive input_tokens"
            );

            assert_eq!(actual.hints.resume_start, exp.hints.resume_start);
            assert_eq!(actual.hints.compaction, exp.hints.compaction);
            assert_eq!(actual.hints.sidechain, exp.hints.sidechain);
            assert_eq!(actual.hints.api_error, exp.hints.api_error);
            assert_eq!(
                actual.hints.rate_limit_used_percent,
                exp.rate_limit_used_percent
            );
            assert_eq!(actual.hints.context_window_override, Some(272_000));

            assert_eq!(
                actual.added.len(),
                exp.added_blocks.len(),
                "added block count mismatch on {scenario} turn {}",
                exp.turn
            );
            for (blk, exp_blk) in actual.added.iter().zip(exp.added_blocks.iter()) {
                assert_eq!(blk.kind, exp_blk.kind);
                assert_eq!(blk.bytes, exp_blk.bytes);
                assert_eq!(blk.tool, exp_blk.tool);
                assert_eq!(blk.est_tokens, exp_blk.bytes.div_ceil(4));
                if matches!(blk.kind, BlockKind::ToolUse | BlockKind::ToolResult) {
                    assert!(
                        blk.key_hash.as_deref().is_some_and(
                            |h| h.len() == 16 && h.chars().all(|c| c.is_ascii_hexdigit())
                        ),
                        "tool block must carry 16-char hex key_hash"
                    );
                }
            }

            tot_prompt += actual.usage.prompt_tokens();
            tot_fresh += actual.usage.fresh_in;
            tot_write += actual.usage.cache_write;
            tot_read += actual.usage.cache_read;
            tot_out += actual.usage.output;
            tot_reasoning += actual.usage.reasoning.unwrap_or(0);
        }

        assert_eq!(parsed.records.len(), exp_session.totals.turns);
        assert_eq!(tot_prompt, exp_session.totals.prompt_tokens);
        assert_eq!(tot_fresh, exp_session.totals.fresh_in);
        assert_eq!(tot_write, exp_session.totals.cache_write);
        assert_eq!(tot_read, exp_session.totals.cache_read);
        assert_eq!(tot_out, exp_session.totals.output);
        assert_eq!(tot_reasoning, exp_session.totals.reasoning);
    }
}

#[test]
fn cumulative_only_rollout_computes_exact_deltas_and_skips_duplicate_snapshots() {
    let jsonl = [
        r#"{"ordinal":0,"timestamp":"2026-10-31T11:00:00Z","type":"session_meta","payload":{"id":"sess-cum-1","session_id":"sess-cum-1","cwd":"/synthetic/repo","cli_version":"0.140.0","source":"cli"}}"#,
        r#"{"ordinal":1,"timestamp":"2026-10-31T11:00:01Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5-codex","cwd":"/synthetic/repo"}}"#,
        r#"{"ordinal":2,"timestamp":"2026-10-31T11:00:01Z","type":"response_item","payload":{"id":"m1","type":"message","role":"user","content":[{"type":"input_text","text":"first request"}]}}"#,
        // Turn 1: cumulative-only (`last_token_usage` omitted) -> P=5400 (fresh=1200, read=4000, write=200)
        r#"{"ordinal":3,"timestamp":"2026-10-31T11:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5400,"cached_input_tokens":4000,"cache_write_input_tokens":200,"output_tokens":110,"reasoning_output_tokens":40,"total_tokens":5510}},"rate_limits":{"primary":{"used_percent":4.0,"window_minutes":300,"resets_at":1793448000}}}}"#,
        // Duplicate consecutive `tc -> tc` snapshot with identical cumulative totals -> must NOT emit Turn 2, only refresh rate_limits!
        r#"{"ordinal":4,"timestamp":"2026-10-31T11:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5400,"cached_input_tokens":4000,"cache_write_input_tokens":200,"output_tokens":110,"reasoning_output_tokens":40,"total_tokens":5510}},"rate_limits":{"primary":{"used_percent":4.5,"window_minutes":300,"resets_at":1793448000}}}}"#,
        // Turn 2: cumulative input=11100 (+5700), cached=9400 (+5400), write=200 (+0) => delta fresh=300, read=5400, write=0, out=90, reasoning=30
        r#"{"ordinal":5,"timestamp":"2026-10-31T11:00:05Z","type":"response_item","payload":{"id":"m2","type":"message","role":"user","content":[{"type":"input_text","text":"second request"}]}}"#,
        r#"{"ordinal":6,"timestamp":"2026-10-31T11:00:07Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":null,"total_token_usage":{"input_tokens":11100,"cached_input_tokens":9400,"cache_write_input_tokens":200,"output_tokens":200,"reasoning_output_tokens":70,"total_tokens":11300}},"rate_limits":{"primary":{"used_percent":5.25,"window_minutes":300,"resets_at":1793448000}}}}"#,
        // Turn 3: full cumulative counter reset (process restart appending to rollout) -> input=2000, cached=500, out=60, reasoning=20
        r#"{"ordinal":7,"timestamp":"2026-10-31T11:05:00Z","type":"response_item","payload":{"id":"m3","type":"message","role":"user","content":[{"type":"input_text","text":"after restart"}]}}"#,
        r#"{"ordinal":8,"timestamp":"2026-10-31T11:05:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2000,"cached_input_tokens":500,"output_tokens":60,"reasoning_output_tokens":20,"total_tokens":2060}}}}"#,
    ]
    .join("\n");

    let parsed =
        parse_codex_reader(BufReader::new(Cursor::new(jsonl.as_bytes()))).expect("parse reader");
    assert!(parsed.diagnostics.issues.is_empty());
    assert_eq!(parsed.records.len(), 3);

    let t1 = &parsed.records[0];
    assert_eq!(t1.turn, 1);
    assert_eq!(t1.usage.fresh_in, 1200);
    assert_eq!(t1.usage.cache_read, 4000);
    assert_eq!(t1.usage.cache_write, 200);
    assert_eq!(t1.usage.prompt_tokens(), 5400);
    assert_eq!(t1.usage.output, 110);
    assert_eq!(t1.usage.reasoning, Some(40));
    assert_eq!(t1.hints.rate_limit_used_percent, Some(4.5));
    assert!(!t1.hints.resume_start);

    let t2 = &parsed.records[1];
    assert_eq!(t2.turn, 2);
    assert_eq!(t2.usage.fresh_in, 300);
    assert_eq!(t2.usage.cache_read, 5400);
    assert_eq!(t2.usage.cache_write, 0);
    assert_eq!(t2.usage.prompt_tokens(), 5700);
    assert_eq!(t2.usage.output, 90);
    assert_eq!(t2.usage.reasoning, Some(30));
    assert_eq!(t2.hints.rate_limit_used_percent, Some(5.25));
    assert!(!t2.hints.resume_start);

    let t3 = &parsed.records[2];
    assert_eq!(t3.turn, 3);
    assert_eq!(t3.usage.fresh_in, 1500);
    assert_eq!(t3.usage.cache_read, 500);
    assert_eq!(t3.usage.cache_write, 0);
    assert_eq!(t3.usage.prompt_tokens(), 2000);
    assert_eq!(t3.usage.output, 60);
    assert_eq!(t3.usage.reasoning, Some(20));
    assert!(t3.hints.resume_start);
}

#[test]
fn reverse_tc_then_tur_and_duplicate_response_id_never_double_count() {
    let jsonl = [
        r#"{"ordinal":0,"timestamp":"2026-10-31T11:10:00Z","type":"session_meta","payload":{"session_id":"sess-rev","cwd":"/synthetic/rev","cli_version":"0.165.0","source":"cli"}}"#,
        r#"{"ordinal":1,"timestamp":"2026-10-31T11:10:01Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5-codex","cwd":"/synthetic/rev"}}"#,
        r#"{"ordinal":2,"timestamp":"2026-10-31T11:10:01Z","type":"response_item","payload":{"id":"u1","type":"message","role":"user","content":[{"type":"input_text","text":"prompt one"}]}}"#,
        // Turn 1: `event_msg/token_count` arrives BEFORE `token_usage_record` (`tc -> tur`)
        r#"{"ordinal":3,"timestamp":"2026-10-31T11:10:02Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":272000,"last_token_usage":{"input_tokens":5000,"cached_input_tokens":3000,"output_tokens":80,"reasoning_output_tokens":20,"total_tokens":5080},"total_token_usage":{"input_tokens":5000,"cached_input_tokens":3000,"output_tokens":80,"reasoning_output_tokens":20,"total_tokens":5080}},"rate_limits":{"primary":{"used_percent":8.0,"window_minutes":300,"resets_at":1793448000}}}}"#,
        r#"{"ordinal":4,"timestamp":"2026-10-31T11:10:03Z","type":"token_usage_record","payload":{"response_id":"resp_001","session_id":"sess-rev","turn_id":"t1","usage":{"input_tokens":5010,"cached_input_tokens":3000,"cache_write_input_tokens":0,"output_tokens":85,"reasoning_output_tokens":25,"total_tokens":5095}}}"#,
        // Duplicate `token_usage_record` with same `response_id` -> must be ignored!
        r#"{"ordinal":5,"timestamp":"2026-10-31T11:10:03Z","type":"token_usage_record","payload":{"response_id":"resp_001","session_id":"sess-rev","turn_id":"t1","usage":{"input_tokens":5010,"cached_input_tokens":3000,"cache_write_input_tokens":0,"output_tokens":85,"reasoning_output_tokens":25,"total_tokens":5095}}}"#,
        // Rate-limit-only `token_count` (`info: null`) before Turn 2
        r#"{"ordinal":6,"timestamp":"2026-10-31T11:10:04Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"primary":{"used_percent":8.25,"window_minutes":300,"resets_at":1793448000}}}}"#,
        // Turn 2: `tc -> tur` again after `saw_any_tur` is already true
        r#"{"ordinal":7,"timestamp":"2026-10-31T11:10:05Z","type":"response_item","payload":{"id":"u2","type":"message","role":"user","content":[{"type":"input_text","text":"prompt two"}]}}"#,
        r#"{"ordinal":8,"timestamp":"2026-10-31T11:10:06Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":272000,"last_token_usage":{"input_tokens":5600,"cached_input_tokens":5010,"output_tokens":95,"reasoning_output_tokens":30,"total_tokens":5695},"total_token_usage":{"input_tokens":10610,"cached_input_tokens":8010,"output_tokens":180,"reasoning_output_tokens":55,"total_tokens":10790}},"rate_limits":{"primary":{"used_percent":9.5,"window_minutes":300,"resets_at":1793448000}}}}"#,
        r#"{"ordinal":9,"timestamp":"2026-10-31T11:10:07Z","type":"token_usage_record","payload":{"response_id":"resp_002","session_id":"sess-rev","turn_id":"t1","usage":{"input_tokens":5600,"cached_input_tokens":5010,"cache_write_input_tokens":0,"output_tokens":95,"reasoning_output_tokens":30,"total_tokens":5695}}}"#,
    ]
    .join("\n");

    let parsed =
        parse_codex_reader(BufReader::new(Cursor::new(jsonl.as_bytes()))).expect("parse reader");
    assert_eq!(parsed.records.len(), 2, "must emit exact 2 turns");

    // Turn 1 took authoritative `token_usage_record` counts (5010 / 85 / 25) AND kept `tc` rate_limits (8.0) & window (272000).
    assert_eq!(parsed.records[0].turn, 1);
    assert_eq!(parsed.records[0].usage.prompt_tokens(), 5010);
    assert_eq!(parsed.records[0].usage.fresh_in, 2010);
    assert_eq!(parsed.records[0].usage.cache_read, 3000);
    assert_eq!(parsed.records[0].usage.output, 85);
    assert_eq!(parsed.records[0].usage.reasoning, Some(25));
    assert_eq!(parsed.records[0].hints.rate_limit_used_percent, Some(8.0));
    assert_eq!(
        parsed.records[0].hints.context_window_override,
        Some(272_000)
    );
    assert_eq!(parsed.records[0].added.len(), 1);

    assert_eq!(parsed.records[1].turn, 2);
    assert_eq!(parsed.records[1].usage.prompt_tokens(), 5600);
    assert_eq!(parsed.records[1].usage.fresh_in, 590);
    assert_eq!(parsed.records[1].usage.cache_read, 5010);
    assert_eq!(parsed.records[1].usage.output, 95);
    assert_eq!(parsed.records[1].usage.reasoning, Some(30));
    assert_eq!(parsed.records[1].hints.rate_limit_used_percent, Some(9.5));
    assert_eq!(
        parsed.records[1].hints.context_window_override,
        Some(272_000)
    );
    assert_eq!(parsed.records[1].added.len(), 1);
}

#[test]
fn discovery_supports_codex_home_archived_sessions_and_zstd_rollouts() {
    let root = workspace_root();
    let dual_fixture = root.join(
        "tests/fixtures/codex/codex-dual-usage/sessions/2026/10/31/rollout-2026-10-31T10-00-00-synth-codex-0002.jsonl",
    );
    let raw_jsonl = fs::read(&dual_fixture).expect("read dual-usage fixture");
    let compressed_zst = zstd::encode_all(Cursor::new(&raw_jsonl), 3).expect("zstd compress");

    let temp = TempDir::new().expect("tempdir");
    let fake_home = temp.path().join("home");
    let codex_home = temp.path().join("override-codex-home");

    // Put a decoy rollout under `HOME/.codex` to verify `CODEX_HOME` takes precedence.
    let decoy = fake_home.join(".codex/sessions/2026/10/31/rollout-decoy.jsonl");
    fs::create_dir_all(decoy.parent().unwrap()).unwrap();
    fs::write(&decoy, &raw_jsonl).unwrap();

    // Active `.jsonl`, active `.jsonl.zst`, and archived `.jsonl.zst` under `CODEX_HOME`.
    let active_plain = codex_home.join("sessions/2026/10/30/rollout-01.jsonl");
    let active_zst = codex_home.join("sessions/2026/10/31/rollout-02.jsonl.zst");
    let archived_zst = codex_home.join("archived_sessions/rollout-00-archived.jsonl.zst");
    let non_rollout = codex_home.join("sessions/2026/10/31/notes.txt");

    for p in [&active_plain, &active_zst, &archived_zst, &non_rollout] {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
    }
    fs::write(&active_plain, &raw_jsonl).unwrap();
    fs::write(&active_zst, &compressed_zst).unwrap();
    fs::write(&archived_zst, &compressed_zst).unwrap();
    fs::write(&non_rollout, b"ignored").unwrap();

    let _ro = ReadOnlyTreeGuard::lock(&codex_home);

    let catalog = PathCatalog::new(Some(fake_home), None, Some(codex_home));
    let discovered = catalog
        .discover_codex_rollouts()
        .expect("discover codex rollouts");
    assert_eq!(discovered.len(), 3);
    assert_eq!(
        discovered
            .iter()
            .map(|d| d.path.clone())
            .collect::<Vec<_>>(),
        vec![
            archived_zst.clone(),
            active_plain.clone(),
            active_zst.clone()
        ]
    );

    let parsed_plain = parse_codex_file(&active_plain).expect("parse plain");
    let parsed_zst = parse_codex_file(&active_zst).expect("parse zst");
    let parsed_archived = parse_codex_file(&archived_zst).expect("parse archived zst");

    assert_eq!(parsed_plain.records, parsed_zst.records);
    assert_eq!(parsed_plain.records, parsed_archived.records);
    assert_eq!(parsed_zst.diagnostics, parsed_plain.diagnostics);
}

#[test]
fn fork_and_subagent_rollouts_exclude_replayed_parent_history_and_set_hints() {
    // 1. Forked rollout (`forked_from_id` set): parent items appear before first `turn_context`.
    let fork_jsonl = [
        r#"{"ordinal":0,"timestamp":"2026-10-31T11:20:00Z","type":"session_meta","payload":{"session_id":"sess-fork-child","forked_from_id":"sess-fork-parent","parent_thread_id":"sess-fork-parent","cwd":"/synthetic/fork","cli_version":"0.165.0","source":"cli"}}"#,
        // Replayed parent history BEFORE first `turn_context` (must NOT appear in child Turn 1 `added`,
        // EXCEPT `call_parent_1` must still register in `tool_registry` so cross-boundary outputs resolve!).
        r#"{"ordinal":1,"timestamp":"2026-10-31T10:00:01Z","type":"response_item","payload":{"id":"pm1","type":"message","role":"user","content":[{"type":"input_text","text":"old parent user prompt that is 48 bytes long...."}]}}"#,
        r#"{"ordinal":2,"timestamp":"2026-10-31T10:00:02Z","type":"response_item","payload":{"id":"pfc1","type":"function_call","call_id":"call_parent_1","name":"exec_command","arguments":"{\"cmd\":[\"ls\"]}"}}"#,
        r#"{"ordinal":3,"timestamp":"2026-10-31T10:00:03Z","type":"response_item","payload":{"id":"pm2","type":"message","role":"assistant","content":[{"type":"output_text","text":"old parent assistant reply"}]}}"#,
        // Child's own first `turn_context` marks end of replayed parent history!
        r#"{"ordinal":4,"timestamp":"2026-10-31T11:20:01Z","type":"turn_context","payload":{"turn_id":"ct1","model":"gpt-5-codex","cwd":"/synthetic/fork"}}"#,
        r#"{"ordinal":5,"timestamp":"2026-10-31T11:20:01Z","type":"response_item","payload":{"id":"cfco1","type":"function_call_output","call_id":"call_parent_1","output":"resolved output"}}"#,
        r#"{"ordinal":6,"timestamp":"2026-10-31T11:20:02Z","type":"response_item","payload":{"id":"cm1","type":"message","role":"user","content":[{"type":"input_text","text":"child prompt"}]}}"#,
        r#"{"ordinal":7,"timestamp":"2026-10-31T11:20:04Z","type":"response_item","payload":{"id":"ca1","type":"message","role":"assistant","content":[{"type":"output_text","text":"child reply"}]}}"#,
        r#"{"ordinal":8,"timestamp":"2026-10-31T11:20:04Z","type":"token_usage_record","payload":{"response_id":"resp_f1","session_id":"sess-fork-child","turn_id":"ct1","usage":{"input_tokens":18000,"cached_input_tokens":2000,"cache_write_input_tokens":15000,"output_tokens":90,"reasoning_output_tokens":30,"total_tokens":18090}}}"#,
        // Compaction + transient stream_error before Turn 2
        r#"{"ordinal":9,"timestamp":"2026-10-31T11:20:05Z","type":"compacted","payload":{"window_number":2,"replacement_history":[]}}"#,
        r#"{"ordinal":10,"timestamp":"2026-10-31T11:20:06Z","type":"event_msg","payload":{"type":"stream_error"}}"#,
        r#"{"ordinal":11,"timestamp":"2026-10-31T11:20:07Z","type":"response_item","payload":{"id":"cm2","type":"message","role":"user","content":[{"type":"input_text","text":"post compact"}]}}"#,
        r#"{"ordinal":12,"timestamp":"2026-10-31T11:20:09Z","type":"token_usage_record","payload":{"response_id":"resp_f2","session_id":"sess-fork-child","turn_id":"ct1","usage":{"input_tokens":4000,"cached_input_tokens":3500,"cache_write_input_tokens":0,"output_tokens":70,"reasoning_output_tokens":20,"total_tokens":4070}}}"#,
    ]
    .join("\n");

    let parsed_fork =
        parse_codex_reader(BufReader::new(Cursor::new(fork_jsonl.as_bytes()))).expect("parse fork");
    assert_eq!(parsed_fork.records.len(), 2);

    let f_t1 = &parsed_fork.records[0];
    assert_eq!(f_t1.session_id, "sess-fork-child");
    assert_eq!(f_t1.parent_session_id.as_deref(), Some("sess-fork-parent"));
    assert!(
        f_t1.hints.resume_start,
        "forked Turn 1 must mark resume_start"
    );
    assert!(!f_t1.hints.sidechain);
    assert!(!f_t1.hints.compaction);
    assert_eq!(
        f_t1.added.len(),
        3,
        "only post-turn_context child blocks may appear in Turn 1 added: {:?}",
        f_t1.added
    );
    assert_eq!(f_t1.added[0].kind, BlockKind::ToolResult);
    assert_eq!(f_t1.added[0].tool.as_deref(), Some("exec_command"));
    assert_eq!(f_t1.added[1].kind, BlockKind::UserPrompt);
    assert_eq!(f_t1.added[1].bytes, 12); // "child prompt"
    assert_eq!(f_t1.added[2].kind, BlockKind::AssistantText);
    assert_eq!(f_t1.added[2].bytes, 11); // "child reply"

    let f_t2 = &parsed_fork.records[1];
    assert!(!f_t2.hints.resume_start);
    assert!(f_t2.hints.compaction);
    assert!(f_t2.hints.api_error);

    // 2. Subagent rollout (`source: {"subagent": ...}`, `subagent_history_start_ordinal: 3`).
    let subagent_jsonl = [
        r#"{"ordinal":0,"timestamp":"2026-10-31T11:25:00Z","type":"session_meta","payload":{"session_id":"sess-sub-1","parent_thread_id":"sess-main-1","subagent_history_start_ordinal":3,"cwd":"/synthetic/sub","cli_version":"0.165.0","source":{"subagent":{"thread_spawn":{"depth":1}}}}}"#,
        r#"{"ordinal":1,"timestamp":"2026-10-31T11:25:00Z","type":"turn_context","payload":{"turn_id":"st1","model":"gpt-5-codex","cwd":"/synthetic/sub"}}"#,
        // Ordinal 2 < subagent_history_start_ordinal (3) -> inherited parent item, must be skipped from `added`!
        r#"{"ordinal":2,"timestamp":"2026-10-31T11:24:00Z","type":"response_item","payload":{"id":"inh1","type":"message","role":"user","content":[{"type":"input_text","text":"inherited parent context"}]}}"#,
        // Ordinal 3 >= subagent_history_start_ordinal (3) -> child subagent's own task prompt!
        r#"{"ordinal":3,"timestamp":"2026-10-31T11:25:01Z","type":"response_item","payload":{"id":"sub_u1","type":"message","role":"user","content":[{"type":"input_text","text":"subagent task"}]}}"#,
        r#"{"ordinal":4,"timestamp":"2026-10-31T11:25:03Z","type":"token_usage_record","payload":{"response_id":"resp_s1","session_id":"sess-sub-1","turn_id":"st1","usage":{"input_tokens":3000,"cached_input_tokens":2400,"cache_write_input_tokens":0,"output_tokens":50,"reasoning_output_tokens":10,"total_tokens":3050}}}"#,
    ]
    .join("\n");

    let parsed_sub = parse_codex_reader(BufReader::new(Cursor::new(subagent_jsonl.as_bytes())))
        .expect("parse subagent");
    assert_eq!(parsed_sub.records.len(), 1);
    let s_t1 = &parsed_sub.records[0];
    assert_eq!(s_t1.parent_session_id.as_deref(), Some("sess-main-1"));
    assert!(s_t1.hints.sidechain, "subagent rollout must mark sidechain");
    assert!(!s_t1.hints.resume_start);
    assert_eq!(s_t1.added.len(), 1);
    assert_eq!(s_t1.added[0].bytes, 13); // "subagent task"
}

#[test]
fn hostile_codex_inputs_and_corrupt_zstd_produce_diagnostics_without_panic() {
    let temp = TempDir::new().expect("tempdir");
    let hostile_jsonl = temp.path().join("rollout-hostile.jsonl");

    {
        let mut f = fs::File::create(&hostile_jsonl).unwrap();
        // Line 1: session_meta
        writeln!(
            f,
            r#"{{"ordinal":0,"timestamp":"2026-10-31T12:00:00Z","type":"session_meta","payload":{{"session_id":"sess-hostile-codex","cwd":"/synthetic/hostile","cli_version":"0.165.0","source":"cli"}}}}"#
        )
        .unwrap();
        // Line 2: turn_context
        writeln!(
            f,
            r#"{{"ordinal":1,"timestamp":"2026-10-31T12:00:01Z","type":"turn_context","payload":{{"turn_id":"t1","model":"gpt-5-codex","cwd":"/synthetic/hostile"}}}}"#
        )
        .unwrap();
        // Line 3: valid Turn 1 via token_count where `cached_input_tokens (9000) > input_tokens (5000)` -> clamped cleanly!
        writeln!(
            f,
            r#"{{"ordinal":2,"timestamp":"2026-10-31T12:00:02Z","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":5000,"cached_input_tokens":9000,"output_tokens":40,"reasoning_output_tokens":10,"total_tokens":5040}}}}}}}}"#
        )
        .unwrap();
        // Line 4: unknown record type
        writeln!(
            f,
            r#"{{"ordinal":3,"timestamp":"2026-10-31T12:00:02Z","type":"future_codex_v99","payload":{{}}}}"#
        )
        .unwrap();
        // Line 5: invalid UTF-8 bytes
        f.write_all(b"{\"type\":\"response_item\",\"bad\":\"\xff\xfe\"}\n")
            .unwrap();
        // Line 6: 20 MiB single line (> 16 MiB cap)
        f.write_all(b"{\"type\":\"world_state\",\"huge\":\"")
            .unwrap();
        let chunk = vec![b'y'; 64 * 1024];
        for _ in 0..320 {
            f.write_all(&chunk).unwrap();
        }
        f.write_all(b"\"}\n").unwrap();
        // Line 7: malformed JSON
        f.write_all(b"{\"type\":\"token_usage_record\",\"payload\":}\n")
            .unwrap();
        // Line 8: valid Turn 2
        writeln!(
            f,
            r#"{{"ordinal":7,"timestamp":"2026-10-31T12:00:05Z","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":6000,"cached_input_tokens":5000,"output_tokens":55,"reasoning_output_tokens":15,"total_tokens":6055}}}}}}}}"#
        )
        .unwrap();
        // Line 9: truncated JSON at EOF
        f.write_all(b"{\"ordinal\":8,\"type\":\"event_msg\"")
            .unwrap();
    }

    // Corrupt `.jsonl.zst`: valid Zstandard frame containing 1 turn followed by corrupt trailing frame bytes.
    let corrupt_zst_path = temp.path().join("rollout-corrupt.jsonl.zst");
    {
        let valid_prefix = concat!(
            r#"{"ordinal":0,"timestamp":"2026-10-31T12:10:00Z","type":"session_meta","payload":{"session_id":"sess-zst-partial","cwd":"/synthetic/zst","cli_version":"0.165.0","source":"cli"}}"#,
            "\n",
            r#"{"ordinal":1,"timestamp":"2026-10-31T12:10:02Z","type":"token_usage_record","payload":{"response_id":"resp_z1","session_id":"sess-zst-partial","turn_id":"zt1","usage":{"input_tokens":1000,"cached_input_tokens":400,"cache_write_input_tokens":0,"output_tokens":30,"reasoning_output_tokens":0,"total_tokens":1030}}}"#,
            "\n"
        );
        let mut zst_bytes =
            zstd::encode_all(Cursor::new(valid_prefix.as_bytes()), 1).expect("encode valid frame");
        // Append a malformed Zstandard magic header + truncated garbage frame!
        zst_bytes.extend_from_slice(&[0x28, 0xB5, 0x2F, 0xFD, 0xFF, 0x00, 0x11]);
        fs::write(&corrupt_zst_path, zst_bytes).unwrap();
    }

    let _ro = ReadOnlyTreeGuard::lock(temp.path());

    let parsed = parse_codex_file(&hostile_jsonl).expect("hostile jsonl must not fail");
    assert_eq!(parsed.records.len(), 2);
    // Turn 1 had `cached_input_tokens (9000) > input_tokens (5000)` -> clamped to `fresh=0, read=5000, P=5000`.
    assert_eq!(parsed.records[0].usage.fresh_in, 0);
    assert_eq!(parsed.records[0].usage.cache_read, 5000);
    assert_eq!(parsed.records[0].usage.prompt_tokens(), 5000);
    assert_eq!(parsed.records[1].usage.fresh_in, 1000);
    assert_eq!(parsed.records[1].usage.cache_read, 5000);
    assert_eq!(parsed.records[1].usage.prompt_tokens(), 6000);
    assert_eq!(
        parsed
            .diagnostics
            .unknown_record_types
            .get("future_codex_v99"),
        Some(&1)
    );

    let issues = &parsed.diagnostics.issues;
    assert_eq!(
        issues.len(),
        4,
        "expected 4 line diagnostics, got {issues:?}"
    );
    assert_eq!(issues[0].line_number, 5);
    assert_eq!(issues[0].kind, ParseDiagnosticKind::InvalidUtf8);
    assert_eq!(issues[1].line_number, 6);
    match issues[1].kind {
        ParseDiagnosticKind::OversizedLine { byte_len, limit } => {
            assert_eq!(limit, DEFAULT_MAX_LINE_BYTES);
            assert!(byte_len >= 20 * 1024 * 1024);
        }
        ref other => panic!("expected OversizedLine, got {other:?}"),
    }
    assert_eq!(issues[2].line_number, 7);
    assert_eq!(issues[2].kind, ParseDiagnosticKind::MalformedJson);
    assert_eq!(issues[3].line_number, 9);
    assert_eq!(issues[3].kind, ParseDiagnosticKind::MalformedJson);

    let parsed_zst =
        parse_codex_file(&corrupt_zst_path).expect("corrupt zstd tail must degrade gracefully");
    assert_eq!(
        parsed_zst.records.len(),
        1,
        "turn decoded from valid first zstd frame must be preserved"
    );
    assert_eq!(parsed_zst.records[0].usage.prompt_tokens(), 1000);
    assert!(
        parsed_zst
            .diagnostics
            .issues
            .iter()
            .any(|d| d.kind == ParseDiagnosticKind::DecompressionError),
        "expected DecompressionError diagnostic on corrupt zstd tail, got {:?}",
        parsed_zst.diagnostics.issues
    );
}

#[test]
fn privacy_canaries_never_leak_into_codex_records_ledger_or_diagnostics() {
    let canaries = [
        "CANARY_CODEX_PROMPT_881",
        "CANARY_CODEX_PATCH_882",
        "CANARY_CODEX_ARGS_883",
        "CANARY_CODEX_OUTPUT_884",
        "CANARY_CODEX_ENCRYPTED_885",
        "/Users/canary_codex_owner/secret_repo",
        "CANARY_WORLD_STATE_886",
        "CANARY_COMPACTED_887",
    ];

    let jsonl = [
        r#"{"ordinal":0,"timestamp":"2026-10-31T12:30:00Z","type":"session_meta","payload":{"session_id":"sess-canary-codex","cwd":"/Users/canary_codex_owner/secret_repo","cli_version":"0.165.0","source":"cli"}}"#,
        r#"{"ordinal":1,"timestamp":"2026-10-31T12:30:00Z","type":"world_state","payload":{"full":true,"state":{"secret":"CANARY_WORLD_STATE_886"}}}"#,
        r#"{"ordinal":2,"timestamp":"2026-10-31T12:30:01Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5-codex","cwd":"/Users/canary_codex_owner/secret_repo"}}"#,
        r#"{"ordinal":3,"timestamp":"2026-10-31T12:30:01Z","type":"response_item","payload":{"id":"m1","type":"message","role":"user","content":[{"type":"input_text","text":"Please inspect CANARY_CODEX_PROMPT_881"}]}}"#,
        r#"{"ordinal":4,"timestamp":"2026-10-31T12:30:02Z","type":"response_item","payload":{"id":"r1","type":"reasoning","encrypted_content":"CANARY_CODEX_ENCRYPTED_885","summary":["CANARY_CODEX_ENCRYPTED_885"]}}"#,
        r#"{"ordinal":5,"timestamp":"2026-10-31T12:30:02Z","type":"response_item","payload":{"id":"fc1","type":"function_call","call_id":"c1","name":"exec_command","arguments":"{\"cmd\":[\"echo\",\"CANARY_CODEX_ARGS_883\"]}"}}"#,
        r#"{"ordinal":6,"timestamp":"2026-10-31T12:30:03Z","type":"response_item","payload":{"id":"ctc1","type":"custom_tool_call","call_id":"c2","name":"apply_patch","input":"*** Begin Patch\n+CANARY_CODEX_PATCH_882\n*** End Patch"}}"#,
        r#"{"ordinal":7,"timestamp":"2026-10-31T12:30:04Z","type":"response_item","payload":{"id":"fco1","type":"function_call_output","call_id":"c1","output":"CANARY_CODEX_OUTPUT_884"}}"#,
        r#"{"ordinal":8,"timestamp":"2026-10-31T12:30:04Z","type":"compacted","payload":{"message":"CANARY_COMPACTED_887"}}"#,
        r#"{"ordinal":9,"timestamp":"2026-10-31T12:30:05Z","type":"token_usage_record","payload":{"response_id":"resp_c1","session_id":"sess-canary-codex","turn_id":"t1","usage":{"input_tokens":2000,"cached_input_tokens":1000,"cache_write_input_tokens":0,"output_tokens":80,"reasoning_output_tokens":20,"total_tokens":2080}}}"#,
    ]
    .join("\n");

    let parsed = parse_codex_reader(BufReader::new(Cursor::new(jsonl.as_bytes())))
        .expect("parse canary codex stream");
    assert_eq!(parsed.records.len(), 1);

    let debug_records = format!("{:?}", parsed.records);
    let debug_diagnostics = format!("{:?}", parsed.diagnostics);
    let ndjson = encode_ndjson(&parsed.records).expect("encode ndjson");

    for canary in canaries {
        assert!(
            !debug_records.contains(canary),
            "privacy leak: canary `{canary}` found in TurnRecord debug output"
        );
        assert!(
            !ndjson.contains(canary),
            "privacy leak: canary `{canary}` found in NDJSON ledger"
        );
        assert!(
            !debug_diagnostics.contains(canary),
            "privacy leak: canary `{canary}` found in ParseDiagnostics"
        );
    }

    // Also verify JSON key-order invariance of `hash_codex_tool_arguments`.
    let (_, h1) = hash_codex_tool_arguments("exec_command", r#"{"a":1,"b":2}"#);
    let (_, h2) = hash_codex_tool_arguments("exec_command", r#"{"b":2,"a":1}"#);
    assert_eq!(
        h1, h2,
        "reordered JSON keys must hash to identical key_hash"
    );
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        cases: 48,
        .. ProptestConfig::default()
    })]

    #[test]
    fn proptest_inclusive_normalisation_and_usage_mode_equivalence(
        turns in prop::collection::vec(
            (1_u64..10_000, 0_u64..10_000, 0_u64..5_000, 1_u64..2_000, 0_u64..1_000),
            1..=6
        )
    ) {
        let mut lines_tur_tc = Vec::new();
        let mut lines_tc_tur = Vec::new();
        let mut lines_last_only = Vec::new();
        let mut lines_cum_only = Vec::new();

        let header = r#"{"ordinal":0,"timestamp":"2026-10-31T13:00:00Z","type":"session_meta","payload":{"session_id":"sess-prop","cwd":"/synthetic/prop","cli_version":"0.165.0","source":"cli"}}"#.to_string();
        let ctx = r#"{"ordinal":1,"timestamp":"2026-10-31T13:00:01Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5-codex","cwd":"/synthetic/prop"}}"#.to_string();
        for vec in [&mut lines_tur_tc, &mut lines_tc_tur, &mut lines_last_only, &mut lines_cum_only] {
            vec.push(header.clone());
            vec.push(ctx.clone());
        }

        let mut cum_in = 0_u64;
        let mut cum_read = 0_u64;
        let mut cum_write = 0_u64;
        let mut cum_out = 0_u64;
        let mut cum_reas = 0_u64;

        for (idx, &(fresh, read, write, out_base, reas)) in turns.iter().enumerate() {
            let out = out_base.max(reas);
            let inc_in = fresh + read + write;
            let tot = inc_in + out;

            cum_in += inc_in;
            cum_read += read;
            cum_write += write;
            cum_out += out;
            cum_reas += reas;
            let cum_tot = cum_in + cum_out;

            let ts = format!("2026-10-31T13:00:{:02}Z", (idx + 2).min(59));
            let user_line = format!(
                r#"{{"ordinal":{},"timestamp":"{ts}","type":"response_item","payload":{{"id":"u_{idx}","type":"message","role":"user","content":[{{"type":"input_text","text":"turn prompt"}}]}}}}"#,
                idx * 4 + 2
            );
            let tur_line = format!(
                r#"{{"ordinal":{},"timestamp":"{ts}","type":"token_usage_record","payload":{{"response_id":"resp_{idx}","session_id":"sess-prop","turn_id":"t1","usage":{{"input_tokens":{inc_in},"cached_input_tokens":{read},"cache_write_input_tokens":{write},"output_tokens":{out},"reasoning_output_tokens":{reas},"total_tokens":{tot}}}}}}}"#,
                idx * 4 + 3
            );
            let tc_full_line = format!(
                r#"{{"ordinal":{},"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{inc_in},"cached_input_tokens":{read},"cache_write_input_tokens":{write},"output_tokens":{out},"reasoning_output_tokens":{reas},"total_tokens":{tot}}},"total_token_usage":{{"input_tokens":{cum_in},"cached_input_tokens":{cum_read},"cache_write_input_tokens":{cum_write},"output_tokens":{cum_out},"reasoning_output_tokens":{cum_reas},"total_tokens":{cum_tot}}}}}}}}}"#,
                idx * 4 + 4
            );
            let tc_cum_line = format!(
                r#"{{"ordinal":{},"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{cum_in},"cached_input_tokens":{cum_read},"cache_write_input_tokens":{cum_write},"output_tokens":{cum_out},"reasoning_output_tokens":{cum_reas},"total_tokens":{cum_tot}}}}}}}}}"#,
                idx * 4 + 4
            );

            lines_tur_tc.push(user_line.clone());
            lines_tur_tc.push(tur_line.clone());
            lines_tur_tc.push(tc_full_line.clone());

            lines_tc_tur.push(user_line.clone());
            lines_tc_tur.push(tc_full_line.clone());
            lines_tc_tur.push(tur_line);

            lines_last_only.push(user_line.clone());
            lines_last_only.push(tc_full_line);

            lines_cum_only.push(user_line);
            lines_cum_only.push(tc_cum_line);
        }

        let p_tur_tc = parse_codex_reader(BufReader::new(Cursor::new(lines_tur_tc.join("\n").into_bytes()))).unwrap();
        let p_tc_tur = parse_codex_reader(BufReader::new(Cursor::new(lines_tc_tur.join("\n").into_bytes()))).unwrap();
        let p_last = parse_codex_reader(BufReader::new(Cursor::new(lines_last_only.join("\n").into_bytes()))).unwrap();
        let p_cum = parse_codex_reader(BufReader::new(Cursor::new(lines_cum_only.join("\n").into_bytes()))).unwrap();

        prop_assert_eq!(p_tur_tc.records.len(), turns.len());
        prop_assert_eq!(p_tc_tur.records.len(), turns.len());
        prop_assert_eq!(p_last.records.len(), turns.len());
        prop_assert_eq!(p_cum.records.len(), turns.len());

        for (i, &(fresh, read, write, out_base, reas)) in turns.iter().enumerate() {
            let out = out_base.max(reas);
            let inc_in = fresh + read + write;

            let u1 = &p_tur_tc.records[i].usage;
            let u2 = &p_tc_tur.records[i].usage;
            let u3 = &p_last.records[i].usage;
            let u4 = &p_cum.records[i].usage;

            prop_assert_eq!(u1, u2);
            prop_assert_eq!(u1, u3);
            prop_assert_eq!(u1, u4);

            prop_assert_eq!(u1.fresh_in, fresh);
            prop_assert_eq!(u1.cache_read, read);
            prop_assert_eq!(u1.cache_write, write);
            prop_assert_eq!(u1.prompt_tokens(), inc_in);
            prop_assert_eq!(u1.output, out);
            prop_assert_eq!(u1.reasoning, Some(reas));
        }
    }
}
