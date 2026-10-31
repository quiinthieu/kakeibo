//! TDD unit and `proptest` reconciliation tests for `kakeibo-core::attrib` (`PLAN.md` §6.1, §9, §10 T06).

use kakeibo_core::attrib::{
    attribute_session, attribute_session_with_subagents, estimate_tokens_from_bytes, TokenSplit,
    ESTIMATION_METHOD_SCALED_BYTES_DIV4,
};
use kakeibo_core::{
    AgentId, BlockKind, Category, Confidence, ContentBlock, Timestamp, TurnHints, TurnRecord, Usage,
};
use proptest::prelude::*;

fn make_ts(offset_secs: i64) -> Timestamp {
    Timestamp::from_unix_timestamp(1_793_437_200 + offset_secs).expect("valid timestamp")
}

fn make_usage(fresh_in: u64, cache_write: u64, cache_read: u64, output: u64) -> Usage {
    Usage {
        fresh_in,
        cache_write,
        cache_read,
        output,
        reasoning: None,
        confidence: Confidence::Measured,
        estimation_method: None,
        cache_write_split: None,
        agent_reported_cost_usd: None,
    }
}

fn make_block(
    kind: BlockKind,
    bytes: u64,
    tool: Option<&str>,
    key_hash: Option<&str>,
    is_error: bool,
) -> ContentBlock {
    ContentBlock {
        kind,
        est_tokens: estimate_tokens_from_bytes(bytes),
        bytes,
        tool: tool.map(str::to_string),
        key_hash: key_hash.map(str::to_string),
        is_error,
    }
}

fn assert_full_reconciliation(turns: &[TurnRecord], subagents: &[&[TurnRecord]]) {
    let attrib = attribute_session_with_subagents(turns, subagents);
    assert_eq!(attrib.turns.len(), turns.len());
    assert_eq!(attrib.categories.sum(), attrib.totals);

    let mut expected_session_totals = TokenSplit::ZERO;
    for (tr, ta) in turns.iter().zip(attrib.turns.iter()) {
        let measured = TokenSplit::from_usage(&tr.usage);
        assert_eq!(ta.turn, tr.turn);
        assert_eq!(ta.measured, measured);
        assert_eq!(
            ta.categories.sum(),
            measured,
            "per-turn category sum must reconcile identically to measured usage on turn {}",
            tr.turn
        );
        expected_session_totals.add_assign(measured);
    }

    for sub in subagents {
        for tr in *sub {
            expected_session_totals.add_assign(TokenSplit::from_usage(&tr.usage));
        }
    }
    assert_eq!(attrib.totals, expected_session_totals);
}

#[test]
fn empty_session_attributes_to_zero_cleanly() {
    let attrib = attribute_session(&[]);
    assert_eq!(attrib.fixed_overhead_tokens, 0);
    assert_eq!(attrib.totals, TokenSplit::ZERO);
    assert_eq!(attrib.categories.sum(), TokenSplit::ZERO);
    assert_eq!(attrib.totals_confidence, Confidence::Measured);
    assert_eq!(
        attrib.estimation_method,
        ESTIMATION_METHOD_SCALED_BYTES_DIV4
    );
    assert!(attrib.turns.is_empty());
    assert!(attrib.carried_blocks.is_empty());
    assert!(attrib.subagents.is_empty());
}

