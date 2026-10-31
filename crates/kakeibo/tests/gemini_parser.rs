//! Integration and adversarial tests for the Gemini CLI JSONL/JSON decoder (`PLAN.md` §5, §10 T10).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;

use common::{ExpectedBlock, ExpectedHints, ExpectedTotals, ExpectedUsage, TestEnv};
use kakeibo_core::{AgentId, BlockKind, Confidence, TurnRecord};
use kakeibo_parse::hash::hash_cwd;
use kakeibo_parse::{
    extract_gemini_session_cwds, parse_gemini_file, parse_gemini_file_with_options,
    parse_gemini_reader, GeminiParseOptions, ParseDiagnosticKind, PathCatalog,
};
use kakeibo_report::ndjson::write_ledger;

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

fn assert_matches_ground_truth(records: &[TurnRecord], scenario: &common::ExpectedGroundTruth) {
    assert_eq!(scenario.sessions.len(), 1);
    let exp_session = &scenario.sessions[0];
    assert_eq!(records.len(), exp_session.turns.len());

    let mut sum_prompt = 0_u64;
    let mut sum_fresh = 0_u64;
    let mut sum_write = 0_u64;
    let mut sum_read = 0_u64;
    let mut sum_out = 0_u64;
    let mut sum_reasoning = 0_u64;

    for (tr, exp) in records.iter().zip(&exp_session.turns) {
        assert_eq!(tr.agent, AgentId::GeminiCli);
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
            "usage mismatch on turn {}",
            exp.turn
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
            "added_blocks mismatch on turn {}",
            exp.turn
        );

        let actual_hints = ExpectedHints {
            resume_start: tr.hints.resume_start,
            compaction: tr.hints.compaction,
            sidechain: tr.hints.sidechain,
            api_error: tr.hints.api_error,
        };
        assert_eq!(
            actual_hints, exp.hints,
            "hints mismatch on turn {}",
            exp.turn
        );
        assert_eq!(
            tr.hints.rate_limit_used_percent,
            exp.rate_limit_used_percent
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
fn gemini_rewind_patch_and_legacy_json_scenarios_match_ground_truth() {
    for (scenario_name, expected_raw_cwd) in [
        ("rewind-patch", "/synthetic/workspace/gemini-alpha"),
        ("legacy-json", "/synthetic/workspace/gemini-legacy"),
    ] {
        let env = TestEnv::new();
        let (staged, expected) = env.stage_fixture("gemini-cli", scenario_name);
        let _ro = env.make_logs_readonly();

        let catalog = PathCatalog::new(Some(env.home_dir().to_path_buf()), None, None);
        let slug_registry = catalog
            .load_gemini_slug_registry()
            .expect("load slug registry");
        let discovered = catalog
            .discover_gemini_top_level()
            .expect("discover gemini top-level");
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].path, staged[0]);

        let parsed = parse_gemini_file(&staged[0]).expect("parse staged gemini transcript");
        assert_eq!(
            parsed.diagnostics.ignored_records, expected.sessions[0].ignored_records,
            "ignored_records mismatch in {scenario_name}"
        );
        assert!(
            parsed.diagnostics.issues.is_empty(),
            "unexpected parse issues in {scenario_name}: {:?}",
            parsed.diagnostics.issues
        );
        assert_matches_ground_truth(&parsed.records, &expected);

        let expected_hash = hash_cwd(expected_raw_cwd);
        for tr in &parsed.records {
            assert_eq!(tr.cwd_hash.as_deref(), Some(expected_hash.as_str()));
        }

        // Verify zero raw path / transcript leakage in NDJSON ledger serialization.
        let mut ledger_buf = Vec::new();
        write_ledger(&mut ledger_buf, &parsed.records).expect("serialize ledger");
        let ledger_str = String::from_utf8(ledger_buf).expect("utf-8 ledger");
        assert!(!ledger_str.contains(expected_raw_cwd));
        assert!(!ledger_str.contains("Inspect parser module"));

        // Verify opt-in `export --show-paths` extractor resolves `session_id -> raw_cwd`.
        let cwds = extract_gemini_session_cwds(&staged[0], &slug_registry)
            .expect("extract opt-in gemini cwds");
        assert_eq!(
            cwds.get(&expected.sessions[0].session_id)
                .map(String::as_str),
            Some(expected_raw_cwd)
        );
    }
}

