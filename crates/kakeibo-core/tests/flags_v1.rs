//! TDD unit and threshold-boundary tests for `kakeibo_core::flags` (`PLAN.md` §0 #7, §6.3, §10 T07).

use kakeibo_core::attrib::{attribute_session, estimate_tokens_from_bytes, TokenSplit};
use kakeibo_core::flags::{
    evaluate_flags_v1, evaluate_flags_v1_default, CacheMissTrigger, ColdResumeTrigger,
    ContextWindowSource, FlagConfigError, FlagEvidence, FlagFinding, FlagId, FlagsConfig,
    ModelCatalog, TurnRange,
};
use kakeibo_core::{
    AgentId, BlockKind, Confidence, ContentBlock, Timestamp, TurnHints, TurnRecord, Usage,
    ESTIMATION_METHOD_SCALED_BYTES_DIV4,
};

fn ts(offset_secs: i64) -> Timestamp {
    Timestamp::from_unix_timestamp(1_793_440_000 + offset_secs).expect("valid timestamp")
}

fn usage(fresh_in: u64, cache_write: u64, cache_read: u64, output: u64) -> Usage {
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

fn block(kind: BlockKind, bytes: u64, tool: Option<&str>) -> ContentBlock {
    ContentBlock {
        kind,
        est_tokens: estimate_tokens_from_bytes(bytes),
        bytes,
        tool: tool.map(str::to_string),
        key_hash: None,
        is_error: false,
    }
}

fn make_turn(
    turn: u32,
    offset_secs: i64,
    model: Option<&str>,
    u: Usage,
    added: Vec<ContentBlock>,
    hints: TurnHints,
) -> TurnRecord {
    TurnRecord {
        agent: AgentId::ClaudeCode,
        session_id: "sess-flag-unit".to_string(),
        parent_session_id: None,
        turn,
        ts: ts(offset_secs),
        model: model.map(str::to_string),
        cwd_hash: None,
        usage: u,
        added,
        hints,
    }
}

fn eval_default(turns: &[TurnRecord]) -> Vec<FlagFinding> {
    let attrib = attribute_session(turns);
    evaluate_flags_v1_default(turns, &attrib)
}

#[test]
fn bundled_configs_validate_and_reject_missing_provenance_or_bad_bounds() {
    let flags_cfg = FlagsConfig::bundled();
    assert_eq!(flags_cfg.schema_version, 1);
    assert_eq!(flags_cfg.cold_resume.cache_ttl_seconds, 300);
    assert!(flags_cfg.cold_resume.reference_url.contains("100447"));
    assert!(flags_cfg
        .heavy_overhead
        .remediation_hint
        .contains("bentoctx budget"));

    let models = ModelCatalog::bundled();
    assert_eq!(models.schema_version, 1);
    assert_eq!(
        models
            .resolve_context_window(Some("claude-sonnet-4-5"), None)
            .unwrap(),
        kakeibo_core::ResolvedContextWindow {
            tokens: 200_000,
            source: ContextWindowSource::Catalog,
        }
    );
    // Dated snapshot matches via separator-bounded prefix:
    assert_eq!(
        models
            .resolve_context_window(Some("claude-sonnet-4-5-20250929"), None)
            .unwrap()
            .tokens,
        200_000
    );
    // Agent-logged override supersedes catalog:
    assert_eq!(
        models
            .resolve_context_window(Some("gpt-5-codex"), Some(272_000))
            .unwrap(),
        kakeibo_core::ResolvedContextWindow {
            tokens: 272_000,
            source: ContextWindowSource::AgentLogOverride,
        }
    );
    // Unknown model without override returns None (never invents a context window):
    assert_eq!(
        models.resolve_context_window(Some("mystery-model-9000"), None),
        None
    );

    // Reject missing `source` provenance:
    let bad_models = r#"
        schema_version = 1
        [models."x"]
        context_window_tokens = 128000
        source = "   "
        checked = "2026-10-31"
    "#;
    assert_eq!(
        ModelCatalog::from_toml_str(bad_models),
        Err(FlagConfigError::MissingProvenance {
            entry: "x".to_string(),
            field: "source",
        })
    );

    // Reject wrong schema_version:
    let bad_ver = r#"
        schema_version = 99
        [models."x"]
        context_window_tokens = 128000
        source = "https://example.invalid"
        checked = "2026-10-31"
    "#;
    assert_eq!(
        ModelCatalog::from_toml_str(bad_ver),
        Err(FlagConfigError::UnsupportedSchemaVersion {
            found: 99,
            expected: 1,
        })
    );
}

#[test]
fn cold_resume_fires_on_idle_gap_resume_or_fork_and_respects_boundaries() {
    // 1. Exact boundary: gap = 300s (not > 300s TTL) without resume_start -> SILENT.
    let at_ttl = vec![
        make_turn(
            1,
            0,
            Some("claude-sonnet-4-5"),
            usage(40, 12_000, 0, 40),
            vec![block(BlockKind::UserPrompt, 160, None)],
            TurnHints::default(),
        ),
        make_turn(
            2,
            10,
            Some("claude-sonnet-4-5"),
            usage(60, 52_000, 12_040, 40),
            vec![block(BlockKind::UserPrompt, 208_000, None)],
            TurnHints::default(),
        ),
        make_turn(
            3,
            310, // gap = 300s == cache_ttl_seconds
            Some("claude-sonnet-4-5"),
            usage(0, 64_100, 0, 40),
            vec![block(BlockKind::UserPrompt, 160, None)],
            TurnHints::default(),
        ),
    ];
    assert!(
        eval_default(&at_ttl).is_empty(),
        "gap == 300s without resume_start must not fire cold-resume"
    );

    // 2. Boundary: gap = 301s (> 300s), P = 63,999 (< 64,000) -> SILENT.
    let below_p = vec![
        make_turn(
            1,
            0,
            Some("claude-sonnet-4-5"),
            usage(40, 12_000, 0, 40),
            vec![block(BlockKind::UserPrompt, 160, None)],
            TurnHints::default(),
        ),
        make_turn(
            2,
            301,
            Some("claude-sonnet-4-5"),
            usage(0, 63_999, 0, 40),
            vec![block(BlockKind::UserPrompt, 200_000, None)],
            TurnHints::default(),
        ),
    ];
    assert!(eval_default(&below_p).is_empty());

    // 3. Boundary: gap = 301s, P = 64,000, cache_write = 44,799 (< 70% = 44,800) -> SILENT.
    let below_pct = vec![
        make_turn(
            1,
            0,
            Some("claude-sonnet-4-5"),
            usage(40, 12_000, 0, 40),
            vec![block(BlockKind::UserPrompt, 160, None)],
            TurnHints::default(),
        ),
        make_turn(
            2,
            301,
            Some("claude-sonnet-4-5"),
            usage(19_201, 44_799, 0, 40),
            vec![block(BlockKind::UserPrompt, 200_000, None)],
            TurnHints::default(),
        ),
    ];
    assert!(eval_default(&below_pct).is_empty());

    // 4. Boundary: gap = 301s, P = 64,000, cache_write = 44,800 (exact 70%) -> FIRES on Turn 2!
    let exact_hit = vec![
        make_turn(
            1,
            0,
            Some("claude-sonnet-4-5"),
            usage(40, 12_000, 0, 40),
            vec![block(BlockKind::UserPrompt, 160, None)],
            TurnHints::default(),
        ),
        make_turn(
            2,
            301,
            Some("claude-sonnet-4-5"),
            usage(19_200, 44_800, 0, 40),
            vec![block(BlockKind::UserPrompt, 200_000, None)],
            TurnHints::default(),
        ),
    ];
    let findings = eval_default(&exact_hit);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, FlagId::ColdResume);
    assert_eq!(findings[0].turn_range, TurnRange::single(2));
    assert_eq!(findings[0].impact_tokens, 44_800);
    assert_eq!(findings[0].confidence, Confidence::Measured);
    assert_eq!(
        findings[0].evidence,
        FlagEvidence::ColdResume {
            trigger: ColdResumeTrigger::IdleGap,
            gap_seconds: Some(301),
            prompt_tokens: 64_000,
            rewritten_tokens: 44_800,
            cache_write_pct: 70,
            reference_url: "https://github.com/anthropics/claude-code/issues/100447".to_string(),
        }
    );

    // 5. Turn 1 in a newly opened fork/resume file (`hints.resume_start == true`, no prior turn timestamp)
    //    with P(1) = 28,000 (using custom min_prompt_tokens = 20,000 so heavy-overhead stays silent) -> FIRES!
    let mut custom_cfg = FlagsConfig::bundled();
    custom_cfg.cold_resume.min_prompt_tokens = 20_000;
    let fork_turn1 = vec![make_turn(
        1,
        0,
        Some("claude-sonnet-4-5"),
        usage(1_000, 27_000, 0, 50),
        vec![block(BlockKind::UserPrompt, 112_000, None)],
        TurnHints {
            resume_start: true,
            ..TurnHints::default()
        },
    )];
    let fork_attrib = attribute_session(&fork_turn1);
    let fork_findings = evaluate_flags_v1(
        &fork_turn1,
        &fork_attrib,
        &custom_cfg,
        &ModelCatalog::bundled(),
        None,
    );
    assert_eq!(fork_findings.len(), 1);
    assert_eq!(fork_findings[0].id, FlagId::ColdResume);
    assert_eq!(fork_findings[0].turn_range, TurnRange::single(1));
    assert!(matches!(
        fork_findings[0].evidence,
        FlagEvidence::ColdResume {
            trigger: ColdResumeTrigger::ResumeOrFork,
            gap_seconds: None,
            ..
        }
    ));
}