#[test]
fn hand_computed_tool_bloat_carried_cost_and_compaction_eviction_exact() {
    // 7-turn hand-computed session:
    // Turn 1: P(1) = 10,020 (FixedOverhead = 10,000 cache_write, u1 = 20 fresh_in), out = 50 (a1=30, tool_use=20)
    // Turn 2: P(2) = 50,070 (cache_read = 10,020, cache_write = 40,050) -> 160 KB (40,000 tok) Bash ToolResult
    // Turn 3: P(3) = 50,120 (cache_read = 50,070, cache_write = 30, fresh_in = 20) -> carry #1 (40,000 cache_read)
    // Turn 4: P(4) = 50,170 (cache_read = 0, cache_write = 50,120, fresh_in = 50) -> carry #2 (40,000 cache_write)
    // Turn 5: P(5) = 50,210 (cache_read = 50,170, cache_write = 0, fresh_in = 40) -> carry #3 (40,000 cache_read)
    // Turn 6: COMPACTION! P(6) = 10,520 (cache_read = 10,000, cache_write = 500, fresh_in = 20) -> evicts bloat!
    // Turn 7: Post-compaction P(7) = 10,560 (cache_read = 10,520, fresh_in = 40) -> bloat never carried again!
    let turns = vec![
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-bloat-hand".to_string(),
            parent_session_id: None,
            turn: 1,
            ts: make_ts(0),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(20, 10_000, 0, 50),
            added: vec![
                make_block(BlockKind::UserPrompt, 80, None, None, false),
                make_block(BlockKind::AssistantText, 120, None, None, false),
                make_block(
                    BlockKind::ToolUse,
                    80,
                    Some("Bash"),
                    Some("0011223344556677"),
                    false,
                ),
            ],
            hints: TurnHints::default(),
        },
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-bloat-hand".to_string(),
            parent_session_id: None,
            turn: 2,
            ts: make_ts(8),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(0, 40_050, 10_020, 20),
            added: vec![
                make_block(
                    BlockKind::ToolResult,
                    160_000,
                    Some("Bash"),
                    Some("0011223344556677"),
                    false,
                ),
                make_block(BlockKind::AssistantText, 80, None, None, false),
            ],
            hints: TurnHints::default(),
        },
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-bloat-hand".to_string(),
            parent_session_id: None,
            turn: 3,
            ts: make_ts(16),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(20, 30, 50_070, 20),
            added: vec![
                make_block(BlockKind::UserPrompt, 120, None, None, false),
                make_block(BlockKind::AssistantText, 80, None, None, false),
            ],
            hints: TurnHints::default(),
        },
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-bloat-hand".to_string(),
            parent_session_id: None,
            turn: 4,
            ts: make_ts(746),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(50, 50_120, 0, 20),
            added: vec![
                make_block(BlockKind::UserPrompt, 120, None, None, false),
                make_block(BlockKind::AssistantText, 80, None, None, false),
            ],
            hints: TurnHints::default(),
        },
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-bloat-hand".to_string(),
            parent_session_id: None,
            turn: 5,
            ts: make_ts(758),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(40, 0, 50_170, 25),
            added: vec![
                make_block(BlockKind::UserPrompt, 80, None, None, false),
                make_block(BlockKind::AssistantText, 100, None, None, false),
            ],
            hints: TurnHints::default(),
        },
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-bloat-hand".to_string(),
            parent_session_id: None,
            turn: 6,
            ts: make_ts(780),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(20, 500, 10_000, 20),
            added: vec![
                make_block(BlockKind::CompactionSummary, 2_000, None, None, false),
                make_block(BlockKind::UserPrompt, 80, None, None, false),
                make_block(BlockKind::AssistantText, 80, None, None, false),
            ],
            hints: TurnHints {
                compaction: true,
                ..TurnHints::default()
            },
        },
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-bloat-hand".to_string(),
            parent_session_id: None,
            turn: 7,
            ts: make_ts(795),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(40, 0, 10_520, 30),
            added: vec![
                make_block(BlockKind::UserPrompt, 80, None, None, false),
                make_block(BlockKind::AssistantText, 120, None, None, false),
            ],
            hints: TurnHints::default(),
        },
    ];

    assert_full_reconciliation(&turns, &[]);
    let attrib = attribute_session(&turns);
    assert_eq!(attrib.fixed_overhead_tokens, 10_000);

    // Inspect the 160 KB Bash ToolResult carry record.
    let bloat = attrib
        .carried_blocks
        .iter()
        .find(|b| b.kind == BlockKind::ToolResult && b.tool.as_deref() == Some("Bash"))
        .expect("Bash ToolResult carry record must exist");

    assert_eq!(bloat.source_turn, 2);
    assert_eq!(bloat.born_turn, Some(2));
    assert_eq!(bloat.evicted_before_turn, Some(6));
    assert_eq!(bloat.bytes, 160_000);
    assert_eq!(bloat.est_tokens, 40_000);
    assert_eq!(bloat.scaled_prompt_tokens, 40_000);
    assert_eq!(
        bloat.initial_split,
        TokenSplit {
            fresh_in: 0,
            cache_write: 40_000,
            cache_read: 0,
            output: 0,
        }
    );
    assert_eq!(bloat.carried_turns, 3);
    assert_eq!(
        bloat.carried_split,
        TokenSplit {
            fresh_in: 0,
            cache_write: 40_000,
            cache_read: 80_000,
            output: 0,
        }
    );
    assert_eq!(bloat.carried_prompt_tokens(), 120_000);
    assert_eq!(bloat.lifetime_prompt_tokens(), 160_000);

    // Turn 1 exact category breakdown:
    let t1 = &attrib.turns[0];
    assert_eq!(
        t1.categories.get(Category::FixedOverhead),
        TokenSplit {
            fresh_in: 0,
            cache_write: 10_000,
            cache_read: 0,
            output: 0,
        }
    );
    assert_eq!(
        t1.categories.get(Category::UserInput),
        TokenSplit {
            fresh_in: 20,
            cache_write: 0,
            cache_read: 0,
            output: 0,
        }
    );
    assert_eq!(
        t1.categories.get(Category::AssistantOutput),
        TokenSplit {
            fresh_in: 0,
            cache_write: 0,
            cache_read: 0,
            output: 50,
        }
    );

    // Turn 4 (cold cache_write miss): ResentHistory must be 0, while carried ToolOutput
    // (`tool_use = 20` + `tool_result = 40,000`) rewrites `40,020` cache_write tokens!
    let t4 = &attrib.turns[3];
    assert_eq!(t4.categories.get(Category::ResentHistory), TokenSplit::ZERO);
    assert_eq!(
        t4.categories.get(Category::ToolOutput),
        TokenSplit {
            fresh_in: 0,
            cache_write: 40_020,
            cache_read: 0,
            output: 0,
        }
    );

    // Turn 6 (compaction): pre-Turn-6 conversation evicted, ResentHistory == 0,
    // CompactionSummary + UserPrompt split the 520 non-fixed tokens proportionally (500 / 20).
    let t6 = &attrib.turns[5];
    assert_eq!(t6.categories.get(Category::ResentHistory), TokenSplit::ZERO);
    assert_eq!(
        t6.categories.get(Category::FixedOverhead),
        TokenSplit {
            fresh_in: 0,
            cache_write: 0,
            cache_read: 10_000,
            output: 0,
        }
    );
    assert_eq!(t6.categories.get(Category::Compaction).prompt_tokens(), 500);
    assert_eq!(t6.categories.get(Category::UserInput).prompt_tokens(), 20);

    // Turn 7 (post-compaction): ResentHistory carries ONLY Turn 6's 520 conversation tokens!
    let t7 = &attrib.turns[6];
    assert_eq!(
        t7.categories.get(Category::ResentHistory),
        TokenSplit {
            fresh_in: 0,
            cache_write: 0,
            cache_read: 520,
            output: 0,
        }
    );
}

