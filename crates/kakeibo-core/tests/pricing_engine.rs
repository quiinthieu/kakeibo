//! T09 unit & reconciliation tests for pure pricing schemas, `5m`/`1h` prompt-cache
//! write tiers, reasoning-token rates, LiteLLM JSON import, and unpriced invariants.

use std::fs;
use std::path::Path;

use kakeibo_core::attrib::{attribute_session, attribute_session_with_subagents};
use kakeibo_core::pricing::{
    import_litellm_json_str, price_session, CostStatus, ModelPriceEntry, PricingCatalog,
    PricingError, RateUnit, LITELLM_IMPORT_SOURCE, PRICING_SCHEMA_VERSION,
};
use kakeibo_core::{
    AgentId, BlockKind, CacheWriteSplit, Category, Confidence, ContentBlock, DisjointUsageInput,
    InclusiveUsageInput, Timestamp, TurnHints, TurnRecord, Usage,
};

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn ts(rfc3339: &str) -> Timestamp {
    Timestamp::parse_rfc3339(rfc3339).expect("valid timestamp")
}

#[test]
fn bundled_pricing_toml_ships_empty_defaults_with_provenance_and_never_prices_as_zero() {
    let catalog = PricingCatalog::bundled();
    assert_eq!(catalog.schema_version, PRICING_SCHEMA_VERSION);
    assert_eq!(catalog.rate_unit, RateUnit::Per1MTokens);
    assert!(
        catalog.models.len() >= 5,
        "expected Anthropic, OpenAI, and Google placeholder entries"
    );

    for (model_id, entry) in &catalog.models {
        assert!(
            !entry.is_priced(),
            "bundled data/pricing.toml entry `{model_id}` must ship with empty rate placeholders"
        );
        assert_eq!(entry.currency, "USD");
        assert!(
            entry.source.starts_with("https://"),
            "bundled entry `{model_id}` must cite an official vendor pricing URL"
        );
        assert!(
            !entry.checked.trim().is_empty(),
            "bundled entry `{model_id}` must record a checked date"
        );
        assert!(catalog.resolve_priced(Some(model_id)).is_none());
    }

    // Even a session on a known model (`claude-sonnet-4-5`) must remain `Unpriced` (`None`), never `$0`.
    let usage = Usage::from_disjoint(DisjointUsageInput::measured(1_000, 2_000, 5_000, 400))
        .expect("valid usage");
    let turns = vec![TurnRecord {
        agent: AgentId::ClaudeCode,
        session_id: "sess-unpriced".to_string(),
        parent_session_id: None,
        turn: 1,
        ts: ts("2026-10-31T10:00:00Z"),
        model: Some("claude-sonnet-4-5".to_string()),
        cwd_hash: Some("0123456789abcdef".to_string()),
        usage,
        added: vec![ContentBlock {
            kind: BlockKind::UserPrompt,
            est_tokens: 40,
            bytes: 160,
            tool: None,
            key_hash: None,
            is_error: false,
        }],
        hints: TurnHints::default(),
    }];
    let attrib = attribute_session(&turns);
    let cost = price_session(&turns, &[], &attrib, &catalog);

    assert_eq!(cost.status, CostStatus::Unpriced);
    assert_eq!(cost.total_cost, None);
    assert_eq!(cost.category_costs, None);
    assert_eq!(cost.priced_turns, 0);
    assert_eq!(cost.unpriced_turns, 1);
    assert_eq!(cost.unpriced_models, vec!["claude-sonnet-4-5".to_string()]);
    assert_eq!(cost.turn_costs, vec![None]);
}

#[test]
fn pricing_toml_validation_rejects_missing_provenance_and_negative_rates() {
    let missing_source = r#"
schema_version = 1
[models."claude-sonnet-4-5"]
currency = "USD"
input = 2.0
output = 10.0
source = "   "
checked = "2026-10-31"
"#;
    assert!(matches!(
        PricingCatalog::from_toml_str(missing_source),
        Err(PricingError::MissingProvenance {
            field: "source",
            ..
        })
    ));

    let missing_checked = r#"
schema_version = 1
[models."claude-sonnet-4-5"]
currency = "USD"
input = 2.0
output = 10.0
source = "https://www.anthropic.com/pricing"
checked = ""
"#;
    assert!(matches!(
        PricingCatalog::from_toml_str(missing_checked),
        Err(PricingError::MissingProvenance {
            field: "checked",
            ..
        })
    ));

    let negative_rate = r#"
schema_version = 1
[models."claude-sonnet-4-5"]
currency = "USD"
input = -0.5
output = 10.0
source = "https://www.anthropic.com/pricing"
checked = "2026-10-31"
"#;
    assert!(matches!(
        PricingCatalog::from_toml_str(negative_rate),
        Err(PricingError::InvalidRate { field: "input", .. })
    ));
}