#[test]
fn near_full_context_respects_window_pct_request_count_override_and_unknown_model_silence() {
    // 1. Unknown model with no context_window_override -> SILENT (never guesses a window).
    let unknown_model_turns: Vec<TurnRecord> = (1..=4)
        .map(|t| {
            make_turn(
                t,
                i64::from(t) * 10,
                Some("unknown-custom-llm"),
                usage(1_000, 0, if t == 1 { 15_000 } else { 195_000 }, 40),
                vec![block(BlockKind::UserPrompt, 160, None)],
                TurnHints::default(),
            )
        })
        .collect();
    assert!(
        eval_default(&unknown_model_turns).is_empty(),
        "unknown model without context_window_override must never invent a window"
    );

    // 2. Only 2 requests >= 85% (< min_requests = 3) -> SILENT.
    let two_near_full = vec![
        make_turn(
            1,
            0,
            Some("claude-sonnet-4-5"),
            usage(30, 12_000, 0, 40),
            vec![block(BlockKind::UserPrompt, 120, None)],
            TurnHints::default(),
        ),
        make_turn(
            2,
            10,
            Some("claude-sonnet-4-5"),
            usage(40, 158_000, 12_030, 40), // P = 170,070 (85.035%)
            vec![block(BlockKind::UserPrompt, 632_000, None)],
            TurnHints::default(),
        ),
        make_turn(
            3,
            20,
            Some("claude-sonnet-4-5"),
            usage(100, 0, 170_070, 40), // P = 170,170 (85.085%)
            vec![block(BlockKind::UserPrompt, 400, None)],
            TurnHints::default(),
        ),
    ];
    assert!(eval_default(&two_near_full).is_empty());

    // 3. `context_window_override = Some(100_000)` on 3 requests at P = 85,000..=87,000 -> FIRES on 2..=4!
    let override_hints = TurnHints {
        context_window_override: Some(100_000),
        ..TurnHints::default()
    };
    let with_override = vec![
        make_turn(
            1,
            0,
            Some("gpt-5-codex"),
            usage(2_000, 0, 10_000, 40),
            vec![block(BlockKind::UserPrompt, 120, None)],
            override_hints.clone(),
        ),
        make_turn(
            2,
            10,
            Some("gpt-5-codex"),
            usage(73_000, 0, 12_000, 40), // P = 85,000 (exact 85% of 100,000 override)
            vec![block(BlockKind::UserPrompt, 292_000, None)],
            override_hints.clone(),
        ),
        make_turn(
            3,
            20,
            Some("gpt-5-codex"),
            usage(1_000, 0, 85_000, 40), // P = 86,000
            vec![block(BlockKind::UserPrompt, 4_000, None)],
            override_hints.clone(),
        ),
        make_turn(
            4,
            30,
            Some("gpt-5-codex"),
            usage(1_000, 0, 86_000, 40), // P = 87,000
            vec![block(BlockKind::UserPrompt, 4_000, None)],
            override_hints,
        ),
    ];
    let findings = eval_default(&with_override);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, FlagId::NearFullContext);
    assert_eq!(findings[0].turn_range, TurnRange::new(2, 4));
    assert_eq!(findings[0].impact_tokens, 85_000 + 86_000 + 87_000);
    assert_eq!(
        findings[0].evidence,
        FlagEvidence::NearFullContext {
            model: Some("gpt-5-codex".to_string()),
            request_count: 3,
            context_window_tokens: 100_000,
            window_source: ContextWindowSource::AgentLogOverride,
            peak_prompt_tokens: 87_000,
            peak_window_pct: 87,
            tokens_resent: 258_000,
            cache_read_tokens: 12_000 + 85_000 + 86_000,
        }
    );
}

