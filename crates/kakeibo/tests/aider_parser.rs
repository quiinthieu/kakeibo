//! Integration, privacy, and resilience tests for Aider `.aider.chat.history.md`
//! and `--analytics-log` JSONL parsers (`PLAN.md` §0 #4–#6, §5, §10 T11).

mod common;

use std::fs;
use std::io::Cursor;

use common::{ExpectedBlock, ExpectedHints, ExpectedTotals, ExpectedUsage, TestEnv};
use kakeibo_core::{AgentId, BlockKind, Confidence, TurnRecord};
use kakeibo_parse::hash::hash_cwd;
use kakeibo_parse::{
    extract_aider_session_cwds, parse_aider_analytics_file, parse_aider_analytics_reader,
    parse_aider_history_file, parse_aider_history_reader, PathCatalog,
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

fn assert_aider_session_matches_ground_truth(
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
        assert_eq!(tr.agent, AgentId::Aider);
        assert_eq!(tr.session_id, exp_session.session_id);
        assert_eq!(tr.turn, exp.turn);
        assert_eq!(tr.ts.to_string(), exp.ts);
        assert_eq!(tr.model.as_deref(), Some(exp.model.as_str()));
        assert_eq!(tr.usage.prompt_tokens(), exp.prompt_tokens);
        assert_eq!(
            tr.usage.confidence,
            Confidence::Estimated,
            "every Aider turn must carry Confidence::Estimated"
        );

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
fn aider_chat_history_fixture_matches_ground_truth_and_drops_raw_text() {
    let env = TestEnv::new();
    let (staged, gt) = env.stage_fixture("aider", "chat-history");
    assert_eq!(staged.len(), 1);
    let raw_md = fs::read_to_string(&staged[0]).expect("read raw .aider.chat.history.md");
    let prompt_snippets: Vec<&str> = raw_md
        .lines()
        .filter_map(|l| l.strip_prefix("#### "))
        .filter(|s| !s.starts_with('/'))
        .collect();
    assert_eq!(prompt_snippets.len(), 4);

    let _ro_guard = env.make_logs_readonly();

    let catalog = PathCatalog::new(Some(env.home_dir().to_path_buf()), None, None);
    let discovered = catalog
        .discover_aider_histories()
        .expect("discover aider histories");
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].agent, AgentId::Aider);
    assert_eq!(discovered[0].path, staged[0]);

    let out = parse_aider_history_file(&staged[0]).expect("parse .aider.chat.history.md");
    assert!(out.diagnostics.issues.is_empty());
    assert!(out.diagnostics.cli_versions.contains("0.82.2"));
    assert_eq!(out.sessions.len(), gt.sessions.len());

    let expected_cwd_hash = hash_cwd(
        staged[0]
            .parent()
            .and_then(|p| p.to_str())
            .expect("staged parent dir"),
    );

    let mut all_records = Vec::new();
    for (batch, exp_session) in out.sessions.iter().zip(&gt.sessions) {
        assert_eq!(batch.session_id, exp_session.session_id);
        assert_eq!(
            batch.diagnostics.ignored_records,
            exp_session.ignored_records
        );
        assert_aider_session_matches_ground_truth(&batch.records, exp_session);
        for tr in &batch.records {
            assert_eq!(tr.cwd_hash.as_deref(), Some(expected_cwd_hash.as_str()));
            assert_eq!(
                tr.usage.estimation_method.as_deref(),
                Some("aider_history_rounded")
            );
        }
        all_records.extend(batch.records.iter().cloned());
    }

    // Verify positive vs `$0.00` agent-reported message costs.
    let s1 = &out.sessions[0].records;
    assert_eq!(s1[0].usage.agent_reported_cost_usd, Some(0.057));
    assert_eq!(s1[1].usage.agent_reported_cost_usd, Some(0.028));
    assert_eq!(s1[2].usage.agent_reported_cost_usd, None);
    assert_eq!(
        out.sessions[1].records[0].usage.agent_reported_cost_usd,
        Some(0.014)
    );

    // Verify zero raw prompt text leaks into the NDJSON ledger (`PLAN.md` §0 #4).
    let mut ndjson = Vec::new();
    write_ledger(&mut ndjson, &all_records).expect("write NDJSON ledger");
    let ledger_str = String::from_utf8(ndjson).expect("UTF-8 NDJSON");
    for snippet in prompt_snippets {
        assert!(
            !ledger_str.contains(&snippet[..32]),
            "NDJSON ledger leaked raw Aider prompt text"
        );
    }
}