#[test]
fn litellm_tiny_fixture_imports_6_rate_classes_and_skips_sample_spec_and_unpriced() {
    let fixture_path = workspace_root().join("tests/fixtures/pricing/litellm-tiny.json");
    let raw = fs::read_to_string(&fixture_path).expect("read litellm-tiny.json");

    let imported = import_litellm_json_str(&raw, "2026-10-31").expect("import litellm-tiny.json");
    assert_eq!(imported.imported_models, 4);
    assert_eq!(imported.skipped_entries, 2);
    assert!(!imported.catalog.models.contains_key("sample_spec"));

    let sonnet = imported
        .catalog
        .models
        .get("claude-sonnet-4-5")
        .expect("sonnet imported");
    assert_eq!(
        sonnet,
        &ModelPriceEntry {
            currency: "USD".to_string(),
            input: Some(2.0),
            cache_write_5m: Some(2.5),
            cache_write_1h: Some(4.0),
            cache_read: Some(0.2),
            output: Some(10.0),
            reasoning: None,
            source: LITELLM_IMPORT_SOURCE.to_string(),
            checked: "2026-10-31".to_string(),
        }
    );

    let codex = imported
        .catalog
        .models
        .get("gpt-5-codex")
        .expect("gpt-5-codex imported");
    assert_eq!(codex.input, Some(1.5));
    assert_eq!(codex.cache_read, Some(0.15));
    assert_eq!(codex.output, Some(6.0));
    assert_eq!(codex.reasoning, Some(8.0));

    // Merging imported catalog onto bundled catalog resolves bare `"claude-opus-4-5"`
    // (including dated `"claude-opus-4-5-20251101"`) to `"anthropic/claude-opus-4-5"`
    // rather than stopping on the unpriced bundled `"claude-opus-4-5"` placeholder.
    let mut merged = PricingCatalog::bundled();
    merged.merge_overlay(&imported.catalog);
    let (matched_key, opus_entry) = merged
        .resolve_priced(Some("claude-opus-4-5-20251101"))
        .expect("dated opus resolves to priced provider-prefixed entry");
    assert_eq!(matched_key, "anthropic/claude-opus-4-5");
    assert_eq!(opus_entry.input, Some(5.0));
    assert_eq!(opus_entry.cache_write_1h, Some(10.0));

    // Round-trip TOML serialization preserves every field cleanly.
    let toml_text = merged.to_toml_string().expect("serialize merged TOML");
    let reparsed = PricingCatalog::from_toml_str(&toml_text).expect("reparse merged TOML");
    assert_eq!(reparsed, merged);
}

