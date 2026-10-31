//! Verification tests for `scripts/lint-privacy.sh`, `scripts/lint-fixtures.sh`,
//! and `scripts/lint-pricing.sh`.
//!
//! Ensures all three lints pass on the clean workspace and reliably fail when
//! forbidden networking tokens, personal fixture paths/emails/keys, or hardcoded
//! price literals are planted.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::tempdir;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must resolve")
}

fn run_script(script_rel_path: &str, arg: Option<&Path>) -> Output {
    let script_path = workspace_root().join(script_rel_path);
    let mut cmd = Command::new(&script_path);
    if let Some(target) = arg {
        cmd.arg(target);
    }
    cmd.output()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", script_path.display()))
}

#[test]
fn privacy_lint_passes_on_clean_workspace() {
    let output = run_script("scripts/lint-privacy.sh", None);
    assert!(
        output.status.success(),
        "lint-privacy.sh failed on clean workspace:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn privacy_lint_catches_planted_std_net() {
    let tmp = tempdir().expect("create temp dir");
    let src_dir = tmp.path().join("crates/rogue/src");
    fs::create_dir_all(&src_dir).expect("create rogue crate dir");

    let planted = concat!("use std", "::", "net::TcpStream;\n");
    fs::write(src_dir.join("lib.rs"), planted).expect("write planted source");

    let output = run_script("scripts/lint-privacy.sh", Some(tmp.path()));
    assert!(
        !output.status.success(),
        "lint-privacy.sh must fail when planted std networking import is present"
    );
}

#[test]
fn privacy_lint_catches_planted_url_session_and_network_crates() {
    // 1. Swift Apple networking session API
    let tmp_swift = tempdir().expect("create temp dir");
    let macos_dir = tmp_swift.path().join("macos");
    fs::create_dir_all(&macos_dir).expect("create macos dir");
    fs::write(
        macos_dir.join("Fetcher.swift"),
        concat!("let s = URL", "Session.shared\n"),
    )
    .expect("write planted swift file");

    let out_swift = run_script("scripts/lint-privacy.sh", Some(tmp_swift.path()));
    assert!(
        !out_swift.status.success(),
        "lint-privacy.sh must fail when Swift networking session API is present"
    );

    // 2. Banned crate in Cargo.toml
    let tmp_cargo = tempdir().expect("create temp dir");
    fs::write(
        tmp_cargo.path().join("Cargo.toml"),
        concat!("[dependencies]\nreq", "west = \"0.12\"\n"),
    )
    .expect("write planted Cargo.toml");

    let out_cargo = run_script("scripts/lint-privacy.sh", Some(tmp_cargo.path()));
    assert!(
        !out_cargo.status.success(),
        "lint-privacy.sh must fail when banned network crate appears in Cargo.toml"
    );
}

#[test]
fn fixtures_lint_passes_on_clean_workspace_and_synthetic_fixture() {
    let clean_ws = run_script("scripts/lint-fixtures.sh", None);
    assert!(
        clean_ws.status.success(),
        "lint-fixtures.sh failed on clean workspace:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&clean_ws.stdout),
        String::from_utf8_lossy(&clean_ws.stderr)
    );

    let tmp = tempdir().expect("create temp dir");
    fs::write(
        tmp.path().join("basic.jsonl"),
        "{\"cwd\":\"/synthetic/project\",\"contact\":\"author@example.com\"}\n",
    )
    .expect("write clean synthetic fixture");

    let clean_fixture = run_script("scripts/lint-fixtures.sh", Some(tmp.path()));
    assert!(
        clean_fixture.status.success(),
        "lint-fixtures.sh must allow clean synthetic fixtures:\nstderr: {}",
        String::from_utf8_lossy(&clean_fixture.stderr)
    );
}

#[test]
fn fixtures_lint_catches_planted_users_alice() {
    let tmp = tempdir().expect("create temp dir");
    let planted = concat!("{\"cwd\":\"", "/Users/", "alice/projects/secret-app\"}\n");
    fs::write(tmp.path().join("leaked.jsonl"), planted).expect("write planted fixture");

    let output = run_script("scripts/lint-fixtures.sh", Some(tmp.path()));
    assert!(
        !output.status.success(),
        "lint-fixtures.sh must fail when planted /Users/alice is present"
    );
}

#[test]
fn fixtures_lint_catches_linux_home_real_email_and_api_key() {
    let cases = [
        (
            "linux_home.jsonl",
            concat!("{\"cwd\":\"/ho", "me/bob/repo\"}\n"),
        ),
        (
            "email.jsonl",
            concat!("{\"user\":\"alice", "@", "acmecorp.io\"}\n"),
        ),
        (
            "apikey.jsonl",
            concat!("{\"key\":\"sk-", "ant-api03-abcdefghijklmnop\"}\n"),
        ),
    ];

    for (filename, content) in cases {
        let tmp = tempdir().expect("create temp dir");
        fs::write(tmp.path().join(filename), content).expect("write planted fixture");
        let output = run_script("scripts/lint-fixtures.sh", Some(tmp.path()));
        assert!(
            !output.status.success(),
            "lint-fixtures.sh must fail on {filename}"
        );
    }
}

#[test]
fn pricing_lint_passes_on_clean_workspace() {
    let output = run_script("scripts/lint-pricing.sh", None);
    assert!(
        output.status.success(),
        "lint-pricing.sh failed on clean workspace:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn pricing_lint_catches_planted_price_literals_in_rust_source() {
    let cases = [
        (
            "per_token_decimal.rs",
            concat!("pub const SONNET_IN: f64 = 0.000", "003;\n"),
        ),
        (
            "per_token_sci.rs",
            concat!("pub const OPUS_OUT: f64 = 7.5", "e-5;\n"),
        ),
        (
            "rate_assignment.rs",
            concat!("fn bad() { let input_rate = ", "3.00; }\n"),
        ),
        (
            "model_arm.rs",
            concat!(
                "fn price(m: &str) -> f64 { match m { \"claude-sonnet-4-5\" => ",
                "3.00, _ => 0.0 } }\n"
            ),
        ),
    ];

    for (filename, content) in cases {
        let tmp = tempdir().expect("create temp dir");
        let src_dir = tmp.path().join("crates/rogue/src");
        fs::create_dir_all(&src_dir).expect("create rogue crate dir");
        fs::write(src_dir.join(filename), content).expect("write planted source");

        let output = run_script("scripts/lint-pricing.sh", Some(tmp.path()));
        assert!(
            !output.status.success(),
            "lint-pricing.sh must fail when hardcoded price literal is planted in {filename}"
        );
    }
}

#[test]
fn pricing_lint_catches_non_empty_rates_in_bundled_pricing_toml() {
    let tmp = tempdir().expect("create temp dir");
    let data_dir = tmp.path().join("data");
    fs::create_dir_all(&data_dir).expect("create data dir");
    fs::write(
        data_dir.join("pricing.toml"),
        concat!(
            "schema_version = 1\n[models.\"claude-sonnet-4-5\"]\ncurrency = \"USD\"\ninput = ",
            "3.0\nsource = \"https://www.anthropic.com/pricing\"\nchecked = \"2026-10-31\"\n"
        ),
    )
    .expect("write planted pricing.toml");

    let output = run_script("scripts/lint-pricing.sh", Some(tmp.path()));
    assert!(
        !output.status.success(),
        "lint-pricing.sh must fail when bundled data/pricing.toml ships non-empty numeric rates"
    );
}
