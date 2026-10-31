//! End-to-end fixture parsing -> attribution reconciliation, NDJSON ledger parity,
//! `tool-bloat` hand-computed carry assertions, and `insta` golden snapshots (`PLAN.md` §6.1, §9, §10 T06).

use std::fs;
use std::path::{Path, PathBuf};

use kakeibo_core::attrib::{attribute_session, CategoryBreakdown, SessionAttribution, TokenSplit};
use kakeibo_core::ledger::{decode_ndjson, encode_ndjson};
use kakeibo_core::{BlockKind, Category, TurnRecord};
use kakeibo_parse::{parse_claude_file, parse_codex_file};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must resolve")
}

#[derive(Debug, Deserialize)]
struct FixtureManifest {
    agent: String,
    scenario: String,
    log_files: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FixtureAttributionSnapshot {
    agent: String,
    scenario: String,
    turns: usize,
    fixed_overhead_tokens: u64,
    totals: TokenSplit,
    categories: CategoryBreakdown,
    top_carried_blocks: Vec<CarriedBlockSnapshot>,
}

#[derive(Debug, Serialize)]
struct CarriedBlockSnapshot {
    source_turn: u32,
    born_turn: Option<u32>,
    kind: BlockKind,
    tool: Option<String>,
    bytes: u64,
    scaled_prompt_tokens: u64,
    carried_turns: u32,
    carried_split: TokenSplit,
    lifetime_prompt_tokens: u64,
}

fn parse_staged_fixture(agent: &str, scenario: &str) -> (FixtureManifest, Vec<TurnRecord>) {
    let root = workspace_root();
    let fixture_dir = root.join("tests/fixtures").join(agent).join(scenario);
    let manifest_raw =
        fs::read_to_string(fixture_dir.join("expected.json")).expect("read expected.json");
    let manifest: FixtureManifest =
        serde_json::from_str(&manifest_raw).expect("parse expected.json");
    assert_eq!(manifest.agent, agent);
    assert_eq!(manifest.scenario, scenario);
    assert_eq!(manifest.log_files.len(), 1);

    // Stage into isolated temp directory (never touching real $HOME or mutating tests/fixtures).
    let temp = TempDir::new().expect("tempdir");
    let staged_log = temp.path().join(&manifest.log_files[0]);
    fs::create_dir_all(staged_log.parent().unwrap()).unwrap();
    fs::copy(fixture_dir.join(&manifest.log_files[0]), &staged_log).unwrap();

    let records = match agent {
        "claude-code" => {
            let out = parse_claude_file(&staged_log).expect("parse claude fixture");
            assert!(
                out.diagnostics.issues.is_empty(),
                "unexpected diagnostics in {agent}/{scenario}: {:?}",
                out.diagnostics.issues
            );
            out.records
        }
        "codex" => {
            let out = parse_codex_file(&staged_log).expect("parse codex fixture");
            assert!(
                out.diagnostics.issues.is_empty(),
                "unexpected diagnostics in {agent}/{scenario}: {:?}",
                out.diagnostics.issues
            );
            out.records
        }
        other => panic!("unsupported test agent {other}"),
    };

    (manifest, records)
}

#[test]
fn all_committed_fixtures_reconcile_and_round_trip_identically_through_ndjson() {
    let scenarios = [
        ("claude-code", "basic"),
        ("claude-code", "streaming-dupes"),
        ("claude-code", "tool-bloat"),
        ("codex", "basic"),
        ("codex", "codex-dual-usage"),
    ];

    let mut snapshots = Vec::new();

    for (agent, scenario) in scenarios {
        let (_manifest, records) = parse_staged_fixture(agent, scenario);
        assert!(!records.is_empty());

        let attrib: SessionAttribution = attribute_session(&records);
        assert_eq!(attrib.turns.len(), records.len());
        assert_eq!(
            attrib.categories.sum(),
            attrib.totals,
            "session category sum mismatch on {agent}/{scenario}"
        );

        let mut measured_sum = TokenSplit::ZERO;
        for (rec, ta) in records.iter().zip(attrib.turns.iter()) {
            let measured = TokenSplit::from_usage(&rec.usage);
            assert_eq!(ta.turn, rec.turn);
            assert_eq!(ta.measured, measured);
            assert_eq!(
                ta.categories.sum(),
                measured,
                "per-turn reconciliation violated on {agent}/{scenario} turn {}",
                rec.turn
            );
            measured_sum.add_assign(measured);
        }
        assert_eq!(attrib.totals, measured_sum);

        // Verify NDJSON ledger parity (`ADR 002`): reloading from text-free `kakeibo.ledger/1`
        // NDJSON must yield an identical `SessionAttribution`!
        let ndjson = encode_ndjson(&records).expect("encode ndjson");
        let reloaded = decode_ndjson(&ndjson).expect("decode ndjson");
        let reloaded_attrib = attribute_session(&reloaded);
        assert_eq!(
            attrib, reloaded_attrib,
            "attribution after NDJSON round-trip must be identical on {agent}/{scenario}"
        );

        if agent == "claude-code" && scenario == "tool-bloat" {
            assert_eq!(attrib.fixed_overhead_tokens, 10_000);
            let bash_bloat = attrib
                .carried_blocks
                .iter()
                .find(|b| b.kind == BlockKind::ToolResult && b.tool.as_deref() == Some("Bash"))
                .expect("Bash ToolResult block must be tracked");

            assert_eq!(bash_bloat.source_turn, 2);
            assert_eq!(bash_bloat.born_turn, Some(2));
            assert_eq!(bash_bloat.evicted_before_turn, None);
            assert_eq!(bash_bloat.bytes, 160_000);
            assert_eq!(bash_bloat.est_tokens, 40_000);
            assert_eq!(bash_bloat.scaled_prompt_tokens, 40_000);
            assert_eq!(
                bash_bloat.initial_split,
                TokenSplit {
                    fresh_in: 0,
                    cache_write: 40_000,
                    cache_read: 0,
                    output: 0,
                }
            );
            assert_eq!(bash_bloat.carried_turns, 3);
            assert_eq!(
                bash_bloat.carried_split,
                TokenSplit {
                    fresh_in: 0,
                    cache_write: 40_000,
                    cache_read: 80_000,
                    output: 0,
                }
            );
            assert_eq!(bash_bloat.carried_prompt_tokens(), 120_000);
            assert_eq!(bash_bloat.lifetime_prompt_tokens(), 160_000);

            // Session category totals on `claude-code/tool-bloat`:
            // - FixedOverhead: 10,000 * 5 turns (20,000 cache_write on T1+T4, 30,000 cache_read on T2+T3+T5) = 50,000
            assert_eq!(
                attrib.categories.get(Category::FixedOverhead),
                TokenSplit {
                    fresh_in: 0,
                    cache_write: 20_000,
                    cache_read: 30_000,
                    output: 0,
                }
            );
        }

        let mut carried_sorted = attrib.carried_blocks.clone();
        carried_sorted.sort_by(|a, b| {
            b.carried_prompt_tokens()
                .cmp(&a.carried_prompt_tokens())
                .then_with(|| a.block_id.cmp(&b.block_id))
        });
        let top_carried_blocks = carried_sorted
            .into_iter()
            .filter(|b| b.carried_prompt_tokens() > 0)
            .take(3)
            .map(|b| CarriedBlockSnapshot {
                source_turn: b.source_turn,
                born_turn: b.born_turn,
                kind: b.kind,
                tool: b.tool.clone(),
                bytes: b.bytes,
                scaled_prompt_tokens: b.scaled_prompt_tokens,
                carried_turns: b.carried_turns,
                carried_split: b.carried_split,
                lifetime_prompt_tokens: b.lifetime_prompt_tokens(),
            })
            .collect();

        snapshots.push(FixtureAttributionSnapshot {
            agent: agent.to_string(),
            scenario: scenario.to_string(),
            turns: attrib.turns.len(),
            fixed_overhead_tokens: attrib.fixed_overhead_tokens,
            totals: attrib.totals,
            categories: attrib.categories,
            top_carried_blocks,
        });
    }

    insta::assert_json_snapshot!("t06_fixture_attributions", snapshots);
}
