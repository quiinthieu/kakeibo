//! Unit and property tests for `kakeibo-core` model, usage normalisation, and
//! `kakeibo.ledger/1` NDJSON codec (`PLAN.md` §4, §5, §10 T03).

use std::str::FromStr;

use kakeibo_core::ledger::{
    decode_line, decode_ndjson, encode_line, encode_ndjson, LedgerError, LEDGER_SCHEMA,
};
use kakeibo_core::{
    AgentId, BlockKind, CacheWriteSplit, Category, Confidence, ContentBlock, DisjointUsageInput,
    InclusiveUsageInput, Timestamp, TurnHints, TurnRecord, Usage, UsageError,
};
use proptest::prelude::*;

fn arb_agent_id() -> impl Strategy<Value = AgentId> {
    prop_oneof![
        Just(AgentId::ClaudeCode),
        Just(AgentId::Codex),
        Just(AgentId::GeminiCli),
        Just(AgentId::Opencode),
        Just(AgentId::Aider),
        Just(AgentId::Cursor),
    ]
}

fn arb_confidence() -> impl Strategy<Value = Confidence> {
    prop_oneof![
        Just(Confidence::Measured),
        Just(Confidence::Estimated),
        Just(Confidence::NotRecorded),
    ]
}

fn arb_block_kind() -> impl Strategy<Value = BlockKind> {
    prop_oneof![
        Just(BlockKind::UserPrompt),
        Just(BlockKind::AssistantText),
        Just(BlockKind::AssistantThinking),
        Just(BlockKind::ToolUse),
        Just(BlockKind::ToolResult),
        Just(BlockKind::SubagentResult),
        Just(BlockKind::CompactionSummary),
        Just(BlockKind::SystemContext),
    ]
}

fn arb_timestamp() -> impl Strategy<Value = Timestamp> {
    // Span 2000-01-01..2100-01-01 with either zero or subsecond nanoseconds.
    (
        946_684_800_i64..4_102_444_800_i64,
        prop_oneof![Just(0_i128), (1_i128..999_999_999_i128)],
    )
        .prop_map(|(secs, nanos)| {
            let total_nanos = i128::from(secs) * 1_000_000_000 + nanos;
            Timestamp::from_unix_timestamp_nanos(total_nanos).expect("valid synthetic timestamp")
        })
}

fn arb_clean_string() -> impl Strategy<Value = String> {
    "[A-Za-z0-9_.:/-]{1,32}".prop_map(|s| s)
}

fn arb_finite_rounded_f64(max: u32) -> impl Strategy<Value = f64> {
    // Quarter-point increments (e.g. 12.25) have exact IEEE-754 & decimal round-trips.
    (0_u32..=(max * 4)).prop_map(|q| f64::from(q) / 4.0)
}

fn arb_usage() -> impl Strategy<Value = Usage> {
    (
        0_u64..5_000_000,
        0_u64..5_000_000,
        0_u64..5_000_000,
        0_u64..500_000,
        proptest::option::of(0_u64..250_000),
        arb_confidence(),
        proptest::option::of(arb_clean_string()),
        proptest::option::of((0_u64..1_000_000, 0_u64..1_000_000)),
        proptest::option::of(arb_finite_rounded_f64(500)),
    )
        .prop_map(
            |(
                fresh_in,
                cache_write,
                cache_read,
                output,
                reasoning,
                confidence,
                estimation_method,
                split,
                agent_reported_cost_usd,
            )| Usage {
                fresh_in,
                cache_write,
                cache_read,
                output,
                reasoning,
                confidence,
                estimation_method,
                cache_write_split: split.map(|(ephemeral_5m, ephemeral_1h)| CacheWriteSplit {
                    ephemeral_5m,
                    ephemeral_1h,
                }),
                agent_reported_cost_usd,
            },
        )
}

