//! T09 integration tests for `kakeibo-index` sole-writer user pricing store (`AppPaths`,
//! `load_effective_pricing`, `save_user_pricing`).

mod common;

use std::fs;

use common::{workspace_root, TestEnv};
use kakeibo_core::pricing::import_litellm_json_str;
use kakeibo_index::{load_effective_pricing, save_user_pricing, AppPaths};

#[test]
fn pricing_store_writes_atomically_inside_isolated_config_dir_and_overlays_bundled() {
    let env = TestEnv::new();
    let _readonly_home = env.make_logs_readonly();

    let paths = AppPaths::new(
        env.config_dir().join("kakeibo"),
        env.cache_dir().join("kakeibo"),
    );

    let initial = load_effective_pricing(&paths).expect("load bundled pricing when no user file");
    assert!(!initial.has_user_overlay);
    assert!(initial
        .catalog
        .resolve_priced(Some("claude-sonnet-4-5"))
        .is_none());

    let raw_litellm =
        fs::read_to_string(workspace_root().join("tests/fixtures/pricing/litellm-tiny.json"))
            .expect("read litellm-tiny.json");
    let imported = import_litellm_json_str(&raw_litellm, "2026-10-31").expect("import");

    // Must succeed even while `$HOME` is chmod read-only (`ReadOnlyGuard`), writing strictly
    // inside `paths.config_dir()`.
    let saved_path = save_user_pricing(&paths, &imported.catalog).expect("save user pricing");
    assert_eq!(saved_path, paths.user_pricing_path());
    assert!(saved_path.is_file());

    let reloaded = load_effective_pricing(&paths).expect("reload effective pricing");
    assert!(reloaded.has_user_overlay);
    let (_, sonnet) = reloaded
        .catalog
        .resolve_priced(Some("claude-sonnet-4-5"))
        .expect("sonnet now priced");
    assert_eq!(sonnet.input, Some(2.0));
    assert_eq!(sonnet.cache_write_1h, Some(4.0));

    // Bundled unpriced models not in LiteLLM import (`gemini-2.5-pro`) remain preserved as unpriced placeholders.
    let (_, gemini_placeholder) = reloaded
        .catalog
        .resolve_any(Some("gemini-2.5-pro"))
        .expect("bundled gemini placeholder preserved");
    assert!(!gemini_placeholder.is_priced());
}