#[test]
fn tool_output_bloat_fires_on_carried_tool_result_with_exact_eviction_turn_range() {
    // Turn 2 ingests 8,000-token Read ToolResult, carried on Turns 3, 4, 5 (3 carried turns),
    // then compacted on Turn 6 and followed by Turn 7 -> turn_range MUST be `2..=5` (not `2..=7`)!
    let turns = vec![
        make_turn(
            1,
            0,
            Some("claude-sonnet-4-5"),
            usage(20, 10_000, 0, 30),
            vec![
                block(BlockKind::UserPrompt, 80, None),
                block(BlockKind::ToolUse, 120, Some("Read")),
            ],
            TurnHints::default(),
        ),
        make_turn(
            2,
            5,
            Some("claude-sonnet-4-5"),
            usage(0, 8_030, 10_020, 20),
            vec![
                block(BlockKind::ToolResult, 32_000, Some("Read")), // exact 8,000 tokens
                block(BlockKind::AssistantText, 80, None),
            ],
            TurnHints::default(),
        ),
        make_turn(
            3,
            10,
            Some("claude-sonnet-4-5"),
            usage(40, 0, 18_050, 20),
            vec![
                block(BlockKind::UserPrompt, 80, None),
                block(BlockKind::AssistantText, 80, None),
            ],
            TurnHints::default(),
        ),
        make_turn(
            4,
            15,
            Some("claude-sonnet-4-5"),
            usage(40, 0, 18_090, 20),
            vec![
                block(BlockKind::UserPrompt, 80, None),
                block(BlockKind::AssistantText, 80, None),
            ],
            TurnHints::default(),
        ),
        make_turn(
            5,
            20,
            Some("claude-sonnet-4-5"),
            usage(40, 0, 18_130, 20),
            vec![
                block(BlockKind::UserPrompt, 80, None),
                block(BlockKind::AssistantText, 80, None),
            ],
            TurnHints::default(),
        ),
        make_turn(
            6,
            25,
            Some("claude-sonnet-4-5"),
            usage(20, 400, 10_000, 20),
            vec![
                block(BlockKind::CompactionSummary, 1_600, None),
                block(BlockKind::UserPrompt, 80, None),
            ],
            TurnHints {
                compaction: true,
                ..TurnHints::default()
            },
        ),
        make_turn(
            7,
            30,
            Some("claude-sonnet-4-5"),
            usage(40, 0, 10_420, 20),
            vec![block(BlockKind::UserPrompt, 160, None)],
            TurnHints::default(),
        ),
    ];

    let findings = eval_default(&turns);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, FlagId::ToolOutputBloat);
    assert_eq!(findings[0].turn_range, TurnRange::new(2, 5));
    assert_eq!(findings[0].impact_tokens, 24_000);
    assert_eq!(findings[0].confidence, Confidence::Estimated);
    assert_eq!(
        findings[0].estimation_method,
        Some(ESTIMATION_METHOD_SCALED_BYTES_DIV4)
    );
    assert_eq!(
        findings[0].evidence,
        FlagEvidence::ToolOutputBloat {
            tool: "Read".to_string(),
            source_turn: 2,
            bytes: 32_000,
            size_tokens: 8_000,
            carried_turns: 3,
            carried_tokens: 24_000,
            carried_split: TokenSplit {
                fresh_in: 0,
                cache_write: 0,
                cache_read: 24_000,
                output: 0,
            },
        }
    );
}

