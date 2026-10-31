//! T09 end-to-end CLI tests (`assert_cmd` + `insta`) for `kakeibo pricing list|check|import`,
//! unpriced-never-shows-`$0` invariants, and `5m`/`1h` prompt-cache write split pricing across
//! terminal tables, Markdown exports, and `schemaVersion: 1` JSON exports.

mod common;

use std::fs;

use common::{workspace_root, TestEnv};

#[test]
fn unpriced_default_never_shows_dollar_zero_across_all_surfaces() {
    let env = TestEnv::new();
    env.stage_fixture("claude-code", "tool-bloat");
    env.stage_fixture("codex", "basic");
    let _readonly = env.make_logs_readonly();

    let commands: &[&[&str]] = &[
        &["pricing", "list"],
        &["pricing", "check"],
        &[],
        &["sessions"],
        &["autopsy", "latest", "--turns"],
        &["top", "--by", "project"],
        &["export", "latest", "--format", "md"],
    ];

    for args in commands {
        let out = env
            .kakeibo_cmd()
            .env("KAKEIBO_TEST_NOW", "2026-10-31T15:00:00Z")
            .args(*args)
            .output()
            .expect("run kakeibo command");
        assert!(
            out.status.success(),
            "command {args:?} failed:\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
        assert!(
            stdout.contains("unpriced"),
            "command {args:?} must surface `unpriced`, got:\n{stdout}"
        );
        assert!(
            !stdout.contains("$0"),
            "command {args:?} must NEVER display `$0` for unpriced models/sessions:\n{stdout}"
        );
    }

    let json_out = env
        .kakeibo_cmd()
        .env("KAKEIBO_TEST_NOW", "2026-10-31T15:00:00Z")
        .args(["export", "--since", "365d", "--format", "json"])
        .output()
        .expect("export json");
    assert!(json_out.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&json_out.stdout).expect("valid JSON export");
    let sessions = doc["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 2);
    for sess in sessions {
        assert_eq!(sess["cost"]["status"], "unpriced");
        assert!(sess["cost"]["usd"].is_null());
    }
}

#[test]
fn pricing_import_tiny_litellm_prices_5m_1h_turns_and_categories_while_keeping_unknown_unpriced() {
    let env = TestEnv::new();

    // Write two synthetic Claude Code sessions into temp HOME before locking HOME read-only:
    //   1. `sess-priced-5m1h` on `claude-sonnet-4-5` with explicit 5m + 1h cache-creation split
    //   2. `sess-unpriced-custom` on `synth-custom-unpriced-v1` (must remain `unpriced`, never `$0`)
    let project_dir = env
        .home_dir()
        .join(".claude/projects/-synthetic-pricing-workspace");
    fs::create_dir_all(&project_dir).expect("create synthetic claude project dir");

    let priced_jsonl = concat!(
        r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"sess-priced-5m1h","timestamp":"2026-10-31T12:00:00Z","cwd":"/synthetic/pricing-ws","message":{"role":"user","content":"Audit module A"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"sess-priced-5m1h","requestId":"req_p1","timestamp":"2026-10-31T12:00:02Z","cwd":"/synthetic/pricing-ws","message":{"id":"msg_p1","type":"message","role":"assistant","model":"claude-sonnet-4-5","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_p1","name":"Read","input":{"file_path":"/synthetic/pricing-ws/src/lib.rs"}}],"usage":{"input_tokens":1000,"cache_creation_input_tokens":30000,"cache_creation":{"ephemeral_5m_input_tokens":10000,"ephemeral_1h_input_tokens":20000},"cache_read_input_tokens":0,"output_tokens":500}}}"#,
        "\n",
        r#"{"type":"user","uuid":"u2","parentUuid":"a1","sessionId":"sess-priced-5m1h","timestamp":"2026-10-31T12:00:04Z","cwd":"/synthetic/pricing-ws","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_p1","is_error":false,"content":"pub fn demo() -> u64 { 42 }"}]}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a2","parentUuid":"u2","sessionId":"sess-priced-5m1h","requestId":"req_p2","timestamp":"2026-10-31T12:00:06Z","cwd":"/synthetic/pricing-ws","message":{"id":"msg_p2","type":"message","role":"assistant","model":"claude-sonnet-4-5","stop_reason":"end_turn","content":[{"type":"text","text":"Audit complete."}],"usage":{"input_tokens":500,"cache_creation_input_tokens":2000,"cache_creation":{"ephemeral_5m_input_tokens":2000,"ephemeral_1h_input_tokens":0},"cache_read_input_tokens":31000,"output_tokens":300}}}"#,
        "\n"
    );
    fs::write(project_dir.join("sess-priced-5m1h.jsonl"), priced_jsonl)
        .expect("write priced session jsonl");

    let unpriced_jsonl = concat!(
        r#"{"type":"user","uuid":"u9","parentUuid":null,"sessionId":"sess-unpriced-custom","timestamp":"2026-10-31T12:05:00Z","cwd":"/synthetic/pricing-ws","message":{"role":"user","content":"Ping"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a9","parentUuid":"u9","sessionId":"sess-unpriced-custom","requestId":"req_u9","timestamp":"2026-10-31T12:05:01Z","cwd":"/synthetic/pricing-ws","message":{"id":"msg_u9","type":"message","role":"assistant","model":"synth-custom-unpriced-v1","stop_reason":"end_turn","content":[{"type":"text","text":"Pong"}],"usage":{"input_tokens":800,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":100}}}"#,
        "\n"
    );
    fs::write(
        project_dir.join("sess-unpriced-custom.jsonl"),
        unpriced_jsonl,
    )
    .expect("write unpriced custom session jsonl");

    // Lock `$HOME` read-only; `pricing import` must write strictly into `XDG_CONFIG_HOME/kakeibo/pricing.toml`.
    let _readonly = env.make_logs_readonly();

    let litellm_fixture = workspace_root().join("tests/fixtures/pricing/litellm-tiny.json");
    let import_out = env
        .kakeibo_cmd()
        .arg("pricing")
        .arg("import")
        .arg(&litellm_fixture)
        .args(["--checked", "2026-10-31"])
        .output()
        .expect("run pricing import");
    assert!(
        import_out.status.success(),
        "pricing import failed:\nstderr: {}",
        String::from_utf8_lossy(&import_out.stderr)
    );
    let import_stdout = String::from_utf8(import_out.stdout).expect("utf8");
    assert!(import_stdout.contains("Imported 4 priced model(s)"));
    assert!(env.config_dir().join("kakeibo/pricing.toml").is_file());

    // 1. `pricing list` shows imported models as `configured` and bundled `gemini-2.5-pro` as `unpriced`.
    let list_out = env
        .kakeibo_cmd()
        .args(["pricing", "list"])
        .output()
        .expect("run pricing list");
    assert!(list_out.status.success());
    let list_stdout = String::from_utf8(list_out.stdout).expect("utf8");
    assert!(list_stdout.contains("claude-sonnet-4-5"));
    assert!(list_stdout.contains("configured"));
    assert!(list_stdout.contains("gemini-2.5-pro"));
    assert!(list_stdout.contains("unpriced"));
    assert!(!list_stdout.contains("$0"));

    // 2. `pricing check` flags `synth-custom-unpriced-v1` as `unpriced` and `claude-sonnet-4-5` as `configured`.
    let check_out = env
        .kakeibo_cmd()
        .args(["pricing", "check"])
        .output()
        .expect("run pricing check");
    assert!(check_out.status.success());
    let check_stdout = String::from_utf8(check_out.stdout).expect("utf8");
    assert!(check_stdout.contains("claude-sonnet-4-5"));
    assert!(check_stdout.contains("configured"));
    assert!(check_stdout.contains("synth-custom-unpriced-v1"));
    assert!(check_stdout.contains("unpriced"));
    assert!(!check_stdout.contains("$0"));

    // 3. Hand-verify exact cost of `sess-priced-5m1h` (`claude-sonnet-4-5` rates from `litellm-tiny.json`:
    //    in=$2/1M, w5m=$2.50/1M, w1h=$4/1M, read=$0.20/1M, out=$10/1M):
    //    Turn 1: 1,000*$2 + 10,000*$2.50 + 20,000*$4.00 + 0*$0.20 + 500*$10 = $0.002 + $0.025 + $0.080 + $0.005 = $0.1120
    //    Turn 2: 500*$2 + 2,000*$2.50 + 0*$4.00 + 31,000*$0.20 + 300*$10   = $0.001 + $0.005 + $0.0062 + $0.003 = $0.0152
    //    Total session API-equivalent cost = $0.1120 + $0.0152 = $0.1272
    let json_priced_out = env
        .kakeibo_cmd()
        .args(["export", "sess-priced-5m1h", "--format", "json"])
        .output()
        .expect("export priced json");
    assert!(json_priced_out.status.success());
    let priced_doc: serde_json::Value =
        serde_json::from_slice(&json_priced_out.stdout).expect("valid json");
    let sess = &priced_doc["sessions"][0];
    assert_eq!(sess["cost"]["status"], "configured");
    assert_eq!(sess["cost"]["currency"], "USD");
    let total_usd = sess["cost"]["usd"].as_f64().expect("numeric usd");
    assert!((total_usd - 0.1272).abs() < 1e-9);

    let turns = sess["turns"].as_array().expect("turns");
    assert_eq!(turns[0]["cache_write_5m_tokens"], 10_000);
    assert_eq!(turns[0]["cache_write_1h_tokens"], 20_000);
    assert!((turns[0]["cost_usd"].as_f64().unwrap() - 0.1120).abs() < 1e-9);
    assert!((turns[1]["cost_usd"].as_f64().unwrap() - 0.0152).abs() < 1e-9);

    let breakdown = sess["categories"]["breakdown"]
        .as_object()
        .expect("breakdown map");
    let cat_cost_sum: f64 = breakdown
        .values()
        .map(|b| b["cost_usd"].as_f64().expect("category cost_usd"))
        .sum();
    assert!((cat_cost_sum - 0.1272).abs() < 1e-9);

    // 4. Autopsy golden snapshot with per-category and per-turn costs.
    let autopsy_out = env
        .kakeibo_cmd()
        .args(["autopsy", "sess-priced-5m1h", "--turns"])
        .output()
        .expect("run priced autopsy");
    assert!(autopsy_out.status.success());
    let autopsy_stdout = String::from_utf8(autopsy_out.stdout).expect("utf8");
    insta::assert_snapshot!("t09_priced_autopsy_5m_1h_turns", autopsy_stdout);

    // 5. Unpriced custom session STILL exports as `unpriced` (`usd: null`) and never `$0` in Markdown.
    let unpriced_json_out = env
        .kakeibo_cmd()
        .args(["export", "sess-unpriced-custom", "--format", "json"])
        .output()
        .expect("export unpriced custom json");
    assert!(unpriced_json_out.status.success());
    let unpriced_doc: serde_json::Value =
        serde_json::from_slice(&unpriced_json_out.stdout).expect("valid json");
    assert_eq!(unpriced_doc["sessions"][0]["cost"]["status"], "unpriced");
    assert!(unpriced_doc["sessions"][0]["cost"]["usd"].is_null());

    let unpriced_md_out = env
        .kakeibo_cmd()
        .args(["export", "sess-unpriced-custom", "--format", "md"])
        .output()
        .expect("export unpriced custom md");
    assert!(unpriced_md_out.status.success());
    let unpriced_md = String::from_utf8(unpriced_md_out.stdout).expect("utf8");
    assert!(unpriced_md.contains("unpriced"));
    assert!(!unpriced_md.contains("$0"));
}