#[test]
fn unattributed_captures_unlogged_prompt_jumps_respecting_byte_ceiling() {
    let turns = vec![
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-unattrib".to_string(),
            parent_session_id: None,
            turn: 1,
            ts: make_ts(0),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(20, 10_000, 0, 10),
            added: vec![
                make_block(BlockKind::UserPrompt, 80, None, None, false), // est 20
                make_block(BlockKind::AssistantText, 40, None, None, false), // est 10, ceiling 40
            ],
            hints: TurnHints::default(),
        },
        // Turn 2: user types 2-byte "ok" (est 1, ceiling 2), while an unlogged MCP schema
        // injects +25,000 prompt tokens (P jumps from 10,020 to 35,020)!
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-unattrib".to_string(),
            parent_session_id: None,
            turn: 2,
            ts: make_ts(5),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(25_000, 0, 10_020, 10),
            added: vec![
                make_block(BlockKind::UserPrompt, 2, None, None, false), // "ok": ceiling 2
                make_block(BlockKind::AssistantText, 40, None, None, false),
            ],
            hints: TurnHints::default(),
        },
        // Turn 3: normal +50 growth (40B prior assistant = 10 + 160B user prompt = 40)
        // with full cache_read = 35,020 -> new_unattributed_prompt_tokens must be 0!
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-unattrib".to_string(),
            parent_session_id: None,
            turn: 3,
            ts: make_ts(10),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(50, 0, 35_020, 15),
            added: vec![make_block(BlockKind::UserPrompt, 160, None, None, false)],
            hints: TurnHints::default(),
        },
    ];

    assert_full_reconciliation(&turns, &[]);
    let attrib = attribute_session(&turns);

    let t2 = &attrib.turns[1];
    // Turn 1's 40B AssistantText caps at 40 tokens, Turn 2's 2B "ok" caps at 2 tokens => 24,958 Unattributed!
    assert_eq!(t2.categories.get(Category::UserInput).prompt_tokens(), 2);
    assert_eq!(
        t2.categories.get(Category::AssistantOutput).prompt_tokens(),
        40
    );
    assert_eq!(t2.new_unattributed_prompt_tokens, 24_958);
    assert_eq!(
        t2.categories.get(Category::Unattributed),
        TokenSplit {
            fresh_in: 24_958,
            cache_write: 0,
            cache_read: 0,
            output: 0,
        }
    );

    let t3 = &attrib.turns[2];
    assert_eq!(t3.new_unattributed_prompt_tokens, 0);
    assert_eq!(t3.categories.get(Category::Unattributed), TokenSplit::ZERO);
    // Carried history on Turn 3 = 20 (u1) + 40 (a1) + 2 (u2) + 24,958 (unattrib_2) = 25,020!
    assert_eq!(
        t3.categories.get(Category::ResentHistory).cache_read,
        25_020
    );
}

