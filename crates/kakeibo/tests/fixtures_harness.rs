//! T02 integration tests: deterministic fixture generator, field-source citations,
//! ground-truth invariants, `TestEnv` temp-`HOME` isolation, and `insta` snapshots.

mod common;

use common::{workspace_root, ExpectedGroundTruth, ExpectedTotals, TestEnv};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const REQUIRED_SCENARIOS: &[(&str, &str)] = &[
    ("claude-code", "basic"),
    ("claude-code", "streaming-dupes"),
    ("codex", "basic"),
    ("codex", "codex-dual-usage"),
];

const OPAQUE_MAP_PREFIXES: &[&str] = &[
    "message.content[].input.",
    "snapshot.trackedFileBackups.",
    "payload.state.",
];

fn collect_relative_files(base: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(base: &Path, cur: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let entries = fs::read_dir(cur)
            .unwrap_or_else(|e| panic!("failed to read directory {}: {e}", cur.display()));
        for entry in entries {
            let entry = entry.expect("valid dir entry");
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path.strip_prefix(base).expect("strip prefix").to_path_buf();
                let bytes = fs::read(&path).expect("read fixture file bytes");
                out.insert(rel, bytes);
            }
        }
    }

    let mut out = BTreeMap::new();
    walk(base, base, &mut out);
    out
}

fn collect_json_field_paths(value: &serde_json::Value, prefix: &str, out: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if !prefix.is_empty() && !prefix.ends_with("[]") {
                out.insert(prefix.to_string());
            }
            for (k, v) in map {
                let next = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                if OPAQUE_MAP_PREFIXES.iter().any(|p| next.starts_with(p)) {
                    continue;
                }
                collect_json_field_paths(v, &next, out);
            }
        }
        serde_json::Value::Array(items) => {
            if !prefix.is_empty() && !prefix.ends_with("[]") {
                out.insert(prefix.to_string());
            }
            let arr_prefix = format!("{prefix}[]");
            for item in items {
                collect_json_field_paths(item, &arr_prefix, out);
            }
        }
        _ => {
            if !prefix.is_empty() && !prefix.ends_with("[]") {
                out.insert(prefix.to_string());
            }
        }
    }
}

fn parse_cited_fields_from_format_doc(doc_path: &Path) -> BTreeSet<String> {
    let raw = fs::read_to_string(doc_path)
        .unwrap_or_else(|e| panic!("failed to read format doc {}: {e}", doc_path.display()));

    // 1. Collect declared `[SN]` source keys in `## Sources`.
    let mut defined_sources = BTreeSet::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("- **[S") {
            if let Some(end) = rest.find("]**") {
                let key = format!("[S{}]", &rest[..end]);
                assert!(
                    trimmed.contains("checked: 2026-"),
                    "source {key} in {} must record a checked date",
                    doc_path.display()
                );
                defined_sources.insert(key);
            }
        }
    }
    assert!(
        !defined_sources.is_empty(),
        "format doc {} must define `[SN]` sources",
        doc_path.display()
    );

    // 2. Parse Markdown table rows `| `<field>` | `<type>` | <semantics> | <source> |`.
    let mut cited_fields = BTreeSet::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("| `") {
            continue;
        }
        let cols: Vec<&str> = trimmed.split('|').map(str::trim).collect();
        if cols.len() < 6 {
            continue;
        }
        let field_cell = cols[1];
        let source_cell = cols[cols.len() - 2];
        if let Some(field) = field_cell
            .strip_prefix('`')
            .and_then(|s| s.strip_suffix('`'))
        {
            if field.contains('/')
                || field.contains('_') && field.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            {
                // Discovery path / env var table row (`~/.claude/...`, `CLAUDE_CONFIG_DIR`).
                continue;
            }
            let cites_known_source = defined_sources
                .iter()
                .any(|src_key| source_cell.contains(src_key.as_str()));
            assert!(
                cites_known_source,
                "field `{field}` in {} has missing or unresolved source citation `{source_cell}`",
                doc_path.display()
            );
            cited_fields.insert(field.to_string());
        }
    }

    cited_fields
}

