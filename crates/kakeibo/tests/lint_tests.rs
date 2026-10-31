//! Verification tests for `scripts/lint-privacy.sh` and `scripts/lint-fixtures.sh`.
//!
//! Ensures both lints pass on the clean workspace and reliably fail when
//! forbidden networking tokens or personal fixture paths/emails/keys are planted.

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