#[test]
fn gemini_discovery_prefers_migrated_jsonl_over_legacy_json_and_discovers_subagents() {
    let env = TestEnv::new();
    let chats_dir = env.home_dir().join(".gemini/tmp/proj-mig/chats");
    let sub_dir = chats_dir.join("parent-uuid-0001");
    fs::create_dir_all(&sub_dir).expect("create chats + subagent dirs");
    fs::write(
        env.home_dir().join(".gemini/tmp/proj-mig/.project_root"),
        "/synthetic/workspace/proj-mig\n",
    )
    .expect("write .project_root");

    // Migrated session has BOTH `.json` and `.jsonl` with the same stem; `.jsonl` must win.
    fs::write(
        chats_dir.join("session-2026-10-31T08-00-aaaa1111.json"),
        r#"{"sessionId":"parent-uuid-0001","projectHash":"h","startTime":"2026-10-31T08:00:00Z","lastUpdated":"2026-10-31T08:00:01Z","messages":[]}"#,
    )
    .expect("write legacy migrated json");
    fs::write(
        chats_dir.join("session-2026-10-31T08-00-aaaa1111.jsonl"),
        concat!(
            r#"{"sessionId":"parent-uuid-0001","projectHash":"h","startTime":"2026-10-31T08:00:00Z","lastUpdated":"2026-10-31T08:00:00Z","kind":"main"}"#,
            "\n",
            r#"{"id":"u1","timestamp":"2026-10-31T08:00:01Z","type":"user","content":"hello"}"#,
            "\n",
            r#"{"id":"g1","timestamp":"2026-10-31T08:00:02Z","type":"gemini","model":"gemini-2.5-pro","content":"world","tokens":{"input":500,"output":40,"cached":200,"thoughts":10,"tool":0,"total":550}}"#,
            "\n"
        ),
    )
    .expect("write migrated jsonl");

    // Standalone legacy `.json` with no `.jsonl` counterpart must still be discovered.
    fs::write(
        chats_dir.join("session-2026-10-31T07-00-bbbb2222.json"),
        r#"{"sessionId":"legacy-only-0002","projectHash":"h","startTime":"2026-10-31T07:00:00Z","lastUpdated":"2026-10-31T07:00:02Z","messages":[{"id":"u0","timestamp":"2026-10-31T07:00:01Z","type":"user","content":"hi"},{"id":"g0","timestamp":"2026-10-31T07:00:02Z","type":"gemini","model":"gemini-2.5-flash","content":"ok","tokens":{"input":300,"output":20,"cached":0,"thoughts":0,"tool":0,"total":320}}]}"#,
    )
    .expect("write standalone legacy json");

    // Subagent transcript under `chats/<parent_session_id>/sub-agent-01.jsonl` (plus migrated `.json` twin).
    fs::write(
        sub_dir.join("sub-agent-01.json"),
        r#"{"sessionId":"sub-uuid-01","projectHash":"h","startTime":"2026-10-31T08:00:01Z","lastUpdated":"2026-10-31T08:00:01Z","kind":"subagent","messages":[]}"#,
    )
    .expect("write subagent json twin");
    fs::write(
        sub_dir.join("sub-agent-01.jsonl"),
        concat!(
            r#"{"sessionId":"sub-uuid-01","projectHash":"h","startTime":"2026-10-31T08:00:01Z","lastUpdated":"2026-10-31T08:00:01Z","kind":"subagent"}"#,
            "\n",
            r#"{"id":"su1","timestamp":"2026-10-31T08:00:01Z","type":"user","content":"sub task"}"#,
            "\n",
            r#"{"id":"sg1","timestamp":"2026-10-31T08:00:02Z","type":"gemini","model":"gemini-2.5-flash","content":"sub done","tokens":{"input":400,"output":30,"cached":100,"thoughts":0,"tool":0,"total":430}}"#,
            "\n"
        ),
    )
    .expect("write subagent jsonl");

    let _ro = env.make_logs_readonly();
    let catalog = PathCatalog::new(Some(env.home_dir().to_path_buf()), None, None);

    let top = catalog.discover_gemini_top_level().expect("top level");
    assert_eq!(
        top.len(),
        2,
        "migrated .json twin must be deduplicated: {top:?}"
    );
    assert!(top
        .iter()
        .any(|d| d.path.ends_with("session-2026-10-31T08-00-aaaa1111.jsonl")));
    assert!(top
        .iter()
        .any(|d| d.path.ends_with("session-2026-10-31T07-00-bbbb2222.json")));

    let subs = catalog.discover_gemini_subagents().expect("subagents");
    assert_eq!(
        subs.len(),
        1,
        "subagent .json twin must be deduplicated: {subs:?}"
    );
    assert_eq!(subs[0].agent, AgentId::GeminiCli);
    assert_eq!(subs[0].parent_session_id, "parent-uuid-0001");

    let registry = catalog.load_gemini_slug_registry().expect("registry");
    let opts = GeminiParseOptions {
        parent_session_id: Some(subs[0].parent_session_id.clone()),
        slug_registry: Some(registry),
        max_line_bytes: None,
    };
    let sub_parsed = parse_gemini_file_with_options(&subs[0].path, &opts).expect("parse subagent");
    assert_eq!(sub_parsed.records.len(), 1);
    let sub_tr = &sub_parsed.records[0];
    assert_eq!(sub_tr.session_id, "sub-uuid-01");
    assert_eq!(
        sub_tr.parent_session_id.as_deref(),
        Some("parent-uuid-0001")
    );
    assert!(sub_tr.hints.sidechain);
    assert_eq!(
        sub_tr.cwd_hash.as_deref(),
        Some(hash_cwd("/synthetic/workspace/proj-mig").as_str())
    );
}

