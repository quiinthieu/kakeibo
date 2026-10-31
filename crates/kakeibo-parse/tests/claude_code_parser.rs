//! TDD integration, privacy, hostile-input, and property tests for the Claude Code
//! discovery and streaming JSONL parser (`PLAN.md` §5, §7, §9, §10 T04).

use std::fs;
use std::io::{BufReader, Cursor, Write};
use std::path::{Path, PathBuf};

use kakeibo_core::ledger::encode_ndjson;
use kakeibo_core::{AgentId, BlockKind, Confidence};
use kakeibo_parse::{
    parse_claude_file, parse_claude_file_with_options, parse_claude_reader, ClaudeParseOptions,
    ParseDiagnosticKind, PathCatalog, DEFAULT_MAX_LINE_BYTES,
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
    usage: ExpectedUsage,
    added_blocks: Vec<ExpectedBlock>,
    hints: ExpectedHints,
    rate_limit_used_percent: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ExpectedUsage {
    fresh_in: u64,
    cache_write: u64,
    cache_read: u64,
    output: u64,
    reasoning: Option<u64>,
    confidence: String,
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
    turns: u32,
    prompt_tokens: u64,
    fresh_in: u64,
    cache_write: u64,
    cache_read: u64,
    output: u64,
    reasoning: u64,
}

#[test]
fn basic_and_streaming_dupes_match_expected_json_exactly() {
    let root = workspace_root();

    for scenario in ["basic", "streaming-dupes"] {
        let scenario_dir = root.join("tests/fixtures/claude-code").join(scenario);
        let gt: ExpectedGroundTruth = serde_json::from_str(
            &fs::read_to_string(scenario_dir.join("expected.json")).expect("read expected.json"),
        )
        .expect("parse expected.json");
        assert_eq!(gt.agent, "claude-code");
        assert_eq!(gt.sessions.len(), 1);
        let expected_session = &gt.sessions[0];

        let temp = TempDir::new().expect("tempdir");
        let synth_home = temp.path().join("home");
        let claude_dir = synth_home.join(".claude");
        let mut staged_hashes = Vec::new();

        for rel in &gt.log_files {
            let src = scenario_dir.join(rel);
            let dst = claude_dir.join(rel);
            fs::create_dir_all(dst.parent().unwrap()).unwrap();
            fs::copy(&src, &dst).unwrap();
            staged_hashes.push((dst.clone(), sha256_file(&dst)));
        }

        let _ro = ReadOnlyTreeGuard::lock(&synth_home);

        let catalog = PathCatalog::new(Some(synth_home.clone()), None, None);
        let discovered = catalog
            .discover_claude_top_level()
            .expect("discover claude top level");
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].agent, AgentId::ClaudeCode);

        let parsed = parse_claude_file(&discovered[0].path).expect("parse_claude_file");
        assert!(
            parsed.diagnostics.issues.is_empty(),
            "unexpected diagnostics in {scenario}: {:?}",
            parsed.diagnostics.issues
        );
        assert_eq!(
            parsed.diagnostics.ignored_records, expected_session.ignored_records,
            "ignored_records mismatch in {scenario}"
        );
        assert!(parsed.diagnostics.cli_versions.contains("2.1.314"));

        assert_eq!(
            parsed.records.len(),
            expected_session.turns.len(),
            "turn count mismatch in {scenario}"
        );

        let mut tot_prompt = 0_u64;
        let mut tot_fresh = 0_u64;
        let mut tot_write = 0_u64;
        let mut tot_read = 0_u64;
        let mut tot_output = 0_u64;
        let mut tot_reasoning = 0_u64;

        for (actual, exp) in parsed.records.iter().zip(expected_session.turns.iter()) {
            assert_eq!(actual.agent, AgentId::ClaudeCode);
            assert_eq!(actual.session_id, expected_session.session_id);
            assert_eq!(actual.parent_session_id, None);
            assert_eq!(actual.turn, exp.turn);
            assert_eq!(actual.ts.to_string(), exp.ts);
            assert_eq!(actual.model.as_deref(), Some(exp.model.as_str()));
            assert_eq!(
                actual.model.as_deref(),
                Some(expected_session.model.as_str())
            );
            assert!(
                actual
                    .cwd_hash
                    .as_deref()
                    .is_some_and(|h| h.len() == 16 && h.chars().all(|c| c.is_ascii_hexdigit())),
                "cwd_hash must be a 16-char hex digest"
            );

            assert_eq!(actual.usage.prompt_tokens(), exp.prompt_tokens);
            assert_eq!(actual.usage.fresh_in, exp.usage.fresh_in);
            assert_eq!(actual.usage.cache_write, exp.usage.cache_write);
            assert_eq!(actual.usage.cache_read, exp.usage.cache_read);
            assert_eq!(actual.usage.output, exp.usage.output);
            assert_eq!(actual.usage.reasoning, exp.usage.reasoning);
            assert_eq!(actual.usage.confidence.as_str(), exp.usage.confidence);

            assert_eq!(actual.hints.resume_start, exp.hints.resume_start);
            assert_eq!(actual.hints.compaction, exp.hints.compaction);
            assert_eq!(actual.hints.sidechain, exp.hints.sidechain);
            assert_eq!(actual.hints.api_error, exp.hints.api_error);
            assert_eq!(
                actual.hints.rate_limit_used_percent,
                exp.rate_limit_used_percent
            );

            assert_eq!(
                actual.added.len(),
                exp.added_blocks.len(),
                "added block count mismatch on turn {} of {scenario}",
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
            tot_output += actual.usage.output;
            tot_reasoning += actual.usage.reasoning.unwrap_or(0);
        }

        assert_eq!(
            u32::try_from(parsed.records.len()).unwrap(),
            expected_session.totals.turns
        );
        assert_eq!(tot_prompt, expected_session.totals.prompt_tokens);
        assert_eq!(tot_fresh, expected_session.totals.fresh_in);
        assert_eq!(tot_write, expected_session.totals.cache_write);
        assert_eq!(tot_read, expected_session.totals.cache_read);
        assert_eq!(tot_output, expected_session.totals.output);
        assert_eq!(tot_reasoning, expected_session.totals.reasoning);

        // Verify read-only files are byte-for-byte untouched.
        for (path, before) in staged_hashes {
            assert_eq!(sha256_file(&path), before);
        }
    }
}

