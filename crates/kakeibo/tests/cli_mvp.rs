//! End-to-end `assert_cmd` integration and golden snapshot tests for Task T08 (`CLI & export — MVP release v0.1.0`).
//!
//! Exercises every MVP subcommand (`kakeibo`, `sessions`, `autopsy`, `flags`, `top`, `export`, `doctor`, `--no-cache`)
//! strictly inside `TestEnv` (temp `HOME`, read-only agent logs).

mod common;

use std::fs;
use std::io::Cursor;

use common::TestEnv;
use kakeibo_parse::hash::hash_cwd;
use kakeibo_report::ndjson::read_ledger;
use serde_json::Value;

const ALL_NINE_CATEGORIES: [&str; 9] = [
    "fixed_overhead",
    "resent_history",
    "tool_output",
    "retries",
    "user_input",
    "assistant_output",
    "subagents",
    "compaction",
    "unattributed",
];

#[test]
fn cli_default_view_empty_home_and_no_cache_parity() {
    let empty_env = TestEnv::new();
    let empty_out = empty_env
        .kakeibo_cmd()
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let empty_stdout = String::from_utf8(empty_out).expect("utf-8 stdout");
    assert!(
        empty_stdout.contains("No sessions found"),
        "expected helpful empty-state message, got: {empty_stdout}"
    );
    assert!(
        !empty_stdout.contains(&empty_env.home_dir().to_string_lossy().to_string()),
        "empty-state output must never leak $HOME path"
    );

    let env = TestEnv::new();
    env.stage_fixture("claude-code", "tool-bloat");
    env.stage_fixture("codex", "basic");
    let _ro = env.make_logs_readonly();

    let default_bytes = env
        .kakeibo_cmd()
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let default_stdout = String::from_utf8(default_bytes).expect("utf-8 stdout");

    // Both sessions appear, ordered newest-first (`0018a668` at 09:30:08Z before `9162aea6` at 09:22:42Z).
    let codex_pos = default_stdout
        .find("0018a668")
        .expect("codex session in default view");
    let claude_pos = default_stdout
        .find("9162aea6")
        .expect("claude tool-bloat session in default view");
    assert!(
        codex_pos < claude_pos,
        "sessions must be ordered newest-first:\n{default_stdout}"
    );

    // Honest pricing & flag display.
    assert!(default_stdout.contains("unpriced"));
    assert!(
        !default_stdout.contains("$0"),
        "unpriced sessions must never display $0:\n{default_stdout}"
    );
    assert!(default_stdout.contains("tool-output-bloat"));
    assert!(!default_stdout.contains("/synthetic/workspace/"));

    // `--no-cache` contract (`PLAN.md` §3.2): identical output.
    let nocache_bytes = env
        .kakeibo_cmd()
        .arg("--no-cache")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let nocache_stdout = String::from_utf8(nocache_bytes).expect("utf-8 stdout");
    assert_eq!(default_stdout, nocache_stdout);
}