#[test]
fn gemini_object_patch_unknown_rewind_clamped_cache_and_hostile_lines() {
    let mut raw: Vec<u8> = Vec::new();
    raw.extend_from_slice(
        br#"{"sessionId":"ses-hostile","projectHash":"ph","startTime":"2026-10-31T11:00:00Z","lastUpdated":"2026-10-31T11:00:00Z","directories":["/synthetic/workspace/hostile"]}"#,
    );
    raw.push(b'\n');
    // Pre-rewind message cleared by `$rewindTo` with unknown message ID.
    raw.extend_from_slice(
        br#"{"id":"old-g1","timestamp":"2026-10-31T11:00:01Z","type":"gemini","model":"gemini-2.5-pro","content":"discarded","tokens":{"input":9000,"output":100,"cached":0,"thoughts":0,"tool":0,"total":9100}}"#,
    );
    raw.push(b'\n');
    raw.extend_from_slice(br#"{"$rewindTo":"non-existent-id"}"#);
    raw.push(b'\n');
    // Invalid UTF-8 line.
    raw.extend_from_slice(b"{\"bad_utf8\":\"\xff\xfe\"}\n");
    // Malformed truncated JSON line.
    raw.extend_from_slice(b"{\"type\":\"gemini\",\"broken\":\n");
    // Oversized line (> 512 bytes custom cap).
    let oversized = format!(
        "{{\"id\":\"huge\",\"type\":\"info\",\"content\":\"{}\"}}\n",
        "x".repeat(700)
    );
    raw.extend_from_slice(oversized.as_bytes());
    // Post-rewind surviving user + gemini turn updated via object-form `$patch`, with `cached > input` clamped.
    raw.extend_from_slice(
        br#"{"id":"u-keep","timestamp":"2026-10-31T11:00:05Z","type":"user","content":"surviving prompt"}"#,
    );
    raw.push(b'\n');
    raw.extend_from_slice(
        br#"{"id":"g-keep","timestamp":"2026-10-31T11:00:06Z","type":"gemini","model":"gemini-2.5-pro","content":"draft"}"#,
    );
    raw.push(b'\n');
    raw.extend_from_slice(
        br#"{"$patch":{"id":"g-keep","content":"final surviving reply","tokens":{"input":400,"output":50,"cached":999,"thoughts":15,"tool":100,"total":565}}}"#,
    );
    raw.push(b'\n');

    let opts = GeminiParseOptions {
        parent_session_id: None,
        slug_registry: Some(BTreeMap::new()),
        max_line_bytes: Some(512),
    };
    let parsed = parse_gemini_reader(Cursor::new(raw), None, &opts).expect("tolerant parse");

    assert_eq!(parsed.records.len(), 1);
    let tr = &parsed.records[0];
    assert_eq!(tr.session_id, "ses-hostile");
    assert_eq!(tr.turn, 1);
    assert_eq!(tr.usage.prompt_tokens(), 500);
    assert_eq!(tr.usage.cache_read, 500, "cached > P must clamp to P");
    assert_eq!(tr.usage.fresh_in, 0);
    assert_eq!(tr.usage.output, 65);
    assert_eq!(tr.usage.reasoning, Some(15));
    assert_eq!(
        tr.cwd_hash.as_deref(),
        Some(hash_cwd("/synthetic/workspace/hostile").as_str())
    );
    assert_eq!(tr.added.len(), 2);
    assert_eq!(tr.added[0].bytes, "surviving prompt".len() as u64);
    assert_eq!(tr.added[1].bytes, "final surviving reply".len() as u64);

    assert!(parsed
        .diagnostics
        .issues
        .iter()
        .any(|d| matches!(d.kind, ParseDiagnosticKind::InvalidUtf8)));
    assert!(parsed
        .diagnostics
        .issues
        .iter()
        .any(|d| matches!(d.kind, ParseDiagnosticKind::MalformedJson)));
    assert!(parsed
        .diagnostics
        .issues
        .iter()
        .any(|d| matches!(d.kind, ParseDiagnosticKind::OversizedLine { .. })));
}