#[test]
fn discovery_respects_top_level_only_and_claude_config_dir() {
    let temp = TempDir::new().expect("tempdir");
    let unused_home = temp.path().join("empty_home");
    let custom_claude = temp.path().join("custom_claude_cfg");

    let slug_dir = custom_claude
        .join("projects")
        .join("-synthetic-workspace-beta");
    let top_session_path = slug_dir.join("parent-sess-01.jsonl");
    let subagent_dir = slug_dir.join("parent-sess-01").join("subagents");
    let subagent_path = subagent_dir.join("agent-sub01.jsonl");

    fs::create_dir_all(&subagent_dir).unwrap();
    fs::write(
        &top_session_path,
        concat!(
            r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"parent-sess-01","timestamp":"2026-10-31T10:00:00Z","cwd":"/synthetic/workspace/beta","isSidechain":false,"message":{"role":"user","content":"hello world"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"parent-sess-01","requestId":"req_1","timestamp":"2026-10-31T10:00:02Z","cwd":"/synthetic/workspace/beta","isSidechain":false,"message":{"id":"msg_1","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":50,"cache_creation_input_tokens":100,"cache_read_input_tokens":0,"output_tokens":10}}}"#,
            "\n"
        ),
    )
    .unwrap();

    // Verified Claude Code 2.1.314 subagent layout: `sessionId` matches parent directory name while `agentId` identifies subagent.
    fs::write(
        &subagent_path,
        concat!(
            r#"{"type":"user","uuid":"su1","parentUuid":null,"sessionId":"parent-sess-01","agentId":"sub01","timestamp":"2026-10-31T10:00:03Z","cwd":"/synthetic/workspace/beta","isSidechain":true,"message":{"role":"user","content":"subagent task"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"sa1","parentUuid":"su1","sessionId":"parent-sess-01","agentId":"sub01","requestId":"req_sub1","timestamp":"2026-10-31T10:00:05Z","cwd":"/synthetic/workspace/beta","isSidechain":true,"message":{"id":"msg_sub1","role":"assistant","model":"claude-haiku-4-5","content":[{"type":"text","text":"subagent done"}],"usage":{"input_tokens":20,"cache_creation_input_tokens":80,"cache_read_input_tokens":0,"output_tokens":12}}}"#,
            "\n"
        ),
    )
    .unwrap();

    let catalog = PathCatalog::new(Some(unused_home), Some(custom_claude), None);
    let top = catalog.discover_claude_top_level().unwrap();
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].path, top_session_path);

    let subs = catalog.discover_claude_subagents().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].parent_session_id, "parent-sess-01");
    assert_eq!(subs[0].path, subagent_path);

    let sub_parsed = parse_claude_file_with_options(
        &subs[0].path,
        &ClaudeParseOptions {
            parent_session_id: Some(subs[0].parent_session_id.clone()),
            max_line_bytes: None,
        },
    )
    .unwrap();
    assert_eq!(sub_parsed.records.len(), 1);
    assert_eq!(
        sub_parsed.records[0].session_id,
        "parent-sess-01:subagent:sub01"
    );
    assert_eq!(
        sub_parsed.records[0].parent_session_id.as_deref(),
        Some("parent-sess-01")
    );
    assert!(sub_parsed.records[0].hints.sidechain);
}