#[test]
fn aider_analytics_log_fixture_matches_ground_truth_and_labels_every_turn_estimated() {
    let env = TestEnv::new();
    let (staged, gt) = env.stage_fixture("aider", "analytics-log");
    assert_eq!(staged.len(), 1);
    let raw_jsonl = fs::read_to_string(&staged[0]).expect("read raw analytics.jsonl");
    let first_json: serde_json::Value =
        serde_json::from_str(raw_jsonl.lines().next().expect("line 1")).expect("parse line 1");
    let raw_user_id = first_json["user_id"]
        .as_str()
        .expect("user_id string")
        .to_string();

    let _ro_guard = env.make_logs_readonly();

    let catalog = PathCatalog::new(Some(env.home_dir().to_path_buf()), None, None);
    let discovered = catalog
        .discover_aider_analytics_logs()
        .expect("discover aider analytics logs");
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].agent, AgentId::Aider);
    assert_eq!(discovered[0].path, staged[0]);

    let out = parse_aider_analytics_file(&staged[0]).expect("parse analytics.jsonl");
    assert!(out.diagnostics.issues.is_empty());
    assert!(out.diagnostics.cli_versions.contains("0.82.2"));
    assert_eq!(out.sessions.len(), 1);

    let batch = &out.sessions[0];
    let exp_session = &gt.sessions[0];
    assert_eq!(
        batch.diagnostics.ignored_records,
        exp_session.ignored_records
    );
    assert_aider_session_matches_ground_truth(&batch.records, exp_session);

    assert_eq!(batch.records[0].usage.agent_reported_cost_usd, Some(0.0412));
    assert_eq!(batch.records[1].usage.agent_reported_cost_usd, Some(0.0528));
    for tr in &batch.records {
        assert_eq!(
            tr.usage.estimation_method.as_deref(),
            Some("aider_analytics_no_cache_split")
        );
        assert!(
            !tr.session_id.contains(&raw_user_id),
            "anonymous user_id must be irreversibly hashed, not embedded raw"
        );
    }

    let mut ndjson = Vec::new();
    write_ledger(&mut ndjson, &batch.records).expect("write NDJSON ledger");
    let ledger_str = String::from_utf8(ndjson).expect("UTF-8 NDJSON");
    assert!(
        !ledger_str.contains(&raw_user_id),
        "NDJSON ledger must not contain raw analytics user_id"
    );
}

