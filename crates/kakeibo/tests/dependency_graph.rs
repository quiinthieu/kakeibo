//! Architectural dependency graph and purity enforcement tests (`DependencyGraphTests`).
//!
//! Verifies `PLAN.md` §3.1:
//! `kakeibo -> {core, parse, index, report}`; `parse`, `index`, `report -> core`; `core ->` nothing.
//! Also verifies `PLAN.md` §0 #3 (`kakeibo-core` is pure: no I/O, clock, or env).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const WORKSPACE_CRATES: &[&str] = &[
    "kakeibo-core",
    "kakeibo-parse",
    "kakeibo-index",
    "kakeibo-report",
    "kakeibo",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must resolve")
}

fn parse_toml(path: &Path) -> toml::Value {
    let raw = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    toml::from_str(&raw).unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

fn collect_dep_names(table: Option<&toml::Value>, out: &mut BTreeSet<String>) {
    if let Some(toml::Value::Table(map)) = table {
        for key in map.keys() {
            out.insert(key.clone());
        }
    }
}

fn all_declared_dependencies(manifest: &toml::Value) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    collect_dep_names(manifest.get("dependencies"), &mut deps);
    collect_dep_names(manifest.get("dev-dependencies"), &mut deps);
    collect_dep_names(manifest.get("build-dependencies"), &mut deps);

    if let Some(toml::Value::Table(targets)) = manifest.get("target") {
        for target_cfg in targets.values() {
            collect_dep_names(target_cfg.get("dependencies"), &mut deps);
            collect_dep_names(target_cfg.get("dev-dependencies"), &mut deps);
            collect_dep_names(target_cfg.get("build-dependencies"), &mut deps);
        }
    }
    deps
}

fn internal_dependencies(manifest: &toml::Value) -> BTreeSet<String> {
    let workspace_set: BTreeSet<&str> = WORKSPACE_CRATES.iter().copied().collect();
    all_declared_dependencies(manifest)
        .into_iter()
        .filter(|dep| workspace_set.contains(dep.as_str()))
        .collect()
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("failed to read dir {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("valid dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn workspace_members_match_plan_architecture() {
    let root_manifest = parse_toml(&workspace_root().join("Cargo.toml"));
    let members = root_manifest
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
        .expect("root Cargo.toml must declare [workspace.members]");

    let actual: BTreeSet<&str> = members
        .iter()
        .map(|v| v.as_str().expect("member entry must be a string"))
        .collect();

    let expected: BTreeSet<&str> = [
        "crates/kakeibo-core",
        "crates/kakeibo-parse",
        "crates/kakeibo-index",
        "crates/kakeibo-report",
        "crates/kakeibo",
    ]
    .into_iter()
    .collect();

    assert_eq!(
        actual, expected,
        "workspace members must match PLAN.md §3.1"
    );
}

#[test]
fn internal_crate_dependency_graph_is_enforced() {
    let root = workspace_root();
    let expected_edges: &[(&str, &[&str])] = &[
        ("kakeibo-core", &[]),
        ("kakeibo-parse", &["kakeibo-core"]),
        ("kakeibo-index", &["kakeibo-core"]),
        ("kakeibo-report", &["kakeibo-core"]),
        (
            "kakeibo",
            &[
                "kakeibo-core",
                "kakeibo-parse",
                "kakeibo-index",
                "kakeibo-report",
            ],
        ),
    ];

    for (crate_name, allowed_internal) in expected_edges {
        let manifest_path = root.join("crates").join(crate_name).join("Cargo.toml");
        let manifest = parse_toml(&manifest_path);

        let pkg_name = manifest
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
            .expect("package.name must exist");
        assert_eq!(pkg_name, *crate_name);

        let actual_internal = internal_dependencies(&manifest);
        let expected_internal: BTreeSet<String> =
            allowed_internal.iter().map(|s| (*s).to_string()).collect();

        assert_eq!(
            actual_internal, expected_internal,
            "crate `{crate_name}` violated architectural dependency graph (`PLAN.md` §3.1)"
        );
    }
}

#[test]
fn kakeibo_core_is_pure_without_io_clock_env_or_net() {
    let root = workspace_root();
    let core_manifest = parse_toml(&root.join("crates/kakeibo-core/Cargo.toml"));
    let deps = all_declared_dependencies(&core_manifest);

    let forbidden_core_crates = [
        "clap",
        "crossterm",
        "directories",
        "ratatui",
        "rayon",
        "rusqlite",
        "tempfile",
        "zstd",
        concat!("req", "west"),
        concat!("hy", "per"),
        concat!("ur", "eq"),
    ];

    for banned in forbidden_core_crates {
        assert!(
            !deps.contains(banned),
            "kakeibo-core must remain pure and cannot depend on `{banned}`"
        );
    }

    let mut rs_files = Vec::new();
    collect_rs_files(&root.join("crates/kakeibo-core/src"), &mut rs_files);
    assert!(
        !rs_files.is_empty(),
        "kakeibo-core/src must contain Rust source files"
    );

    let forbidden_std_modules = [
        "std::fs",
        "std::io",
        "std::env",
        "std::process",
        "SystemTime",
        "Instant::now",
        concat!("std", "::", "net"),
    ];

    for file in rs_files {
        let src = fs::read_to_string(&file).expect("readable core source file");
        for needle in forbidden_std_modules {
            assert!(
                !src.contains(needle),
                "kakeibo-core purity violation in {}: found `{needle}`",
                file.display()
            );
        }
    }
}

#[test]
fn product_names_are_not_hardcoded_outside_product_rs() {
    let root = workspace_root();
    let product_rs = root
        .join("crates/kakeibo-core/src/product.rs")
        .canonicalize()
        .expect("product.rs must exist");

    let mut rs_files = Vec::new();
    for crate_name in WORKSPACE_CRATES {
        collect_rs_files(
            &root.join("crates").join(crate_name).join("src"),
            &mut rs_files,
        );
    }

    let forbidden_literals = [
        "\"Kakeibo\"",
        "\"kakeibo\"",
        "\"kakeibo-bar\"",
        "\"io.github.quiinthieu.kakeibo\"",
    ];

    for file in rs_files {
        if file.canonicalize().expect("canonicalize file") == product_rs {
            continue;
        }
        let src = fs::read_to_string(&file).expect("readable source file");
        for literal in forbidden_literals {
            assert!(
                !src.contains(literal),
                "hardcoded product string {literal} found in {}; reference `kakeibo_core::product` instead",
                file.display()
            );
        }
    }
}