#[test]
fn hints_and_key_hash_capture_resume_compaction_sidechain_and_errors() {
    let stream = concat!(
        // Turn 1: Read tool_use with keys ordered {"b":2,"a":1}
        r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"sess-hints","timestamp":"2026-10-31T11:00:00Z","cwd":"/synthetic/workspace/alpha","isSidechain":false,"message":{"role":"user","content":"start"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"sess-hints","requestId":"req_1","timestamp":"2026-10-31T11:00:02Z","cwd":"/synthetic/workspace/alpha","isSidechain":false,"message":{"id":"msg_1","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"b":2,"a":1}}],"usage":{"input_tokens":100,"cache_creation_input_tokens":500,"cache_read_input_tokens":0,"output_tokens":30}}}"#,
        "\n",
        // Away summary -> resume_start on Turn 2, plus failed tool_result + zero-token API error before retry!
        r#"{"type":"system","subtype":"away_summary","uuid":"sys1","parentUuid":"a1","sessionId":"sess-hints","timestamp":"2026-10-31T13:00:00Z"}"#,
        "\n",
        r#"{"type":"user","uuid":"u2","parentUuid":"a1","sessionId":"sess-hints","timestamp":"2026-10-31T13:00:01Z","isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":"error reading"}]}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a_err","parentUuid":"u2","sessionId":"sess-hints","isApiErrorMessage":true,"timestamp":"2026-10-31T13:00:02Z","message":{"id":"msg_err","role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"API Error 529"}],"usage":{"input_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0}}}"#,
        "\n",
        // Turn 2: retried Read tool_use with keys ordered {"a":1,"b":2} -> must produce IDENTICAL key_hash!
        r#"{"type":"assistant","uuid":"a2","parentUuid":"u2","sessionId":"sess-hints","requestId":"req_2","timestamp":"2026-10-31T13:00:05Z","isSidechain":false,"message":{"id":"msg_2","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"tool_use","id":"toolu_2","name":"Read","input":{"a":1,"b":2}}],"usage":{"input_tokens":40,"cache_creation_input_tokens":600,"cache_read_input_tokens":0,"output_tokens":32}}}"#,
        "\n",
        // Compaction boundary + compact summary user record -> CompactionSummary block + hints.compaction on Turn 3
        r#"{"type":"system","subtype":"compact_boundary","uuid":"sys2","parentUuid":null,"logicalParentUuid":"a2","sessionId":"sess-hints","timestamp":"2026-10-31T13:05:00Z","compactMetadata":{"preTokens":640,"postTokens":120}}"#,
        "\n",
        r#"{"type":"user","uuid":"u3","parentUuid":"sys2","sessionId":"sess-hints","isCompactSummary":true,"timestamp":"2026-10-31T13:05:01Z","message":{"role":"user","content":"compacted summary payload"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a3","parentUuid":"u3","sessionId":"sess-hints","requestId":"req_3","timestamp":"2026-10-31T13:05:04Z","isSidechain":true,"message":{"id":"msg_3","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"post-compact reply"}],"usage":{"input_tokens":120,"cache_creation_input_tokens":80,"cache_read_input_tokens":0,"output_tokens":25}}}"#,
        "\n"
    );

    let out = parse_claude_reader(
        Cursor::new(stream.as_bytes()),
        &ClaudeParseOptions::default(),
    )
    .expect("parse hints stream");

    // 3 real turns (zero-token a_err was NOT emitted as a turn; away_summary + a_err + compact_boundary = 3 ignored records)
    assert_eq!(out.records.len(), 3);
    assert_eq!(out.diagnostics.ignored_records, 3);

    let t1 = &out.records[0];
    assert_eq!(t1.turn, 1);
    assert!(!t1.hints.resume_start);
    assert!(!t1.hints.compaction);
    assert!(!t1.hints.api_error);
    assert!(!t1.hints.sidechain);
    let t1_tool_hash = t1.added[1].key_hash.clone().expect("t1 tool_use key_hash");

    let t2 = &out.records[1];
    assert_eq!(t2.turn, 2);
    assert!(t2.hints.resume_start);
    assert!(t2.hints.api_error);
    assert!(!t2.hints.compaction);
    // Added[0] is the failed tool_result resolving tool="Read" and matching t1's key_hash
    assert_eq!(t2.added[0].kind, BlockKind::ToolResult);
    assert_eq!(t2.added[0].tool.as_deref(), Some("Read"));
    assert!(t2.added[0].is_error);
    assert_eq!(t2.added[0].key_hash.as_deref(), Some(t1_tool_hash.as_str()));
    // Added[1] is the retried tool_use with {"a":1,"b":2} -> identical canonical key_hash!
    assert_eq!(t2.added[1].kind, BlockKind::ToolUse);
    assert_eq!(t2.added[1].key_hash.as_deref(), Some(t1_tool_hash.as_str()));

    let t3 = &out.records[2];
    assert_eq!(t3.turn, 3);
    assert!(!t3.hints.resume_start);
    assert!(t3.hints.compaction);
    assert!(t3.hints.sidechain);
    assert!(!t3.hints.api_error);
    assert_eq!(t3.added[0].kind, BlockKind::CompactionSummary);
    assert_eq!(t3.added[0].bytes, "compacted summary payload".len() as u64);
}

#[test]
fn hostile_log_emits_diagnostics_without_panicking() {
    let temp = TempDir::new().expect("tempdir");
    let hostile_path = temp.path().join("hostile-session.jsonl");

    {
        let mut f = fs::File::create(&hostile_path).unwrap();
        // Line 1: valid user
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"sess-hostile","timestamp":"2026-10-31T12:00:00Z","message":{{"role":"user","content":"ok prompt 1"}}}}"#
        )
        .unwrap();
        // Line 2: valid assistant turn 1
        writeln!(
            f,
            r#"{{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"sess-hostile","requestId":"req_1","timestamp":"2026-10-31T12:00:02Z","message":{{"id":"msg_1","role":"assistant","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"ok reply 1"}}],"usage":{{"input_tokens":20,"cache_creation_input_tokens":100,"cache_read_input_tokens":0,"output_tokens":15}}}}}}"#
        )
        .unwrap();
        // Line 3: unknown record type
        writeln!(f, r#"{{"type":"future-record-v99","payload":123}}"#).unwrap();
        // Line 4: invalid UTF-8 bytes
        f.write_all(b"{\"type\":\"user\",\"bad\":\"\xff\xfe\"}\n")
            .unwrap();
        // Line 5: 20 MiB single line (> 16 MiB cap) streamed in 64 KiB chunks
        f.write_all(b"{\"type\":\"user\",\"huge\":\"").unwrap();
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..320 {
            // 320 * 64 KiB = 20 MiB
            f.write_all(&chunk).unwrap();
        }
        f.write_all(b"\"}\n").unwrap();
        // Line 6: malformed JSON
        f.write_all(b"{\"type\":\"assistant\",\"broken_json\":}\n")
            .unwrap();
        // Line 7: assistant missing usage
        writeln!(
            f,
            r#"{{"type":"assistant","uuid":"a_bad","sessionId":"sess-hostile","timestamp":"2026-10-31T12:00:03Z","message":{{"id":"msg_bad","role":"assistant","content":[]}}}}"#
        )
        .unwrap();
        // Line 8: valid user 2
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u2","parentUuid":"a1","sessionId":"sess-hostile","timestamp":"2026-10-31T12:00:04Z","message":{{"role":"user","content":"ok prompt 2"}}}}"#
        )
        .unwrap();
        // Line 9: valid assistant turn 2
        writeln!(
            f,
            r#"{{"type":"assistant","uuid":"a2","parentUuid":"u2","sessionId":"sess-hostile","requestId":"req_2","timestamp":"2026-10-31T12:00:06Z","message":{{"id":"msg_2","role":"assistant","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"ok reply 2"}}],"usage":{{"input_tokens":15,"cache_creation_input_tokens":10,"cache_read_input_tokens":120,"output_tokens":18}}}}}}"#
        )
        .unwrap();
        // Line 10: truncated final JSON line at EOF (no trailing newline)
        f.write_all(b"{\"type\":\"user\",\"truncated").unwrap();
    }

    let _ro = ReadOnlyTreeGuard::lock(temp.path());

    let parsed = parse_claude_file(&hostile_path).expect("hostile log must not fail I/O");
    assert_eq!(parsed.records.len(), 2);
    assert_eq!(parsed.records[0].turn, 1);
    assert_eq!(parsed.records[0].usage.prompt_tokens(), 120);
    assert_eq!(parsed.records[1].turn, 2);
    assert_eq!(parsed.records[1].usage.prompt_tokens(), 145);
    assert_eq!(
        parsed
            .diagnostics
            .unknown_record_types
            .get("future-record-v99"),
        Some(&1)
    );

    let issues = &parsed.diagnostics.issues;
    assert_eq!(
        issues.len(),
        5,
        "expected 5 line diagnostics, got {issues:?}"
    );
    assert_eq!(issues[0].line_number, 4);
    assert_eq!(issues[0].kind, ParseDiagnosticKind::InvalidUtf8);

    assert_eq!(issues[1].line_number, 5);
    match issues[1].kind {
        ParseDiagnosticKind::OversizedLine { byte_len, limit } => {
            assert_eq!(limit, DEFAULT_MAX_LINE_BYTES);
            assert!(byte_len >= 20 * 1024 * 1024);
        }
        ref other => panic!("expected OversizedLine at line 5, got {other:?}"),
    }

    assert_eq!(issues[2].line_number, 6);
    assert_eq!(issues[2].kind, ParseDiagnosticKind::MalformedJson);

    assert_eq!(issues[3].line_number, 7);
    assert_eq!(
        issues[3].kind,
        ParseDiagnosticKind::MissingField {
            field: "message.usage"
        }
    );

    assert_eq!(issues[4].line_number, 10);
    assert_eq!(issues[4].kind, ParseDiagnosticKind::MalformedJson);
}