fn arb_content_block() -> impl Strategy<Value = ContentBlock> {
    (
        arb_block_kind(),
        0_u64..500_000,
        0_u64..2_000_000,
        proptest::option::of(arb_clean_string()),
        proptest::option::of("[0-9a-f]{16}"),
        any::<bool>(),
    )
        .prop_map(
            |(kind, est_tokens, bytes, tool, key_hash, is_error)| ContentBlock {
                kind,
                est_tokens,
                bytes,
                tool,
                key_hash,
                is_error,
            },
        )
}

fn arb_turn_hints() -> impl Strategy<Value = TurnHints> {
    (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        proptest::option::of(arb_finite_rounded_f64(100)),
        proptest::option::of(8_192_u64..2_000_000),
    )
        .prop_map(
            |(
                resume_start,
                compaction,
                sidechain,
                api_error,
                rate_limit_used_percent,
                context_window_override,
            )| TurnHints {
                resume_start,
                compaction,
                sidechain,
                api_error,
                rate_limit_used_percent,
                context_window_override,
            },
        )
}

fn arb_turn_record() -> impl Strategy<Value = TurnRecord> {
    (
        arb_agent_id(),
        arb_clean_string(),
        proptest::option::of(arb_clean_string()),
        1_u32..10_000,
        arb_timestamp(),
        proptest::option::of(arb_clean_string()),
        proptest::option::of("[0-9a-f]{16}"),
        arb_usage(),
        proptest::collection::vec(arb_content_block(), 0..6),
        arb_turn_hints(),
    )
        .prop_map(
            |(
                agent,
                session_id,
                parent_session_id,
                turn,
                ts,
                model,
                cwd_hash,
                usage,
                added,
                hints,
            )| TurnRecord {
                agent,
                session_id,
                parent_session_id,
                turn,
                ts,
                model,
                cwd_hash,
                usage,
                added,
                hints,
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn disjoint_usage_preserves_prompt_sum(
        // Bound each component to u64::MAX / 4 so their sum never overflows u64.
        input_tokens in 0_u64..(u64::MAX / 4),
        cache_write_tokens in 0_u64..(u64::MAX / 4),
        cache_read_tokens in 0_u64..(u64::MAX / 4),
        output_tokens in any::<u64>(),
        reasoning_tokens in proptest::option::of(any::<u64>()),
    ) {
        let expected_p = input_tokens + cache_write_tokens + cache_read_tokens;
        let mut raw = DisjointUsageInput::measured(
            input_tokens,
            cache_write_tokens,
            cache_read_tokens,
            output_tokens,
        );
        raw.reasoning_tokens = reasoning_tokens;

        let usage = Usage::from_disjoint(raw).expect("bounded disjoint inputs must succeed");

        prop_assert_eq!(usage.fresh_in, input_tokens);
        prop_assert_eq!(usage.cache_write, cache_write_tokens);
        prop_assert_eq!(usage.cache_read, cache_read_tokens);
        prop_assert_eq!(usage.output, output_tokens);
        prop_assert_eq!(usage.reasoning, reasoning_tokens);
        prop_assert_eq!(usage.fresh_in + usage.cache_write + usage.cache_read, expected_p);
        prop_assert_eq!(usage.prompt_tokens(), expected_p);
        prop_assert_eq!(usage.prompt_tokens_checked(), Some(expected_p));
    }

    #[test]
    fn inclusive_usage_checked_preserves_prompt_sum_and_never_adds_cache_to_input(
        (input_tokens, cache_write_tokens, cache_read_tokens) in (0_u64..(u64::MAX / 2))
            .prop_flat_map(|p| {
                (0_u64..=p).prop_flat_map(move |cw| {
                    (0_u64..=(p - cw)).prop_map(move |cr| (p, cw, cr))
                })
            }),
        output_tokens in any::<u64>(),
        reasoning_tokens in proptest::option::of(any::<u64>()),
    ) {
        let mut raw = InclusiveUsageInput::measured(
            input_tokens,
            cache_write_tokens,
            cache_read_tokens,
            output_tokens,
        );
        raw.reasoning_tokens = reasoning_tokens;

        let usage = Usage::from_inclusive_checked(raw)
            .expect("valid inclusive counts must normalise cleanly");

        // Total prompt P is `input_tokens`; cached tokens are inside `input_tokens`, never added.
        prop_assert_eq!(usage.fresh_in + usage.cache_write + usage.cache_read, input_tokens);
        prop_assert_eq!(usage.prompt_tokens(), input_tokens);
        prop_assert_eq!(usage.prompt_tokens_checked(), Some(input_tokens));
        prop_assert_eq!(
            usage.fresh_in,
            input_tokens - cache_write_tokens - cache_read_tokens
        );
        prop_assert_eq!(usage.cache_write, cache_write_tokens);
        prop_assert_eq!(usage.cache_read, cache_read_tokens);
    }

    #[test]
    fn inclusive_usage_clamped_preserves_prompt_sum_on_arbitrary_hostile_counts(
        input_tokens in any::<u64>(),
        cache_write_tokens in any::<u64>(),
        cache_read_tokens in any::<u64>(),
        output_tokens in any::<u64>(),
    ) {
        let raw = InclusiveUsageInput::measured(
            input_tokens,
            cache_write_tokens,
            cache_read_tokens,
            output_tokens,
        );
        let exceeds = cache_read_tokens
            .checked_add(cache_write_tokens)
            .is_none_or(|sum| sum > input_tokens);

        let (usage, was_clamped) = Usage::from_inclusive_clamped(raw);

        prop_assert_eq!(was_clamped, exceeds);
        prop_assert_eq!(usage.prompt_tokens_checked(), Some(input_tokens));
        prop_assert_eq!(usage.fresh_in + usage.cache_write + usage.cache_read, input_tokens);
        prop_assert_eq!(usage.prompt_tokens(), input_tokens);
    }

    #[test]
    fn turn_record_ndjson_line_and_batch_round_trip(
        records in proptest::collection::vec(arb_turn_record(), 0..12),
    ) {
        for record in &records {
            let line = encode_line(record).expect("valid TurnRecord must encode");
            prop_assert!(!line.contains('\n'));
            prop_assert!(!line.contains('\r'));
            prop_assert!(line.contains("\"schema\":\"kakeibo.ledger/1\""));

            let decoded = decode_line(&line).expect("encoded line must decode");
            prop_assert_eq!(&decoded, record);
        }

        let batch = encode_ndjson(&records).expect("batch NDJSON must encode");
        let non_empty_lines = batch.lines().filter(|l| !l.trim().is_empty()).count();
        prop_assert_eq!(non_empty_lines, records.len());

        let decoded_batch = decode_ndjson(&batch).expect("batch NDJSON must decode");
        prop_assert_eq!(decoded_batch, records);
    }
}

#[test]
fn timestamp_normalises_offsets_to_canonical_utc() {
    let utc = Timestamp::parse_rfc3339("2026-10-31T09:00:04Z").expect("valid UTC timestamp");
    let bangkok =
        Timestamp::parse_rfc3339("2026-10-31T16:00:04+07:00").expect("valid +07:00 timestamp");
    let ny = Timestamp::parse_rfc3339("2026-10-31T05:00:04-04:00").expect("valid -04:00 timestamp");

    assert_eq!(utc, bangkok);
    assert_eq!(utc, ny);
    assert_eq!(bangkok.to_string(), "2026-10-31T09:00:04Z");
    assert_eq!(ny.to_string(), "2026-10-31T09:00:04Z");

    let subsec =
        Timestamp::parse_rfc3339("2026-10-31T16:00:04.125+07:00").expect("valid subsec timestamp");
    assert_eq!(subsec.to_string(), "2026-10-31T09:00:04.125Z");
    assert_eq!(subsec.duration_seconds_since(utc), 0);

    let later = Timestamp::parse_rfc3339("2026-10-31T09:05:14Z").expect("later timestamp");
    assert_eq!(later.duration_seconds_since(utc), 310);

    assert!(Timestamp::parse_rfc3339("not-a-timestamp").is_err());
    assert!(Timestamp::from_str("2026-13-40T99:00:00Z").is_err());
}

#[test]
fn enum_wire_representations_match_ledger_schema_doc() {
    let expected_agents = [
        (AgentId::ClaudeCode, "claude-code"),
        (AgentId::Codex, "codex"),
        (AgentId::GeminiCli, "gemini-cli"),
        (AgentId::Opencode, "opencode"),
        (AgentId::Aider, "aider"),
        (AgentId::Cursor, "cursor"),
    ];
    for (agent, slug) in expected_agents {
        assert_eq!(agent.as_str(), slug);
        assert_eq!(AgentId::from_str(slug).unwrap(), agent);
        assert_eq!(
            serde_json::to_string(&agent).unwrap(),
            format!("\"{slug}\"")
        );
    }
    assert!(AgentId::from_str("unknown-agent").is_err());

    let expected_categories = [
        (Category::FixedOverhead, "fixed_overhead"),
        (Category::ResentHistory, "resent_history"),
        (Category::ToolOutput, "tool_output"),
        (Category::Retries, "retries"),
        (Category::UserInput, "user_input"),
        (Category::AssistantOutput, "assistant_output"),
        (Category::Subagents, "subagents"),
        (Category::Compaction, "compaction"),
        (Category::Unattributed, "unattributed"),
    ];
    assert_eq!(Category::ALL.len(), expected_categories.len());
    for (cat, wire) in expected_categories {
        assert_eq!(cat.as_str(), wire);
        assert_eq!(serde_json::to_string(&cat).unwrap(), format!("\"{wire}\""));
    }

    assert_eq!(BlockKind::ALL.len(), 8);
    assert_eq!(Confidence::Measured.as_str(), "measured");
    assert_eq!(Confidence::Estimated.as_str(), "estimated");
    assert_eq!(Confidence::NotRecorded.as_str(), "not_recorded");
}

#[test]
fn usage_constructors_reject_overflow_and_invalid_counts() {
    let overflow = DisjointUsageInput::measured(u64::MAX, 1, 0, 10);
    assert!(matches!(
        Usage::from_disjoint(overflow),
        Err(UsageError::PromptOverflow { .. })
    ));

    let cached_exceeds = InclusiveUsageInput::measured(1_000, 300, 800, 50);
    assert!(matches!(
        Usage::from_inclusive_checked(cached_exceeds),
        Err(UsageError::CachedExceedsInput { .. })
    ));

    let mut bad_cost = DisjointUsageInput::measured(10, 0, 0, 5);
    bad_cost.agent_reported_cost_usd = Some(f64::NAN);
    assert!(matches!(
        Usage::from_disjoint(bad_cost),
        Err(UsageError::NonFiniteCost { .. })
    ));
}

#[test]
fn ledger_decoder_enforces_schema_and_ignores_unknown_additive_fields() {
    assert_eq!(LEDGER_SCHEMA, "kakeibo.ledger/1");

    // 1. Missing schema
    let missing = r#"{"agent":"codex","session_id":"s1","turn":1,"ts":"2026-10-31T10:00:03Z","usage":{"fresh_in":10,"cache_write":0,"cache_read":0,"output":5,"confidence":"measured"},"added":[],"hints":{}}"#;
    assert_eq!(
        decode_line(missing),
        Err(LedgerError::MissingSchema { line: None })
    );

    // 2. Wrong schema version on line 2 (with leading blank line on line 1)
    let wrong_version = format!(
        "\n{}\n",
        r#"{"schema":"kakeibo.ledger/2","agent":"codex","session_id":"s1","turn":1,"ts":"2026-10-31T10:00:03Z","usage":{"fresh_in":10,"cache_write":0,"cache_read":0,"output":5,"confidence":"measured"},"added":[],"hints":{}}"#
    );
    assert_eq!(
        decode_ndjson(&wrong_version),
        Err(LedgerError::UnsupportedSchema {
            found: "kakeibo.ledger/2".to_string(),
            expected: LEDGER_SCHEMA,
            line: Some(2),
        })
    );

    // 3. Malformed JSON reports exact 1-based line number
    let malformed = "\n\n{not-json}\n";
    match decode_ndjson(malformed) {
        Err(LedgerError::Json { line: Some(3), .. }) => {}
        other => panic!("expected Json error at line 3, got {other:?}"),
    }

    // 4. Unknown additive fields within kakeibo.ledger/1 are ignored cleanly
    let forward_compatible = r#"{"schema":"kakeibo.ledger/1","future_top_level_field":true,"agent":"claude-code","session_id":"57e1faba-6510-4204-b4ab-d143feb24055","turn":1,"ts":"2026-10-31T09:00:04Z","model":"claude-sonnet-4-5","cwd_hash":"a9f1e4c0b2d87136","usage":{"fresh_in":332,"cache_write":9400,"cache_read":0,"output":84,"confidence":"measured","future_usage_metric":42,"cache_write_split":{"ephemeral_5m":9400,"ephemeral_1h":0}},"added":[{"kind":"user_prompt","est_tokens":24,"bytes":96,"future_block_tag":"ok"},{"kind":"tool_use","est_tokens":14,"bytes":53,"tool":"Read","key_hash":"4c8e21b90f3d7a12"}],"hints":{"resume_start":false,"compaction":false,"sidechain":false,"api_error":false,"future_hint":1}}"#;
    let decoded = decode_line(forward_compatible).expect("unknown additive fields must be ignored");
    assert_eq!(decoded.agent, AgentId::ClaudeCode);
    assert_eq!(decoded.usage.prompt_tokens(), 9_732);
    assert_eq!(decoded.added.len(), 2);

    // 5. Non-finite floats in TurnRecord are rejected on encode_line
    let mut bad_record = decoded;
    bad_record.hints.rate_limit_used_percent = Some(f64::INFINITY);
    assert!(matches!(
        encode_line(&bad_record),
        Err(LedgerError::NonFiniteNumber {
            field: "hints.rate_limit_used_percent",
            ..
        })
    ));
}

#[test]
fn ledger_schema_doc_examples_decode_and_round_trip() {
    let doc_examples = concat!(
        r#"{"schema":"kakeibo.ledger/1","agent":"claude-code","session_id":"57e1faba-6510-4204-b4ab-d143feb24055","turn":1,"ts":"2026-10-31T09:00:04Z","model":"claude-sonnet-4-5","cwd_hash":"a9f1e4c0b2d87136","usage":{"fresh_in":332,"cache_write":9400,"cache_read":0,"output":84,"confidence":"measured","cache_write_split":{"ephemeral_5m":9400,"ephemeral_1h":0}},"added":[{"kind":"user_prompt","est_tokens":24,"bytes":96},{"kind":"assistant_text","est_tokens":28,"bytes":112},{"kind":"tool_use","est_tokens":14,"bytes":53,"tool":"Read","key_hash":"4c8e21b90f3d7a12"}],"hints":{"resume_start":false,"compaction":false,"sidechain":false,"api_error":false}}"#,
        "\n",
        r#"{"schema":"kakeibo.ledger/1","agent":"codex","session_id":"3304d23d-b2a8-4503-baed-7332b91656d8","turn":1,"ts":"2026-10-31T10:00:03Z","model":"gpt-5-codex","cwd_hash":"7b3d10c8e5a24f91","usage":{"fresh_in":3100,"cache_write":0,"cache_read":8960,"output":190,"reasoning":92,"confidence":"measured"},"added":[{"kind":"user_prompt","est_tokens":34,"bytes":136},{"kind":"tool_use","est_tokens":53,"bytes":210,"tool":"apply_patch","key_hash":"9e01a74c6b2f8d55"}],"hints":{"resume_start":false,"compaction":false,"sidechain":false,"api_error":false,"rate_limit_used_percent":11.0,"context_window_override":272000}}"#,
        "\n"
    );

    let records = decode_ndjson(doc_examples).expect("doc example NDJSON must decode");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].usage.prompt_tokens(), 9_732);
    assert_eq!(records[1].usage.prompt_tokens(), 12_060);

    let reencoded = encode_ndjson(&records).expect("re-encode doc examples");
    let round_tripped = decode_ndjson(&reencoded).expect("round-trip doc examples");
    assert_eq!(round_tripped, records);
}
