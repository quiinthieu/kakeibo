//! CLI `--version` integration tests verifying `product.rs` is the source of truth.

use assert_cmd::Command;
use kakeibo_core::product;

#[test]
fn cli_version_flag_outputs_product_version_string() {
    let output = Command::new(env!("CARGO_BIN_EXE_kakeibo"))
        .arg("--version")
        .output()
        .expect("failed to execute kakeibo --version");

    assert!(
        output.status.success(),
        "kakeibo --version exited with non-zero status: {:?}",
        output.status
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout must be valid UTF-8");
    assert_eq!(
        stdout.trim(),
        product::version_string(),
        "--version output must come from kakeibo_core::product"
    );
}

#[test]
fn cli_short_version_flag_outputs_product_version_string() {
    let output = Command::new(env!("CARGO_BIN_EXE_kakeibo"))
        .arg("-V")
        .output()
        .expect("failed to execute kakeibo -V");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout must be valid UTF-8");
    assert_eq!(stdout.trim(), product::version_string());
}