#[test]
fn retries_classify_repeated_key_hash_after_failed_tool_result_and_clear_on_success() {
    let key = "deadbeefcafebabe";
    let turns = vec![
        // Turn 1: initial Bash call with `key` (not yet failed -> ToolOutput).
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-retry".to_string(),
            parent_session_id: None,
            turn: 1,
            ts: make_ts(0),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(20, 5_000, 0, 30),
            added: vec![
                make_block(BlockKind::UserPrompt, 80, None, None, false),
                make_block(BlockKind::ToolUse, 120, Some("Bash"), Some(key), false),
            ],
            hints: TurnHints::default(),
        },
        // Turn 2: failed ToolResult (`is_error = true`) for `key` (marks `key` failed),
        // followed immediately by assistant retrying the SAME `key` (`is_retry = true` -> Retries)!
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-retry".to_string(),
            parent_session_id: None,
            turn: 2,
            ts: make_ts(5),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(130, 0, 5_020, 30),
            added: vec![
                make_block(BlockKind::ToolResult, 400, Some("Bash"), Some(key), true),
                make_block(BlockKind::ToolUse, 120, Some("Bash"), Some(key), false),
            ],
            hints: TurnHints::default(),
        },
        // Turn 3: retried call succeeds (`is_error = false`) -> this ToolResult is part of the
        // retry (`is_retry = true`), AND clears `key` from `failed_key_hashes`!
        // Assistant then calls `key` once more later -> must NOT be marked retry!
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-retry".to_string(),
            parent_session_id: None,
            turn: 3,
            ts: make_ts(10),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(130, 0, 5_150, 30),
            added: vec![
                make_block(BlockKind::ToolResult, 400, Some("Bash"), Some(key), false),
                make_block(BlockKind::ToolUse, 120, Some("Bash"), Some(key), false),
            ],
            hints: TurnHints::default(),
        },
        // Turn 4: post-recovery ToolResult for `key` (`is_retry = false` -> ToolOutput)
        // on a turn that hits `hints.api_error = true` (routing O(4) to Retries).
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-retry".to_string(),
            parent_session_id: None,
            turn: 4,
            ts: make_ts(15),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(130, 0, 5_280, 15),
            added: vec![make_block(
                BlockKind::ToolResult,
                400,
                Some("Bash"),
                Some(key),
                false,
            )],
            hints: TurnHints {
                api_error: true,
                ..TurnHints::default()
            },
        },
    ];

    assert_full_reconciliation(&turns, &[]);
    let attrib = attribute_session(&turns);

    let tool_blocks: Vec<_> = attrib
        .carried_blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::ToolUse | BlockKind::ToolResult))
        .collect();
    assert_eq!(tool_blocks.len(), 6);

    // 0: Turn 1 ToolUse (initial) -> not retry
    assert!(!tool_blocks[0].is_retry);
    // 1: Turn 2 ToolResult (first failure) -> not retry
    assert!(!tool_blocks[1].is_retry);
    // 2: Turn 2 ToolUse (repeated after failure) -> RETRY!
    assert!(tool_blocks[2].is_retry);
    // 3: Turn 3 ToolResult (result of retry, succeeds) -> RETRY!
    assert!(tool_blocks[3].is_retry);
    // 4: Turn 3 ToolUse (after recovery) -> not retry!
    assert!(!tool_blocks[4].is_retry);
    // 5: Turn 4 ToolResult (after recovery, though Turn 4 itself has api_error) ->
    // wait: Turn 4 has `api_error: true`, let's verify Turn 2 output (30) and Turn 3 prompt (130) went to Retries!
    assert_eq!(attrib.turns[1].categories.get(Category::Retries).output, 30);
    assert_eq!(
        attrib.turns[2]
            .categories
            .get(Category::Retries)
            .prompt_tokens(),
        130
    );
    assert_eq!(attrib.turns[3].categories.get(Category::Retries).output, 15);
}