#[test]
fn cache_miss_storm_detects_mid_session_rewrites_and_infers_neutral_triggers() {
    // 1. Only 2 mid-session cache_write misses (< min_requests = 3) -> SILENT.
    let two_misses = vec![
        make_turn(
            1,
            0,
            Some("claude-sonnet-4-5"),
            usage(30, 12_000, 0, 40),
            vec![block(BlockKind::UserPrompt, 120, None)],
            TurnHints::default(),
        ),
        make_turn(
            2,
            10,
            Some("claude-opus-4-5"),
            usage(30, 12_030, 0, 40),
            vec![block(BlockKind::UserPrompt, 120, None)],
            TurnHints::default(),
        ),
        make_turn(
            3,
            20,
            Some("claude-sonnet-4-5"),
            usage(30, 12_060, 0, 40),
            vec![block(BlockKind::UserPrompt, 120, None)],
            TurnHints::default(),
        ),
    ];
    assert!(eval_default(&two_misses).is_empty());

    // 2. 3 mid-session misses on same model with no unattributed spike -> PrefixInvalidation.
    let prefix_storm = vec![
        make_turn(
            1,
            0,
            Some("claude-sonnet-4-5"),
            usage(30, 12_000, 0, 40),
            vec![block(BlockKind::UserPrompt, 120, None)],
            TurnHints::default(),
        ),
        make_turn(
            2,
            10,
            Some("claude-sonnet-4-5"),
            usage(30, 12_030, 0, 40),
            vec![block(BlockKind::UserPrompt, 120, None)],
            TurnHints::default(),
        ),
        make_turn(
            3,
            20,
            Some("claude-sonnet-4-5"),
            usage(30, 12_060, 0, 40),
            vec![block(BlockKind::UserPrompt, 120, None)],
            TurnHints::default(),
        ),
        make_turn(
            4,
            30,
            Some("claude-sonnet-4-5"),
            usage(30, 12_090, 0, 40),
            vec![block(BlockKind::UserPrompt, 120, None)],
            TurnHints::default(),
        ),
    ];
    let findings = eval_default(&prefix_storm);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, FlagId::CacheMissStorm);
    assert_eq!(findings[0].turn_range, TurnRange::new(2, 4));
    assert_eq!(findings[0].impact_tokens, 12_030 + 12_060 + 12_090);
    assert_eq!(
        findings[0].evidence,
        FlagEvidence::CacheMissStorm {
            miss_requests: 3,
            rewritten_tokens: 36_180,
            cache_read_tokens: 0,
            likely_trigger: CacheMissTrigger::PrefixInvalidation,
        }
    );
}