#[test]
fn generator_is_byte_stable_for_fixed_seed() {
    let root = workspace_root();
    let generator = root.join("scripts/gen-fixtures/generate.py");
    assert!(
        generator.is_file(),
        "fixture generator missing at {}",
        generator.display()
    );

    let out_a = tempfile::tempdir().expect("tempdir A");
    let out_b = tempfile::tempdir().expect("tempdir B");
    let out_diff_seed = tempfile::tempdir().expect("tempdir seed 99");

    for (seed, dir) in [
        ("42", out_a.path()),
        ("42", out_b.path()),
        ("99", out_diff_seed.path()),
    ] {
        let status = Command::new("python3")
            .arg(&generator)
            .args(["--seed", seed, "--out"])
            .arg(dir)
            .status()
            .expect("invoke generate.py");
        assert!(status.success(), "generate.py --seed {seed} failed");
    }

    let files_a = collect_relative_files(out_a.path());
    let files_b = collect_relative_files(out_b.path());
    let files_committed = collect_relative_files(&root.join("tests/fixtures"));
    let files_seed_99 = collect_relative_files(out_diff_seed.path());

    assert!(!files_a.is_empty(), "generator emitted zero files");
    assert_eq!(
        files_a, files_b,
        "two runs with --seed 42 must be byte-for-byte identical"
    );
    assert_eq!(
        files_a, files_committed,
        "committed tests/fixtures/ must match `generate.py --seed 42`"
    );
    assert_ne!(
        files_a, files_seed_99,
        "different seeds must produce distinct synthetic payloads"
    );

    let check_status = Command::new("python3")
        .arg(&generator)
        .args(["--seed", "42", "--check"])
        .status()
        .expect("invoke generate.py --check");
    assert!(
        check_status.success(),
        "generate.py --seed 42 --check must succeed against committed fixtures"
    );
}

#[test]
fn every_used_fixture_field_cites_a_source_in_format_docs() {
    let root = workspace_root();
    let agent_docs = [
        ("claude-code", root.join("docs/formats/claude-code.md")),
        ("codex", root.join("docs/formats/codex.md")),
    ];

    for (agent, doc_path) in agent_docs {
        let cited_fields = parse_cited_fields_from_format_doc(&doc_path);
        let agent_fixtures_dir = root.join("tests/fixtures").join(agent);
        let files = collect_relative_files(&agent_fixtures_dir);

        let mut used_fields = BTreeSet::new();
        let mut jsonl_count = 0usize;

        for (rel, bytes) in &files {
            if rel.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            jsonl_count += 1;
            let text = std::str::from_utf8(bytes).expect("UTF-8 synthetic jsonl");
            for (line_idx, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let val: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
                    panic!(
                        "invalid JSON in {}/{}:{} -> {e}",
                        agent,
                        rel.display(),
                        line_idx + 1
                    )
                });
                collect_json_field_paths(&val, "", &mut used_fields);
            }
        }

        assert!(jsonl_count >= 2, "expected >= 2 JSONL fixtures for {agent}");
        let uncited: Vec<&String> = used_fields
            .iter()
            .filter(|f| !cited_fields.contains(*f))
            .collect();
        assert!(
            uncited.is_empty(),
            "agent `{agent}` fixtures use field(s) missing a cited entry in {}: {uncited:?}",
            doc_path.display()
        );
    }
}