#[test]
fn subagent_rollup_and_inline_sidechain_route_to_subagents_category() {
    let parent = vec![
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "parent-1".to_string(),
            parent_session_id: None,
            turn: 1,
            ts: make_ts(0),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(20, 4_000, 0, 40),
            added: vec![
                make_block(BlockKind::UserPrompt, 80, None, None, false),
                make_block(BlockKind::AssistantText, 160, None, None, false),
            ],
            hints: TurnHints::default(),
        },
        // Inline sidechain turn inside parent stream -> routes 100% to Subagents without
        // disturbing parent's carried prompt size (4,020)!
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "parent-1".to_string(),
            parent_session_id: None,
            turn: 2,
            ts: make_ts(3),
            model: Some("claude-haiku-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(100, 1_500, 0, 60),
            added: vec![make_block(BlockKind::UserPrompt, 200, None, None, false)],
            hints: TurnHints {
                sidechain: true,
                ..TurnHints::default()
            },
        },
        // Parent Turn 3 resumes at parent P = 4,020 + 40 (prior parent a1) + 50 (SubagentResult) = 4,110
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "parent-1".to_string(),
            parent_session_id: None,
            turn: 3,
            ts: make_ts(8),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(90, 0, 4_020, 50),
            added: vec![make_block(
                BlockKind::SubagentResult,
                200,
                None,
                None,
                false,
            )],
            hints: TurnHints::default(),
        },
    ];

    let child_subagent = vec![
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "subagent-child-a".to_string(),
            parent_session_id: Some("parent-1".to_string()),
            turn: 1,
            ts: make_ts(4),
            model: Some("claude-haiku-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(50, 2_000, 0, 80),
            added: vec![make_block(BlockKind::UserPrompt, 100, None, None, false)],
            hints: TurnHints {
                sidechain: true,
                ..TurnHints::default()
            },
        },
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "subagent-child-a".to_string(),
            parent_session_id: Some("parent-1".to_string()),
            turn: 2,
            ts: make_ts(6),
            model: Some("claude-haiku-4-5".to_string()),
            cwd_hash: None,
            usage: make_usage(30, 0, 2_050, 40),
            added: vec![make_block(BlockKind::UserPrompt, 80, None, None, false)],
            hints: TurnHints {
                sidechain: true,
                ..TurnHints::default()
            },
        },
    ];

    let child_refs: &[&[TurnRecord]] = &[&child_subagent];
    assert_full_reconciliation(&parent, child_refs);

    let attrib = attribute_session_with_subagents(&parent, child_refs);
    assert_eq!(attrib.subagents.len(), 1);
    assert_eq!(attrib.subagents[0].session_id, "subagent-child-a");
    assert_eq!(attrib.subagents[0].turns, 2);
    assert_eq!(
        attrib.subagents[0].totals,
        TokenSplit {
            fresh_in: 80,
            cache_write: 2_000,
            cache_read: 2_050,
            output: 120,
        }
    );

    // Total Subagents category = inline sidechain (100, 1500, 0, 60)
    //                          + parent Turn 3 SubagentResult block (50, 0, 0, 0)
    //                          + child_subagent (80, 2000, 2050, 120)
    //                          = (230, 3500, 2050, 180).
    assert_eq!(
        attrib.categories.get(Category::Subagents),
        TokenSplit {
            fresh_in: 230,
            cache_write: 3_500,
            cache_read: 2_050,
            output: 180,
        }
    );
}