#[test]
fn cache_write_5m_1h_split_reasoning_subagents_and_categories_reconcile_exactly() {
    let fixture_path = workspace_root().join("tests/fixtures/pricing/litellm-tiny.json");
    let raw = fs::read_to_string(&fixture_path).expect("read litellm-tiny.json");
    let imported = import_litellm_json_str(&raw, "2026-10-31").expect("import");
    let mut catalog = PricingCatalog::bundled();
    catalog.merge_overlay(&imported.catalog);

    // Turn 1 (`claude-sonnet-4-5`):
    //   fresh_in = 1,000 * $2.00/1M         = $0.002000
    //   cache_write = 30,000 split into:
    //     ephemeral_5m = 10,000 * $2.50/1M  = $0.025000
    //     ephemeral_1h = 20,000 * $4.00/1M  = $0.080000
    //   cache_read = 0                      = $0.000000
    //   output = 500 * $10.00/1M            = $0.005000
    //   Turn 1 exact total                  = $0.112000
    let u1 = Usage::from_disjoint(DisjointUsageInput {
        input_tokens: 1_000,
        cache_write_tokens: 30_000,
        cache_read_tokens: 0,
        output_tokens: 500,
        reasoning_tokens: None,
        confidence: Confidence::Measured,
        estimation_method: None,
        cache_write_split: Some(CacheWriteSplit {
            ephemeral_5m: 10_000,
            ephemeral_1h: 20_000,
        }),
        agent_reported_cost_usd: None,
    })
    .expect("valid u1");

    // Turn 2 (`gpt-5-codex`, testing distinct reasoning rate without double-counting output):
    //   inclusive input = 40,000 with cache_read = 30,000 -> fresh_in = 10,000
    //   fresh_in = 10,000 * $1.50/1M        = $0.015000
    //   cache_read = 30,000 * $0.15/1M      = $0.004500
    //   output = 2,000 with reasoning = 1,500 inside output:
    //     regular_out = 500 * $6.00/1M      = $0.003000
    //     reasoning   = 1,500 * $8.00/1M    = $0.012000
    //   Turn 2 exact total                  = $0.034500
    let u2 = Usage::from_inclusive_checked(InclusiveUsageInput {
        input_tokens: 40_000,
        cache_write_tokens: 0,
        cache_read_tokens: 30_000,
        output_tokens: 2_000,
        reasoning_tokens: Some(1_500),
        confidence: Confidence::Measured,
        estimation_method: None,
        agent_reported_cost_usd: None,
    })
    .expect("valid u2");

    let primary_turns = vec![
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-mixed".to_string(),
            parent_session_id: None,
            turn: 1,
            ts: ts("2026-10-31T11:00:00Z"),
            model: Some("claude-sonnet-4-5".to_string()),
            cwd_hash: Some("aaaa1111bbbb2222".to_string()),
            usage: u1,
            added: vec![ContentBlock {
                kind: BlockKind::UserPrompt,
                est_tokens: 200,
                bytes: 800,
                tool: None,
                key_hash: None,
                is_error: false,
            }],
            hints: TurnHints::default(),
        },
        TurnRecord {
            agent: AgentId::ClaudeCode,
            session_id: "sess-mixed".to_string(),
            parent_session_id: None,
            turn: 2,
            ts: ts("2026-10-31T11:00:10Z"),
            model: Some("gpt-5-codex".to_string()),
            cwd_hash: Some("aaaa1111bbbb2222".to_string()),
            usage: u2,
            added: vec![ContentBlock {
                kind: BlockKind::ToolResult,
                est_tokens: 8_000,
                bytes: 32_000,
                tool: Some("Read".to_string()),
                key_hash: None,
                is_error: false,
            }],
            hints: TurnHints::default(),
        },
    ];

    // Child subagent (`claude-haiku-4-5`):
    //   fresh_in = 2,000 * $1.00/1M         = $0.002000
    //   cache_write (5m) = 4,000 * $1.25/1M = $0.005000
    //   cache_read = 10,000 * $0.10/1M      = $0.001000
    //   output = 1,000 * $5.00/1M           = $0.005000
    //   Subagent exact total                = $0.013000
    let sub_usage = Usage::from_disjoint(DisjointUsageInput::measured(2_000, 4_000, 10_000, 1_000))
        .expect("valid sub usage");
    let subagent_turns = vec![TurnRecord {
        agent: AgentId::ClaudeCode,
        session_id: "sess-mixed:subagent:sub01".to_string(),
        parent_session_id: Some("sess-mixed".to_string()),
        turn: 1,
        ts: ts("2026-10-31T11:00:05Z"),
        model: Some("claude-haiku-4-5".to_string()),
        cwd_hash: Some("aaaa1111bbbb2222".to_string()),
        usage: sub_usage,
        added: vec![],
        hints: TurnHints {
            sidechain: true,
            ..TurnHints::default()
        },
    }];

    let sub_slices: &[&[TurnRecord]] = &[subagent_turns.as_slice()];
    let attrib = attribute_session_with_subagents(&primary_turns, sub_slices);
    let priced = price_session(&primary_turns, sub_slices, &attrib, &catalog);

    assert_eq!(priced.status, CostStatus::Configured);
    assert_eq!(priced.currency.as_deref(), Some("USD"));
    assert_eq!(priced.priced_turns, 3);
    assert_eq!(priced.unpriced_turns, 0);
    assert!(priced.unpriced_models.is_empty());

    let t1_cost = priced.turn_costs[0].as_ref().expect("turn 1 priced");
    assert_eq!(t1_cost.write_tiers.tokens_5m, 10_000);
    assert_eq!(t1_cost.write_tiers.tokens_1h, 20_000);
    assert!((t1_cost.cache_write_5m_cost - 0.025).abs() < 1e-12);
    assert!((t1_cost.cache_write_1h_cost - 0.080).abs() < 1e-12);
    assert!((t1_cost.total_cost - 0.112).abs() < 1e-12);

    let t2_cost = priced.turn_costs[1].as_ref().expect("turn 2 priced");
    assert!((t2_cost.output_cost - 0.003).abs() < 1e-12);
    assert!((t2_cost.reasoning_cost - 0.012).abs() < 1e-12);
    assert!((t2_cost.total_cost - 0.0345).abs() < 1e-12);

    // Total session cost = $0.112 + $0.0345 + $0.013 = $0.1595
    let total_usd = priced.total_cost.expect("session total_cost");
    assert!((total_usd - 0.1595).abs() < 1e-12);

    let cat_costs = priced.category_costs.as_ref().expect("category_costs");
    let subagent_cat_usd = cat_costs
        .get(&Category::Subagents)
        .copied()
        .expect("subagents category");
    assert!((subagent_cat_usd - 0.013).abs() < 1e-12);

    let cat_sum: f64 = Category::ALL
        .into_iter()
        .map(|c| cat_costs.get(&c).copied().unwrap_or(0.0))
        .sum();
    assert!(
        (cat_sum - total_usd).abs() < 1e-12,
        "category costs ({cat_sum}) must reconcile exactly to session total ({total_usd})"
    );
}