#[test]
fn aider_parsers_handle_m_suffix_multiline_prompts_reset_and_malformed_lines() {
    let md_sample = "\
# aider chat started at 2026-10-31 15:00:00
> Aider v0.83.0
> Main model: claude-opus-4-6 with architect edit format
#### First line of user prompt
#### Second line of user prompt

Paragraph one of assistant reply.

Paragraph two of assistant reply.

> Tokens: 1.25M sent, 250k cache write, 800k cache hit, 2.5k received. Cost: $1.85 message, $1.85 session.
> Tokens: malformed unparseable token summary
#### /reset
#### /model gpt-4o-mini
#### Follow-up prompt after reset
Short answer.
> Tokens: 1,240 sent, 95 received.
";

    let out = parse_aider_history_reader(Cursor::new(md_sample.as_bytes()), None)
        .expect("parse synthetic history sample");
    assert_eq!(out.diagnostics.issues.len(), 1);
    assert_eq!(out.sessions.len(), 1);
    let turns = &out.sessions[0].records;
    assert_eq!(turns.len(), 2);

    assert_eq!(turns[0].model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(turns[0].usage.prompt_tokens(), 1_250_000);
    assert_eq!(turns[0].usage.cache_write, 250_000);
    assert_eq!(turns[0].usage.cache_read, 800_000);
    assert_eq!(turns[0].usage.fresh_in, 200_000);
    assert_eq!(turns[0].usage.output, 2_500);
    assert_eq!(turns[0].usage.confidence, Confidence::Estimated);
    assert_eq!(
        turns[0].added[0].bytes,
        "First line of user prompt\nSecond line of user prompt".len() as u64
    );
    assert_eq!(
        turns[0].added[1].bytes,
        "Paragraph one of assistant reply.\n\nParagraph two of assistant reply.".len() as u64
    );

    assert!(turns[1].hints.compaction);
    assert_eq!(turns[1].model.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(turns[1].usage.prompt_tokens(), 1_240);
    assert_eq!(turns[1].usage.output, 95);
    assert_eq!(turns[1].usage.confidence, Confidence::Estimated);
    assert_eq!(turns[1].usage.agent_reported_cost_usd, None);

    let analytics_sample = "\
not-json-at-all
{\"event\":\"launched\",\"properties\":{\"version\":\"0.83.0\"},\"time\":1793458800,\"user_id\":\"u1\"}
{\"event\":\"message_send\",\"properties\":{\"main_model\":\"gpt-4o\",\"prompt_tokens\":4500,\"completion_tokens\":210,\"cost\":0.0},\"time\":\"2026-10-31T15:00:05Z\",\"user_id\":\"u1\"}
";
    let ana_out = parse_aider_analytics_reader(Cursor::new(analytics_sample.as_bytes()), None)
        .expect("parse synthetic analytics sample");
    assert_eq!(ana_out.diagnostics.issues.len(), 1);
    assert_eq!(ana_out.sessions.len(), 1);
    assert_eq!(ana_out.sessions[0].records.len(), 1);
    assert_eq!(
        ana_out.sessions[0].records[0].usage.confidence,
        Confidence::Estimated
    );
    assert_eq!(
        ana_out.sessions[0].records[0].usage.agent_reported_cost_usd,
        None
    );
}

#[test]
fn aider_discovery_finds_enclosing_repo_history_and_extracts_opt_in_cwds() {
    let env = TestEnv::new();
    let repo_root = env.home_dir().join("work/synth-repo");
    let nested_cwd = repo_root.join("crates/sub");
    fs::create_dir_all(repo_root.join(".git")).expect("create .git marker");
    fs::create_dir_all(&nested_cwd).expect("create nested cwd");

    // Also place an unrelated sibling directory under $HOME to verify discovery
    // never walks downward across $HOME.
    let unrelated_repo = env.home_dir().join("other-repo");
    fs::create_dir_all(&unrelated_repo).expect("create unrelated repo");
    let sample_md = "\
# aider chat started at 2026-10-31 16:00:00
> Main model: claude-sonnet-4-5
#### Explain module layout
Module layout details.
> Tokens: 3.1k sent, 180 received.
";
    let repo_hist = repo_root.join(".aider.chat.history.md");
    fs::write(&repo_hist, sample_md).expect("write repo history");
    fs::write(unrelated_repo.join(".aider.chat.history.md"), sample_md)
        .expect("write unrelated repo history");

    let catalog = PathCatalog::new(Some(env.home_dir().to_path_buf()), None, None)
        .with_current_dir(Some(nested_cwd));
    let discovered = catalog
        .discover_aider_histories()
        .expect("discover enclosing repo history");
    assert_eq!(
        discovered.len(),
        1,
        "must find enclosing repo history without scanning sibling $HOME directories"
    );
    assert_eq!(discovered[0].path, repo_hist);

    let parsed = parse_aider_history_file(&repo_hist).expect("parse repo history");
    let cwds = extract_aider_session_cwds(&repo_hist).expect("extract opt-in aider cwds");
    assert_eq!(
        cwds.get(&parsed.sessions[0].session_id).map(String::as_str),
        repo_root.to_str()
    );
}