fn arb_content_block() -> impl Strategy<Value = ContentBlock> {
    (
        prop_oneof![
            Just(BlockKind::UserPrompt),
            Just(BlockKind::AssistantText),
            Just(BlockKind::AssistantThinking),
            Just(BlockKind::ToolUse),
            Just(BlockKind::ToolResult),
            Just(BlockKind::SubagentResult),
            Just(BlockKind::CompactionSummary),
            Just(BlockKind::SystemContext),
        ],
        prop_oneof![Just(0_u64), 1_u64..16, 16_u64..4_000, 4_000_u64..200_000,],
        prop_oneof![
            Just(None),
            Just(Some("0000000000000001")),
            Just(Some("0000000000000002")),
            Just(Some("0000000000000003")),
        ],
        any::<bool>(),
    )
        .prop_map(|(kind, bytes, key_opt, err_flag)| {
            let is_tool = matches!(kind, BlockKind::ToolUse | BlockKind::ToolResult);
            ContentBlock {
                kind,
                est_tokens: estimate_tokens_from_bytes(bytes),
                bytes,
                tool: is_tool.then(|| "SynthTool".to_string()),
                key_hash: if is_tool {
                    key_opt.map(str::to_string)
                } else {
                    None
                },
                is_error: kind == BlockKind::ToolResult && err_flag,
            }
        })
}

fn arb_turn_spec() -> impl Strategy<
    Value = (
        u64,
        u64,
        u64,
        u64,
        Confidence,
        Vec<ContentBlock>,
        bool,
        bool,
        bool,
        bool,
    ),