#[test]
fn heavy_overhead_fires_on_threshold_or_user_median_with_bentoctx_hint() {
    let cfg = FlagsConfig::bundled();
    let models = ModelCatalog::bundled();

    // 1. P(1) = 29,999 with no user median -> SILENT.
    let below = vec![make_turn(
        1,
        0,
        Some("claude-sonnet-4-5"),
        usage(24, 29_975, 0, 40),
        vec![block(BlockKind::UserPrompt, 96, None)],
        TurnHints::default(),
    )];
    let below_attrib = attribute_session(&below);
    assert!(evaluate_flags_v1(&below, &below_attrib, &cfg, &models, None).is_empty());

    // 2. P(1) = 21,000 (< 30,000 threshold, >= 16,000 floor) with user_median_p1 = Some(14,000) (150%) -> FIRES!
    let above_median = vec![make_turn(
        1,
        0,
        Some("claude-sonnet-4-5"),
        usage(24, 20_976, 0, 40),
        vec![block(BlockKind::UserPrompt, 96, None)],
        TurnHints::default(),
    )];
    let med_attrib = attribute_session(&above_median);
    let findings = evaluate_flags_v1(&above_median, &med_attrib, &cfg, &models, Some(14_000));
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, FlagId::HeavyOverhead);
    assert_eq!(findings[0].turn_range, TurnRange::single(1));
    assert_eq!(findings[0].impact_tokens, 21_000);
    assert_eq!(
        findings[0].evidence,
        FlagEvidence::HeavyOverhead {
            first_prompt_tokens: 21_000,
            fixed_overhead_tokens: 20_976,
            user_median_p1: Some(14_000),
            exceeded_threshold: false,
            exceeded_user_median: true,
            remediation_hint: "try `bentoctx budget`".to_string(),
        }
    );
}

