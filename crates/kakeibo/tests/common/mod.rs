//! Isolated `TestEnv` harness (temp `HOME`) and fixture ground-truth types (`PLAN.md` §0 #5, §9, T02).
//!
//! Guarantees tests never read from or write to the host user's `$HOME`.

#![allow(dead_code)]

use assert_cmd::Command;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedGroundTruth {
    pub schema: String,
    pub agent: String,
    pub scenario: String,
    pub seed: u64,
    pub log_files: Vec<String>,
    pub sessions: Vec<ExpectedSession>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedSession {
    pub session_id: String,
    pub model: String,
    pub ignored_records: u64,
    pub turns: Vec<ExpectedTurn>,
    pub totals: ExpectedTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedTurn {
    pub turn: u32,
    pub ts: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub usage: ExpectedUsage,
    pub added_blocks: Vec<ExpectedBlock>,
    pub hints: ExpectedHints,
    pub rate_limit_used_percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedUsage {
    pub fresh_in: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    pub reasoning: Option<u64>,
    pub confidence: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedBlock {
    pub kind: String,
    pub bytes: u64,
    pub tool: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedHints {
    pub resume_start: bool,
    pub compaction: bool,
    pub sidechain: bool,
    pub api_error: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedTotals {
    pub turns: u32,
    pub prompt_tokens: u64,
    pub fresh_in: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    pub reasoning: u64,
}

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must resolve")
}

/// Isolated temporary environment providing synthetic `HOME` and XDG directories.
pub struct TestEnv {
    temp: TempDir,
    home: PathBuf,
    xdg_config: PathBuf,
    xdg_cache: PathBuf,
    xdg_data: PathBuf,
}

impl TestEnv {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("create TestEnv tempdir");
        let home = temp.path().join("home");
        let xdg_config = temp.path().join("xdg_config");
        let xdg_cache = temp.path().join("xdg_cache");
        let xdg_data = temp.path().join("xdg_data");

        for dir in [&home, &xdg_config, &xdg_cache, &xdg_data] {
            fs::create_dir_all(dir).expect("create isolated TestEnv directory");
        }

        Self {
            temp,
            home,
            xdg_config,
            xdg_cache,
            xdg_data,
        }
    }

    pub fn root_dir(&self) -> &Path {
        self.temp.path()
    }

    pub fn home_dir(&self) -> &Path {
        &self.home
    }

    pub fn config_dir(&self) -> &Path {
        &self.xdg_config
    }

    pub fn cache_dir(&self) -> &Path {
        &self.xdg_cache
    }

    pub fn data_dir(&self) -> &Path {
        &self.xdg_data
    }

    /// Copies a synthetic fixture scenario into this `TestEnv`'s temp `HOME`
    /// (`home/.claude/` or `home/.codex/`) and parses its `expected.json`.
    pub fn stage_fixture(
        &self,
        agent: &str,
        scenario: &str,
    ) -> (Vec<PathBuf>, ExpectedGroundTruth) {
        let scenario_dir = workspace_root()
            .join("tests/fixtures")
            .join(agent)
            .join(scenario);
        assert!(
            scenario_dir.is_dir(),
            "missing fixture scenario directory: {}",
            scenario_dir.display()
        );

        let expected_raw = fs::read_to_string(scenario_dir.join("expected.json"))
            .expect("read scenario expected.json");
        let expected: ExpectedGroundTruth =
            serde_json::from_str(&expected_raw).expect("valid expected.json");

        let agent_home_subdir = match agent {
            "claude-code" => self.home.join(".claude"),
            "codex" => self.home.join(".codex"),
            other => panic!("unsupported fixture agent `{other}`"),
        };

        let mut staged_paths = Vec::with_capacity(expected.log_files.len());
        for rel in &expected.log_files {
            let src = scenario_dir.join(rel);
            let dst = agent_home_subdir.join(rel);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).expect("create staged log parent directory");
            }
            fs::copy(&src, &dst).unwrap_or_else(|e| {
                panic!(
                    "failed to stage fixture {} -> {}: {e}",
                    src.display(),
                    dst.display()
                )
            });
            staged_paths.push(dst);
        }

        (staged_paths, expected)
    }

    /// Recursively removes write permissions on the synthetic `HOME` tree to prove
    /// decoders open agent logs strictly read-only (`PLAN.md` §0 #5 & §9).
    pub fn make_logs_readonly(&self) -> ReadOnlyGuard {
        set_tree_readonly_postorder(&self.home);
        ReadOnlyGuard {
            target: self.home.clone(),
        }
    }

    /// Builds a `kakeibo` CLI command strictly isolated to this `TestEnv`.
    pub fn kakeibo_cmd(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_kakeibo"));
        cmd.current_dir(self.root_dir())
            .env("HOME", self.home_dir())
            .env("USERPROFILE", self.home_dir())
            .env("XDG_CONFIG_HOME", self.config_dir())
            .env("XDG_CACHE_HOME", self.cache_dir())
            .env("XDG_DATA_HOME", self.data_dir())
            .env("NO_COLOR", "1")
            .env("TZ", "UTC")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME");
        cmd
    }
}

pub struct ReadOnlyGuard {
    target: PathBuf,
}

impl Drop for ReadOnlyGuard {
    fn drop(&mut self) {
        restore_tree_writable_preorder(&self.target);
    }
}

fn set_tree_readonly_postorder(path: &Path) {
    if path.is_dir() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                set_tree_readonly_postorder(&entry.path());
            }
        }
        set_mode(path, true, true);
    } else {
        set_mode(path, false, true);
    }
}

fn restore_tree_writable_preorder(path: &Path) {
    if path.is_dir() {
        set_mode(path, true, false);
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                restore_tree_writable_preorder(&entry.path());
            }
        }
    } else {
        set_mode(path, false, false);
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, is_dir: bool, readonly: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = match (is_dir, readonly) {
        (true, true) => 0o555,
        (true, false) => 0o755,
        (false, true) => 0o444,
        (false, false) => 0o644,
    };
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(path: &Path, _is_dir: bool, readonly: bool) {
    if let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_readonly(readonly);
        let _ = fs::set_permissions(path, perms);
    }
}