#[test]
fn privacy_canary_strings_never_appear_in_records_ledger_or_diagnostics() {
    let canaries = [
        "KAKEIBO_CANARY_PROMPT_9912",
        "KAKEIBO_CANARY_THINKING_8821",
        "KAKEIBO_CANARY_SIGNATURE_7710",
        "KAKEIBO_CANARY_ASSISTANT_6609",
        "KAKEIBO_CANARY_TOOL_ARG_5508",
        "KAKEIBO_CANARY_TOOL_OUT_4407",
        "/synthetic/secret-canary-cwd-3306",
        "feature/secret-canary-branch-2205",
        "KAKEIBO_CANARY_HOSTILE_LINE_1104",
        "KAKEIBO_CANARY_BAD_TYPE_WITH_SPACES_0003",
    ];

    let temp = TempDir::new().expect("tempdir");
    let log_path = temp.path().join("canary.jsonl");
    let payload = format!(
        concat!(
            r#"{{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"sess-priv","timestamp":"2026-10-31T12:10:00Z","cwd":"{cwd}","gitBranch":"{branch}","message":{{"role":"user","content":"{prompt}"}}}}"#,
            "\n",
            r#"{{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"sess-priv","requestId":"req_p1","timestamp":"2026-10-31T12:10:03Z","cwd":"{cwd}","gitBranch":"{branch}","message":{{"id":"msg_p1","role":"assistant","model":"claude-sonnet-4-5","content":[{{"type":"thinking","thinking":"{think}","signature":"{sig}"}},{{"type":"text","text":"{asst}"}},{{"type":"tool_use","id":"toolu_p1","name":"Read","input":{{"file_path":"{arg}"}}}}],"usage":{{"input_tokens":30,"cache_creation_input_tokens":200,"cache_read_input_tokens":0,"output_tokens":50}}}}}}"#,
            "\n",
            r#"{{"type":"user","uuid":"u2","parentUuid":"a1","sessionId":"sess-priv","timestamp":"2026-10-31T12:10:04Z","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_p1","is_error":false,"content":"{tout}"}}]}}}}"#,
            "\n",
            r#"{{"type":"assistant","uuid":"a2","parentUuid":"u2","sessionId":"sess-priv","requestId":"req_p2","timestamp":"2026-10-31T12:10:06Z","cwd":"{cwd}","message":{{"id":"msg_p2","role":"assistant","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"{asst}"}}],"usage":{{"input_tokens":20,"cache_creation_input_tokens":10,"cache_read_input_tokens":230,"output_tokens":15}}}}}}"#,
            "\n",
            r#"{{"type":"{bad_type}","note":"hostile non-identifier type"}}"#,
            "\n",
            r#"{{"broken":"{hostile}"#,
            "\n"
        ),
        prompt = canaries[0],
        think = canaries[1],
        sig = canaries[2],
        asst = canaries[3],
        arg = canaries[4],
        tout = canaries[5],
        cwd = canaries[6],
        branch = canaries[7],
        hostile = canaries[8],
        bad_type = canaries[9],
    );
    fs::write(&log_path, payload).unwrap();
    let hash_before = sha256_file(&log_path);
    let _ro = ReadOnlyTreeGuard::lock(temp.path());

    let parsed = parse_claude_file(&log_path).unwrap();
    assert_eq!(parsed.records.len(), 2);
    assert_eq!(parsed.records[0].usage.confidence, Confidence::Measured);
    assert_eq!(sha256_file(&log_path), hash_before);

    let ledger_ndjson = encode_ndjson(&parsed.records).unwrap();
    let debug_dump = format!("{parsed:?}");

    for canary in canaries {
        assert!(
            !ledger_ndjson.contains(canary),
            "PRIVACY VIOLATION: canary `{canary}` leaked into NDJSON ledger"
        );
        assert!(
            !debug_dump.contains(canary),
            "PRIVACY VIOLATION: canary `{canary}` leaked into Debug representation"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn streaming_dedupe_is_idempotent_and_keeps_max_output(
        partial_outputs in proptest::collection::vec(1_u64..5_000, 1..6),
        dup_mask in proptest::collection::vec(any::<bool>(), 1..6),
    ) {
        let expected_max = *partial_outputs.iter().max().unwrap();

        let mut lines = Vec::new();
        lines.push(r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"sess-prop","timestamp":"2026-10-31T09:00:00Z","message":{"role":"user","content":"synthetic prompt"}}"#.to_string());

        for (idx, &out_toks) in partial_outputs.iter().enumerate() {
            let line = format!(
                r#"{{"type":"assistant","uuid":"a_{idx}","parentUuid":"u1","sessionId":"sess-prop","requestId":"req_same","timestamp":"2026-10-31T09:00:{sec:02}Z","message":{{"id":"msg_same","role":"assistant","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"chunk-{idx}-payload"}}],"usage":{{"input_tokens":100,"cache_creation_input_tokens":400,"cache_read_input_tokens":0,"output_tokens":{out_toks}}}}}}}"#,
                sec = (idx + 1).min(59)
            );
            lines.push(line.clone());
            if dup_mask.get(idx).copied().unwrap_or(false) {
                // Exact duplicate line (same uuid) AND duplicate content with fresh uuid
                lines.push(line);
                let fresh_uuid_dup = format!(
                    r#"{{"type":"assistant","uuid":"a_dup_{idx}","parentUuid":"u1","sessionId":"sess-prop","requestId":"req_same","timestamp":"2026-10-31T09:00:{sec:02}Z","message":{{"id":"msg_same","role":"assistant","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"chunk-{idx}-payload"}}],"usage":{{"input_tokens":100,"cache_creation_input_tokens":400,"cache_read_input_tokens":0,"output_tokens":{out_toks}}}}}}}"#,
                    sec = (idx + 1).min(59)
                );
                lines.push(fresh_uuid_dup);
            }
        }

        let once_stream = lines.join("\n") + "\n";
        let twice_stream = format!("{once_stream}{once_stream}");

        let out_once = parse_claude_reader(
            Cursor::new(once_stream.as_bytes()),
            &ClaudeParseOptions::default(),
        )
        .unwrap();
        let out_twice = parse_claude_reader(
            Cursor::new(twice_stream.as_bytes()),
            &ClaudeParseOptions::default(),
        )
        .unwrap();

        prop_assert_eq!(&out_once.records, &out_twice.records);
        prop_assert_eq!(out_once.records.len(), 1);
        prop_assert_eq!(out_once.records[0].usage.output, expected_max);
        // 1 user_prompt + partial_outputs.len() distinct assistant_text chunks
        prop_assert_eq!(out_once.records[0].added.len(), 1 + partial_outputs.len());
    }

    #[test]
    fn chunked_bufread_matches_one_pass(
        buf_cap in 17_usize..257,
    ) {
        let root = workspace_root();
        let bytes = fs::read(
            root.join("tests/fixtures/claude-code/streaming-dupes/projects/-synthetic-workspace-alpha/session-dupes-0002.jsonl"),
        )
        .unwrap();

        let one_pass = parse_claude_reader(
            Cursor::new(&bytes),
            &ClaudeParseOptions::default(),
        )
        .unwrap();
        let chunked = parse_claude_reader(
            BufReader::with_capacity(buf_cap, Cursor::new(&bytes)),
            &ClaudeParseOptions::default(),
        )
        .unwrap();

        prop_assert_eq!(chunked, one_pass);
    }
}