#[test]
fn multiple_flags_rank_deterministically_by_impact_tokens_desc() {
    // Session triggers:
    // - `heavy-overhead` on Turn 1: P(1) = 35,020 (impact = 35,020)
    // - `tool-output-bloat` across Turns 2..=5: 20,000-token Bash result carried 3 turns (impact = 60,000)
    // - `cold-resume` on Turn 4 after 600s gap: P(4) = 65,120, cache_write = 65,080 (impact = 65,080)
    // Expected deterministic ranking: cold-resume (65,080) -> tool-output-bloat (60,000) -> heavy-overhead (35,020).
    let turns = vec![
        make_turn(
            1,
            0,
            Some("claude-sonnet-4-5"),
            usage(20, 35_000, 0, 40),
            vec![
                block(BlockKind::UserPrompt, 80, None),
                block(BlockKind::ToolUse, 160, Some("Bash")),
            ],
            TurnHints::default(),
        ),
        make_turn(
            2,
            10,
            Some("claude-sonnet-4-5"),
            usage(0, 20_040, 35_020, 20),
            vec![
                block(BlockKind::ToolResult, 80_000, Some("Bash")), // 20,000 tokens
                block(BlockKind::AssistantText, 80, None),
            ],
            TurnHints::default(),
        ),
        make_turn(
            3,
            20,
            Some("claude-sonnet-4-5"),
            usage(40, 0, 55_060, 20),
            vec![
                block(BlockKind::UserPrompt, 80, None),
                block(BlockKind::AssistantText, 80, None),
            ],
            TurnHints::default(),
        ),
        make_turn(
            4,
            630, // 610s gap > 300s TTL; P(4) = 65,120 >= 64,000
            Some("claude-sonnet-4-5"),
            usage(40, 65_080, 0, 20),
            vec![
                block(BlockKind::UserPrompt, 40_000, None),
                block(BlockKind::AssistantText, 80, None),
            ],
            TurnHints::default(),
        ),
        make_turn(
            5,
            640,
            Some("claude-sonnet-4-5"),
            usage(40, 0, 65_120, 20),
            vec![block(BlockKind::UserPrompt, 160, None)],
            TurnHints::default(),
        ),
    ];

    let findings = eval_default(&turns);
    let ids_and_impacts: Vec<(FlagId, TurnRange, u64)> = findings
        .iter()
        .map(|f| (f.id, f.turn_range, f.impact_tokens))
        .collect();
    assert_eq!(
        ids_and_impacts,
        vec![
            (FlagId::ColdResume, TurnRange::single(4), 65_080),
            (FlagId::ToolOutputBloat, TurnRange::new(2, 5), 60_000),
            (FlagId::HeavyOverhead, TurnRange::single(1), 35_020),
        ]
    );
}
