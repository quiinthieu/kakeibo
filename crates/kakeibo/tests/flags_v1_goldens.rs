//! End-to-end Task T07 fixture verification and `insta` text goldens (`PLAN.md` §6.3, §9, §10 T07).

mod common;

use common::TestEnv;
use kakeibo_core::attrib::attribute_session;
use kakeibo_core::flags::{evaluate_flags_v1_default, FlagFinding, FlagId, TurnRange};
use kakeibo_core::TurnRecord;
use kakeibo_parse::{parse_claude_file, parse_codex_file};
use kakeibo_report::flags::{render_flag_evidence, render_flag_sentence, render_flags_text};

fn stage_and_evaluate(agent: &str, scenario: &str) -> (Vec<TurnRecord>, Vec<FlagFinding>, String) {
    let env = TestEnv::new();
    let (staged_paths, gt) = env.stage_fixture(agent, scenario);
    assert_eq!(gt.agent, agent);
    assert_eq!(gt.scenario, scenario);
    assert_eq!(staged_paths.len(), 1);

    let _ro_guard = env.make_logs_readonly();
    let records = match agent {
        "claude-code" => {
            let out = parse_claude_file(&staged_paths[0]).expect("parse claude fixture");
            assert!(
                out.diagnostics.issues.is_empty(),
                "unexpected parse diagnostics in {agent}/{scenario}: {:?}",
                out.diagnostics.issues
            );
            out.records
        }
        "codex" => {
            let out = parse_codex_file(&staged_paths[0]).expect("parse codex fixture");
            assert!(
                out.diagnostics.issues.is_empty(),
                "unexpected parse diagnostics in {agent}/{scenario}: {:?}",
                out.diagnostics.issues
            );
            out.records
        }
        other => panic!("unsupported test agent `{other}`"),
    };

    let attrib = attribute_session(&records);
    assert_eq!(
        attrib.categories.sum(),
        attrib.totals,
        "session attribution must reconcile on {agent}/{scenario}"
    );
    let findings = evaluate_flags_v1_default(&records, &attrib);
    let rendered = render_flags_text(&findings);
    (records, findings, rendered)
}

#[test]
fn basic_and_non_flag_scenarios_are_completely_silent() {
    for (agent, scenario) in [
        ("claude-code", "basic"),
        ("codex", "basic"),
        ("claude-code", "streaming-dupes"),
        ("codex", "codex-dual-usage"),
    ] {
        let (_records, findings, rendered) = stage_and_evaluate(agent, scenario);
        assert!(
            findings.is_empty(),
            "expected zero flags on `{agent}/{scenario}`, got: {findings:?}"
        );
        assert_eq!(rendered, "No flags.\n");
    }
}

#[test]
fn each_v1_flag_fires_on_its_dedicated_scenario_with_exact_turn_range_and_text_golden() {
    let expected_cases = [
        (
            "claude-code",
            "resume-cold",
            FlagId::ColdResume,
            TurnRange::single(3),
            122_110_u64,
        ),
        (
            "claude-code",
            "near-full-context",
            FlagId::NearFullContext,
            TurnRange::new(2, 5),
            724_560_u64,
        ),
        (
            "claude-code",
            "tool-bloat",
            FlagId::ToolOutputBloat,
            TurnRange::new(2, 5),
            120_000_u64,
        ),
        (
            "claude-code",
            "cache-miss-storm",
            FlagId::CacheMissStorm,
            TurnRange::new(2, 4),
            45_480_u64,
        ),
        (
            "claude-code",
            "heavy-overhead",
            FlagId::HeavyOverhead,
            TurnRange::single(1),
            48_400_u64,
        ),
    ];

    let banned_provider_blame_phrases = [
        "anthropic bug",
        "openai bug",
        "server bug",
        "provider bug",
        "provider outage",
        "provider evicted",
        "server-side",
        "provider-side",
    ];

    let mut golden_sections = Vec::new();
    for (agent, scenario) in [("claude-code", "basic"), ("codex", "basic")] {
        let (_records, _findings, rendered) = stage_and_evaluate(agent, scenario);
        golden_sections.push(format!("=== {agent}/{scenario} ===\n{rendered}"));
    }

    for (agent, scenario, expected_id, expected_range, expected_impact) in expected_cases {
        let (_records, findings, rendered) = stage_and_evaluate(agent, scenario);
        assert_eq!(
            findings.len(),
            1,
            "expected `{expected_id}` on `{agent}/{scenario}`, got: {findings:?}"
        );
        let finding = &findings[0];
        assert_eq!(finding.id, expected_id);
        assert_eq!(finding.turn_range, expected_range);
        assert_eq!(finding.impact_tokens, expected_impact);

        let sentence = render_flag_sentence(finding);
        let evidence = render_flag_evidence(finding);
        assert!(
            sentence.ends_with('.'),
            "flag sentence must end with a period: `{sentence}`"
        );
        assert!(
            !sentence[..sentence.len() - 1].contains(". "),
            "flag summary must be a single sentence (`PLAN.md` §6.3): `{sentence}`"
        );
        assert!(
            evidence.starts_with("evidence: "),
            "flag evidence line must start with `evidence: `: `{evidence}`"
        );
        assert!(
            evidence.contains("measured") || evidence.contains("estimated:"),
            "every evidence line must label provenance (`PLAN.md` §0 #7): `{evidence}`"
        );

        let lower = rendered.to_ascii_lowercase();
        for banned in banned_provider_blame_phrases {
            assert!(
                !lower.contains(banned),
                "rendered flag output must never claim a provider-side cause (`{banned}`): {rendered}"
            );
        }

        golden_sections.push(format!("=== {agent}/{scenario} ===\n{rendered}"));
    }

    let full_golden = golden_sections.join("\n");
    insta::assert_snapshot!("t07_flags_v1_text", full_golden);
}