#[test]
fn ground_truth_normalisation_invariants_and_fixture_lint_hold() {
    let root = workspace_root();

    // Run scripts/lint-fixtures.sh across the populated tests/fixtures/ tree.
    let lint_out = Command::new(root.join("scripts/lint-fixtures.sh"))
        .output()
        .expect("run lint-fixtures.sh");
    assert!(
        lint_out.status.success(),
        "lint-fixtures.sh failed on generated fixtures:\n{}",
        String::from_utf8_lossy(&lint_out.stderr)
    );

    for (agent, scenario) in REQUIRED_SCENARIOS {
        let scenario_dir = root.join("tests/fixtures").join(agent).join(scenario);
        let raw = fs::read_to_string(scenario_dir.join("expected.json"))
            .unwrap_or_else(|e| panic!("missing expected.json for {agent}/{scenario}: {e}"));
        let gt: ExpectedGroundTruth = serde_json::from_str(&raw).expect("valid expected.json");

        assert_eq!(gt.schema, "kakeibo.fixture-ground-truth/1");
        assert_eq!(gt.agent, *agent);
        assert_eq!(gt.scenario, *scenario);
        assert_eq!(gt.seed, 42);
        assert!(!gt.sessions.is_empty());

        for rel in &gt.log_files {
            assert!(
                scenario_dir.join(rel).is_file(),
                "expected log_file {rel} missing in {agent}/{scenario}"
            );
        }

        for session in &gt.sessions {
            assert!(!session.turns.is_empty());
            let mut sum_prompt = 0u64;
            let mut sum_fresh = 0u64;
            let mut sum_write = 0u64;
            let mut sum_read = 0u64;
            let mut sum_out = 0u64;
            let mut sum_reasoning = 0u64;

            for (idx, turn) in session.turns.iter().enumerate() {
                assert_eq!(turn.turn, u32::try_from(idx + 1).expect("turn index"));
                assert_eq!(
                    turn.usage.fresh_in + turn.usage.cache_write + turn.usage.cache_read,
                    turn.prompt_tokens,
                    "fresh + write + read == P violated in {agent}/{scenario} turn {}",
                    turn.turn
                );
                sum_prompt += turn.prompt_tokens;
                sum_fresh += turn.usage.fresh_in;
                sum_write += turn.usage.cache_write;
                sum_read += turn.usage.cache_read;
                sum_out += turn.usage.output;
                sum_reasoning += turn.usage.reasoning.unwrap_or(0);
            }

            assert_eq!(
                session.totals,
                ExpectedTotals {
                    turns: u32::try_from(session.turns.len()).expect("turns len"),
                    prompt_tokens: sum_prompt,
                    fresh_in: sum_fresh,
                    cache_write: sum_write,
                    cache_read: sum_read,
                    output: sum_out,
                    reasoning: sum_reasoning,
                },
                "session totals mismatch in {agent}/{scenario}"
            );
        }
    }
}

#[test]
fn test_env_isolates_home_and_enforces_readonly_guard() {
    let env = TestEnv::new();
    assert!(env.home_dir().starts_with(env.root_dir()));
    assert!(env.config_dir().starts_with(env.root_dir()));
    assert!(env.cache_dir().starts_with(env.root_dir()));
    assert!(env.data_dir().starts_with(env.root_dir()));

    if let Some(real_home) = std::env::var_os("HOME") {
        assert_ne!(
            env.home_dir(),
            Path::new(&real_home),
            "TestEnv::home_dir must never point to real $HOME"
        );
    }

    let (staged_claude, claude_gt) = env.stage_fixture("claude-code", "basic");
    let (staged_codex, codex_gt) = env.stage_fixture("codex", "codex-dual-usage");
    assert_eq!(claude_gt.agent, "claude-code");
    assert_eq!(codex_gt.agent, "codex");

    for path in staged_claude.iter().chain(staged_codex.iter()) {
        assert!(path.starts_with(env.home_dir()));
        assert!(path.is_file());
    }

    {
        let _guard = env.make_logs_readonly();
        let write_attempt = fs::OpenOptions::new().append(true).open(&staged_claude[0]);
        assert!(
            write_attempt.is_err(),
            "staged agent log must reject write opens while ReadOnlyGuard is active"
        );

        let cli_out = env
            .kakeibo_cmd()
            .arg("--version")
            .output()
            .expect("run kakeibo inside TestEnv");
        assert!(cli_out.status.success());
    }

    // After ReadOnlyGuard drops, write permissions are restored for clean TempDir removal.
    let reopen_writable = fs::OpenOptions::new().append(true).open(&staged_claude[0]);
    assert!(
        reopen_writable.is_ok(),
        "ReadOnlyGuard::drop must restore write bits for cleanup"
    );
}

#[derive(Serialize)]
struct ScenarioSnapshotEntry {
    agent: String,
    scenario: String,
    session_id: String,
    ignored_records: u64,
    totals: ExpectedTotals,
}

#[test]
fn fixture_corpus_summary_golden_snapshot() {
    let root = workspace_root();
    let mut entries = Vec::new();

    for (agent, scenario) in REQUIRED_SCENARIOS {
        let raw = fs::read_to_string(
            root.join("tests/fixtures")
                .join(agent)
                .join(scenario)
                .join("expected.json"),
        )
        .expect("read expected.json");
        let gt: ExpectedGroundTruth = serde_json::from_str(&raw).expect("parse expected.json");
        for session in gt.sessions {
            entries.push(ScenarioSnapshotEntry {
                agent: gt.agent.clone(),
                scenario: gt.scenario.clone(),
                session_id: session.session_id,
                ignored_records: session.ignored_records,
                totals: session.totals,
            });
        }
    }

    insta::assert_json_snapshot!("t02_fixture_corpus_summary", entries);
}