> {
    (
        prop_oneof![Just(0_u64), 0_u64..50_000],
        prop_oneof![Just(0_u64), 0_u64..50_000],
        prop_oneof![Just(0_u64), 0_u64..100_000],
        prop_oneof![Just(0_u64), 0_u64..10_000],
        prop_oneof![
            Just(Confidence::Measured),
            Just(Confidence::Estimated),
            Just(Confidence::NotRecorded),
        ],
        prop::collection::vec(arb_content_block(), 0..=6),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        cases: 96,
        .. ProptestConfig::default()
    })]

    #[test]
    fn proptest_category_sums_reconcile_to_measured_totals_on_every_turn(
        parent_specs in prop::collection::vec(arb_turn_spec(), 1..=10),
        sub_specs in prop::collection::vec(prop::collection::vec(arb_turn_spec(), 1..=4), 0..=2),
    ) {
        let parent_turns: Vec<TurnRecord> = parent_specs
            .into_iter()
            .enumerate()
            .map(|(idx, (fresh, write, read, out, conf, added, resume, compact, side, api_err))| {
                TurnRecord {
                    agent: AgentId::ClaudeCode,
                    session_id: "prop-parent".to_string(),
                    parent_session_id: None,
                    turn: u32::try_from(idx + 1).unwrap(),
                    ts: make_ts(i64::try_from(idx * 5).unwrap()),
                    model: Some("claude-sonnet-4-5".to_string()),
                    cwd_hash: None,
                    usage: Usage {
                        fresh_in: fresh,
                        cache_write: write,
                        cache_read: read,
                        output: out,
                        reasoning: None,
                        confidence: conf,
                        estimation_method: None,
                        cache_write_split: None,
                        agent_reported_cost_usd: None,
                    },
                    added,
                    hints: TurnHints {
                        resume_start: resume,
                        compaction: compact,
                        sidechain: side,
                        api_error: api_err,
                        rate_limit_used_percent: None,
                        context_window_override: None,
                    },
                }
            })
            .collect();

        let sub_sessions: Vec<Vec<TurnRecord>> = sub_specs
            .into_iter()
            .enumerate()
            .map(|(s_idx, specs)| {
                specs
                    .into_iter()
                    .enumerate()
                    .map(|(t_idx, (fresh, write, read, out, conf, added, _, compact, _, api_err))| {
                        TurnRecord {
                            agent: AgentId::ClaudeCode,
                            session_id: format!("prop-sub-{s_idx}"),
                            parent_session_id: Some("prop-parent".to_string()),
                            turn: u32::try_from(t_idx + 1).unwrap(),
                            ts: make_ts(i64::try_from(t_idx * 3).unwrap()),
                            model: Some("claude-haiku-4-5".to_string()),
                            cwd_hash: None,
                            usage: Usage {
                                fresh_in: fresh,
                                cache_write: write,
                                cache_read: read,
                                output: out,
                                reasoning: None,
                                confidence: conf,
                                estimation_method: None,
                                cache_write_split: None,
                                agent_reported_cost_usd: None,
                            },
                            added,
                            hints: TurnHints {
                                resume_start: false,
                                compaction: compact,
                                sidechain: true,
                                api_error: api_err,
                                rate_limit_used_percent: None,
                                context_window_override: None,
                            },
                        }
                    })
                    .collect()
            })
            .collect();

        let sub_slices: Vec<&[TurnRecord]> = sub_sessions.iter().map(Vec::as_slice).collect();
        let attrib = attribute_session_with_subagents(&parent_turns, &sub_slices);

        prop_assert_eq!(attrib.turns.len(), parent_turns.len());
        prop_assert_eq!(attrib.categories.sum(), attrib.totals);

        let mut expected_totals = TokenSplit::ZERO;
        for (tr, ta) in parent_turns.iter().zip(attrib.turns.iter()) {
            let measured = TokenSplit::from_usage(&tr.usage);
            prop_assert_eq!(ta.measured, measured);
            prop_assert_eq!(ta.categories.sum(), measured);
            expected_totals.add_assign(measured);
        }
        for sub in &sub_sessions {
            for tr in sub {
                expected_totals.add_assign(TokenSplit::from_usage(&tr.usage));
            }
        }
        prop_assert_eq!(attrib.totals, expected_totals);

        for blk in &attrib.carried_blocks {
            let ceiling = blk.bytes.max(blk.est_tokens);
            prop_assert!(
                blk.scaled_prompt_tokens <= ceiling,
                "block scaled_prompt_tokens ({}) exceeded byte ceiling ({})",
                blk.scaled_prompt_tokens,
                ceiling
            );
            prop_assert_eq!(blk.initial_split.prompt_tokens(), blk.scaled_prompt_tokens);
            prop_assert_eq!(blk.initial_split.output, 0);
            prop_assert_eq!(blk.carried_split.output, 0);
            if blk.born_turn.is_none() {
                prop_assert_eq!(blk.scaled_prompt_tokens, 0);
                prop_assert_eq!(blk.carried_turns, 0);
            }
        }
    }
}