#[test]
fn cli_sessions_filters_by_agent_since_and_project() {
    let env = TestEnv::new();
    env.stage_fixture("claude-code", "tool-bloat");
    env.stage_fixture("codex", "basic");
    let _ro = env.make_logs_readonly();

    // 1. `--agent` filter.
    let codex_only = String::from_utf8(
        env.kakeibo_cmd()
            .args(["sessions", "--agent", "codex"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert!(codex_only.contains("0018a668"));
    assert!(!codex_only.contains("9162aea6"));

    // Unknown `--agent` exits non-zero.
    env.kakeibo_cmd()
        .args(["sessions", "--agent", "not-an-agent"])
        .assert()
        .failure();

    // 2. `--since` relative window (`5m` back from 2026-10-31T09:32:00Z -> cutoff 09:27:00Z).
    let recent_rel = String::from_utf8(
        env.kakeibo_cmd()
            .env("KAKEIBO_TEST_NOW", "2026-10-31T09:32:00Z")
            .args(["sessions", "--since", "5m"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert!(recent_rel.contains("0018a668"));
    assert!(!recent_rel.contains("9162aea6"));

    // Explicit RFC 3339 cutoff in `--since`.
    let recent_abs = String::from_utf8(
        env.kakeibo_cmd()
            .args(["sessions", "--since", "2026-10-31T09:25:00Z"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert!(recent_abs.contains("0018a668"));
    assert!(!recent_abs.contains("9162aea6"));

    // 3. `--project` filter by path (hashed on the fly) and by `cwd_hash` prefix.
    let by_path = String::from_utf8(
        env.kakeibo_cmd()
            .args(["sessions", "--project", "/synthetic/workspace/bloat"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert!(by_path.contains("9162aea6"));
    assert!(!by_path.contains("0018a668"));

    let beta_hash = hash_cwd("/synthetic/workspace/beta");
    let by_hash_prefix = String::from_utf8(
        env.kakeibo_cmd()
            .args(["sessions", "--project", &beta_hash[..8]])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert!(by_hash_prefix.contains("0018a668"));
    assert!(!by_hash_prefix.contains("9162aea6"));
}

#[test]
fn cli_autopsy_latest_prefix_turns_category_subagent_and_golden() {
    let env = TestEnv::new();
    env.stage_fixture("claude-code", "tool-bloat");
    env.stage_fixture("codex", "basic");
    let _ro = env.make_logs_readonly();

    // `latest` resolves to `0018a668-5865-4d4b-8b2f-56b827242e3d` (09:30:08Z).
    let latest_out = String::from_utf8(
        env.kakeibo_cmd()
            .args(["autopsy", "latest"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert!(latest_out.contains("0018a668-5865-4d4b-8b2f-56b827242e3d"));
    assert!(latest_out.contains("codex"));

    // Prefix match + `--turns` locked by insta text golden.
    let bloat_autopsy = String::from_utf8(
        env.kakeibo_cmd()
            .args(["autopsy", "9162aea6", "--turns"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert!(bloat_autopsy.contains("9162aea6-bfa4-43d5-9ae7-fbbacaceafa7"));
    assert!(bloat_autopsy.contains("tool_output"));
    assert!(bloat_autopsy.contains("tool-output-bloat"));
    assert!(bloat_autopsy.contains("Bash"));
    assert!(bloat_autopsy.contains("unpriced"));
    assert!(!bloat_autopsy.contains("$0"));
    assert!(!bloat_autopsy.contains("/synthetic/workspace/bloat"));
    insta::assert_snapshot!("t08_autopsy_tool_bloat_turns", bloat_autopsy);

    // `--category` filter accepts both `tool_output` and `tool-output`.
    let cat_out = String::from_utf8(
        env.kakeibo_cmd()
            .args(["autopsy", "9162aea6", "--category", "tool-output"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert!(cat_out.contains("tool_output"));
    assert!(cat_out.contains("Bash"));

    // Unknown session or category fails cleanly.
    env.kakeibo_cmd()
        .args(["autopsy", "deadbeef"])
        .assert()
        .failure();
    env.kakeibo_cmd()
        .args(["autopsy", "9162aea6", "--category", "bogus"])
        .assert()
        .failure();

    // Subagent rollup test: stage a child subagent file under `<claude_root>/projects/<slug>/<parent>/subagents/`.
    drop(_ro);
    let sub_dir = env
        .home_dir()
        .join(".claude/projects/-synthetic-workspace-bloat/9162aea6-bfa4-43d5-9ae7-fbbacaceafa7/subagents");
    fs::create_dir_all(&sub_dir).expect("create subagents dir");
    let sub_jsonl = concat!(
        r#"{"agentId":"sub01","cwd":"/synthetic/workspace/bloat","isMeta":false,"isSidechain":true,"message":{"content":"subagent prompt","role":"user"},"parentUuid":null,"sessionId":"9162aea6-bfa4-43d5-9ae7-fbbacaceafa7","timestamp":"2026-10-31T09:11:00Z","type":"user","version":"2.1.314"}"#,
        "\n",
        r#"{"agentId":"sub01","cwd":"/synthetic/workspace/bloat","isSidechain":true,"message":{"content":[{"text":"subagent reply","type":"text"}],"id":"msg_sub_1","model":"claude-haiku-4-5","role":"assistant","stop_reason":"end_turn","type":"message","usage":{"cache_creation_input_tokens":500,"cache_read_input_tokens":1500,"input_tokens":100,"output_tokens":60}},"requestId":"req_sub_1","sessionId":"9162aea6-bfa4-43d5-9ae7-fbbacaceafa7","timestamp":"2026-10-31T09:11:02Z","type":"assistant","uuid":"uuid-sub-1","version":"2.1.314"}"#,
        "\n"
    );
    fs::write(sub_dir.join("agent-sub-0001.jsonl"), sub_jsonl).expect("write subagent log");
    let _ro2 = env.make_logs_readonly();

    let with_sub_json: Value = serde_json::from_slice(
        &env.kakeibo_cmd()
            .args(["export", "9162aea6", "--format", "json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .expect("parse json with subagent");
    assert_eq!(
        with_sub_json["sessions"][0]["subagents"][0]["session_id"],
        "9162aea6-bfa4-43d5-9ae7-fbbacaceafa7:subagent:sub01"
    );
    assert_eq!(
        with_sub_json["sessions"][0]["categories"]["breakdown"]["subagents"]["total_tokens"],
        2160
    );
}

#[test]
fn cli_flags_and_top_leaderboards() {
    let basic_env = TestEnv::new();
    basic_env.stage_fixture("claude-code", "basic");
    basic_env.stage_fixture("codex", "basic");
    let _ro_basic = basic_env.make_logs_readonly();

    let quiet_flags = String::from_utf8(
        basic_env
            .kakeibo_cmd()
            .arg("flags")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert_eq!(quiet_flags.trim(), "No flags.");

    let env = TestEnv::new();
    env.stage_fixture("claude-code", "basic");
    env.stage_fixture("claude-code", "tool-bloat");
    env.stage_fixture("claude-code", "resume-cold");
    env.stage_fixture("codex", "basic");
    let _ro = env.make_logs_readonly();

    // `flags`: `cold-resume` (122,110 impact tokens on 10f2c5a4) ranks above `tool-output-bloat` (120,031 on 9162aea6).
    let flags_out = String::from_utf8(
        env.kakeibo_cmd()
            .arg("flags")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    let cold_pos = flags_out.find("cold-resume").expect("cold-resume present");
    let bloat_pos = flags_out
        .find("tool-output-bloat")
        .expect("tool-output-bloat present");
    assert!(
        cold_pos < bloat_pos,
        "higher-impact cold-resume must rank before tool-output-bloat:\n{flags_out}"
    );
    assert!(flags_out.contains("10f2c5a4"));
    assert!(flags_out.contains("9162aea6"));

    // `top --by category|tool|project|flag`.
    let top_cat = String::from_utf8(
        env.kakeibo_cmd()
            .args(["top", "--by", "category"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert!(top_cat.contains("user_input"));
    assert!(top_cat.contains("tool_output"));
    assert!(top_cat.contains("fixed_overhead"));

    let top_tool = String::from_utf8(
        env.kakeibo_cmd()
            .args(["top", "--by", "tool"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    let bash_pos = top_tool.find("Bash").expect("Bash in top tools");
    let exec_pos = top_tool
        .find("exec_command")
        .expect("exec_command in top tools");
    assert!(
        bash_pos < exec_pos,
        "Bash (carried 160 KB dump) must rank above exec_command:\n{top_tool}"
    );

    let top_proj = String::from_utf8(
        env.kakeibo_cmd()
            .args(["top", "--by", "project"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert!(top_proj.contains("proj:"));
    assert!(!top_proj.contains("/synthetic/workspace/"));

    let top_flag = String::from_utf8(
        env.kakeibo_cmd()
            .args(["top", "--by", "flag"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8");
    assert!(top_flag.contains("cold-resume"));
    assert!(top_flag.contains("tool-output-bloat"));
}

#[test]
fn cli_export_json_matches_cli_schema_v1_and_golden() {
    let env = TestEnv::new();
    env.stage_fixture("claude-code", "tool-bloat");
    let _ro = env.make_logs_readonly();

    let raw_stdout = String::from_utf8(
        env.kakeibo_cmd()
            .args(["export", "9162aea6", "--format", "json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8 json");

    assert!(
        !raw_stdout.contains("/synthetic/workspace/bloat"),
        "default JSON export must be pathless"
    );

    let doc: Value = serde_json::from_str(&raw_stdout).expect("valid JSON document");
    assert_eq!(doc["schemaVersion"], 1);
    assert_eq!(doc["generator"]["name"], "kakeibo");
    assert_eq!(doc["generator"]["version"], "0.1.0");
    assert_eq!(doc["session_count"], 1);

    let sessions = doc["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 1);
    let sess = &sessions[0];

    assert_eq!(sess["agent"], "claude-code");
    assert_eq!(sess["session_id"], "9162aea6-bfa4-43d5-9ae7-fbbacaceafa7");
    assert_eq!(sess["turn_count"], 5);
    assert_eq!(sess["started_at"], "2026-10-31T09:10:04Z");
    assert_eq!(sess["ended_at"], "2026-10-31T09:22:42Z");
    assert_eq!(sess["models"], serde_json::json!(["claude-sonnet-4-5"]));
    assert_eq!(
        sess["project_hash"].as_str(),
        Some(hash_cwd("/synthetic/workspace/bloat").as_str())
    );
    assert!(
        sess.get("project_path").is_none(),
        "project_path must be omitted by default"
    );

    assert_eq!(sess["cost"]["status"], "unpriced");
    assert!(sess["cost"]["usd"].is_null());

    // Totals & exact 9-category reconciliation (`docs/cli-schema.md` §5-§6).
    let totals = &sess["totals"];
    let fresh_in = totals["fresh_in"].as_u64().expect("fresh_in");
    let cache_write = totals["cache_write"].as_u64().expect("cache_write");
    let cache_read = totals["cache_read"].as_u64().expect("cache_read");
    let output = totals["output"].as_u64().expect("output");
    let prompt_tokens = totals["prompt_tokens"].as_u64().expect("prompt_tokens");
    let total_tokens = totals["total_tokens"].as_u64().expect("total_tokens");
    assert_eq!(fresh_in + cache_write + cache_read, prompt_tokens);
    assert_eq!(prompt_tokens, 210_590);
    assert_eq!(prompt_tokens + output, total_tokens);
    assert_eq!(totals["confidence"], "measured");

    assert_eq!(sess["fixed_overhead"]["confidence"], "estimated");
    assert_eq!(
        sess["fixed_overhead"]["estimation_method"],
        "utf8_bytes_div4_ceil_scaled"
    );

    let breakdown = sess["categories"]["breakdown"]
        .as_object()
        .expect("categories.breakdown object");
    assert_eq!(breakdown.len(), 9);
    let (mut sum_f, mut sum_w, mut sum_r, mut sum_o) = (0_u64, 0_u64, 0_u64, 0_u64);
    for cat in ALL_NINE_CATEGORIES {
        let bucket = breakdown
            .get(cat)
            .unwrap_or_else(|| panic!("missing category `{cat}` in breakdown"));
        let bf = bucket["fresh_in"].as_u64().unwrap();
        let bw = bucket["cache_write"].as_u64().unwrap();
        let br = bucket["cache_read"].as_u64().unwrap();
        let bo = bucket["output"].as_u64().unwrap();
        assert_eq!(bf + bw + br, bucket["prompt_tokens"].as_u64().unwrap());
        assert_eq!(bf + bw + br + bo, bucket["total_tokens"].as_u64().unwrap());
        assert!(bucket["share_bps"].as_u64().unwrap() <= 10_000);
        sum_f += bf;
        sum_w += bw;
        sum_r += br;
        sum_o += bo;
    }
    assert_eq!(
        (sum_f, sum_w, sum_r, sum_o),
        (fresh_in, cache_write, cache_read, output)
    );

    let flags = sess["flags"].as_array().expect("flags array");
    assert_eq!(flags.len(), 1);
    assert_eq!(flags[0]["id"], "tool-output-bloat");
    assert_eq!(flags[0]["turn_range"]["start"], 2);
    assert_eq!(flags[0]["turn_range"]["end"], 5);
    assert!(!flags[0]["sentence"].as_str().unwrap().is_empty());
    assert!(flags[0]["evidence_text"]
        .as_str()
        .unwrap()
        .starts_with("evidence: "));
    assert_eq!(flags[0]["evidence"]["kind"], "tool-output-bloat");

    let tools = sess["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["tool"], "Bash");
    assert_eq!(tools[0]["calls"], 1);
    assert_eq!(tools[0]["results"], 1);

    let turns = sess["turns"].as_array().expect("turns array");
    assert_eq!(turns.len(), 5);

    insta::assert_snapshot!("t08_export_tool_bloat_json", raw_stdout);
}

#[test]
fn cli_export_markdown_pathless_by_default_show_paths_and_out_file() {
    let env = TestEnv::new();
    env.stage_fixture("claude-code", "tool-bloat");
    env.stage_fixture("codex", "basic");
    let _ro = env.make_logs_readonly();

    // 1. Default Markdown is strictly pathless (`PLAN.md` §6.4, §10 T08).
    let md_default = String::from_utf8(
        env.kakeibo_cmd()
            .args(["export", "9162aea6", "--format", "md"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8 md");

    let expected_hash = hash_cwd("/synthetic/workspace/bloat");
    assert!(
        !md_default.contains("/synthetic/workspace/bloat"),
        "default Markdown export MUST NOT contain raw project paths:\n{md_default}"
    );
    assert!(
        md_default.contains(&format!("proj:{expected_hash}")),
        "default Markdown export must show hashed project digest"
    );
    assert!(md_default.contains("unpriced"));
    assert!(!md_default.contains("$0"));
    assert!(md_default.contains("tool-output-bloat"));
    insta::assert_snapshot!("t08_export_tool_bloat_md", md_default);

    // 2. `--show-paths` explicitly reveals the raw `cwd`.
    let md_with_paths = String::from_utf8(
        env.kakeibo_cmd()
            .args(["export", "9162aea6", "--format", "md", "--show-paths"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8 md with paths");
    assert!(
        md_with_paths.contains("/synthetic/workspace/bloat"),
        "--show-paths must reveal raw project path when requested:\n{md_with_paths}"
    );

    // 3. `--since` + `--out <FILE>` writes cleanly while `$HOME` logs remain `chmod a-w`.
    let out_file = env.root_dir().join("exported-report.md");
    env.kakeibo_cmd()
        .args([
            "export",
            "--since",
            "2026-10-31T00:00:00Z",
            "--format",
            "md",
            "--out",
        ])
        .arg(&out_file)
        .assert()
        .success();

    let file_contents = fs::read_to_string(&out_file).expect("read --out markdown file");
    assert!(file_contents.contains("9162aea6-bfa4-43d5-9ae7-fbbacaceafa7"));
    assert!(file_contents.contains("0018a668-5865-4d4b-8b2f-56b827242e3d"));
    assert!(!file_contents.contains("/synthetic/workspace/"));
}

#[test]
fn cli_export_ndjson_round_trips_and_rejects_show_paths() {
    let env = TestEnv::new();
    let (_, expected) = env.stage_fixture("claude-code", "tool-bloat");
    let _ro = env.make_logs_readonly();

    let ndjson_bytes = env
        .kakeibo_cmd()
        .args(["export", "9162aea6", "--format", "ndjson"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ndjson_str = String::from_utf8(ndjson_bytes.clone()).expect("utf-8 ndjson");
    assert!(!ndjson_str.contains("/synthetic/workspace/bloat"));

    let records = read_ledger(Cursor::new(ndjson_bytes)).expect("valid kakeibo.ledger/1 stream");
    assert_eq!(records.len(), expected.sessions[0].turns.len());
    assert_eq!(
        records[0].session_id,
        "9162aea6-bfa4-43d5-9ae7-fbbacaceafa7"
    );

    // `kakeibo.ledger/1` is unconditionally pathless (`docs/ledger-schema.md` §2).
    env.kakeibo_cmd()
        .args(["export", "9162aea6", "--format", "ndjson", "--show-paths"])
        .assert()
        .failure();
}

#[test]
fn cli_doctor_reports_sources_versions_unknown_records_and_issues_without_raw_paths() {
    let env = TestEnv::new();
    env.stage_fixture("claude-code", "basic");
    env.stage_fixture("codex", "basic");

    // Add a synthetic Claude log with an unknown record type and a truncated JSON line.
    let hostile_dir = env
        .home_dir()
        .join(".claude/projects/-synthetic-workspace-doctor");
    fs::create_dir_all(&hostile_dir).expect("create doctor slug dir");
    let hostile_jsonl = concat!(
        r#"{"type":"future_telemetry_ping","version":"2.1.314","timestamp":"2026-10-31T11:00:00Z"}"#,
        "\n",
        r#"{"type":"assistant","truncated":true"#,
        "\n"
    );
    fs::write(hostile_dir.join("doctor-probe.jsonl"), hostile_jsonl).expect("write doctor probe");

    let _ro = env.make_logs_readonly();

    let doctor_out = String::from_utf8(
        env.kakeibo_cmd()
            .arg("doctor")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf-8 doctor output");

    assert!(doctor_out.contains("claude-code"));
    assert!(doctor_out.contains("codex"));
    assert!(doctor_out.contains("2.1.314"));
    assert!(doctor_out.contains("0.165.0"));
    assert!(doctor_out.contains("future_telemetry_ping"));
    assert!(
        !doctor_out.contains(&env.home_dir().to_string_lossy().to_string()),
        "doctor output must never leak $HOME filesystem paths:\n{doctor_out}"
    );
}
