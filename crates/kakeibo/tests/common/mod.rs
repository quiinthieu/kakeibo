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

        if agent == "cursor" {
            let vscdb_path = self
                .home
                .join("Library/Application Support/Cursor/User/globalStorage/state.vscdb");
            if let Some(parent) = vscdb_path.parent() {
                fs::create_dir_all(parent).expect("create Cursor globalStorage directory");
            }
            materialize_cursor_vscdb(&scenario_dir.join("cursor_kv.json"), &vscdb_path);
            assert!(
                vscdb_path.is_file(),
                "expected staged Cursor state.vscdb missing: {}",
                vscdb_path.display()
            );
            return (vec![vscdb_path], expected);
        }

        let agent_home_subdir = match agent {
            "claude-code" => self.home.join(".claude"),
            "codex" => self.home.join(".codex"),
            "gemini-cli" => self.home.join(".gemini"),
            "opencode" => self.xdg_data.join("opencode"),
            "aider" => self.home.join(".aider").join(scenario),
            other => panic!("unsupported fixture agent `{other}`"),
        };

        copy_scenario_tree(&scenario_dir, &scenario_dir, &agent_home_subdir);

        let mut staged_paths = Vec::with_capacity(expected.log_files.len());
        for rel in &expected.log_files {
            let dst = agent_home_subdir.join(rel);
            assert!(
                dst.is_file(),
                "expected staged log file missing after copy: {}",
                dst.display()
            );
            staged_paths.push(dst);
        }

        (staged_paths, expected)
    }

    /// Recursively removes write permissions on the synthetic `HOME` and `XDG_DATA_HOME`
    /// trees to prove decoders open agent logs strictly read-only (`PLAN.md` §0 #5 & §9).
    pub fn make_logs_readonly(&self) -> ReadOnlyGuard {
        let targets = vec![self.home.clone(), self.xdg_data.clone()];
        for target in &targets {
            set_tree_readonly_postorder(target);
        }
        ReadOnlyGuard { targets }
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
            .env_remove("CODEX_HOME")
            .env_remove("GEMINI_CLI_HOME")
            .env_remove("OPENCODE_DATA_DIR")
            .env_remove("AIDER_HOME")
            .env_remove("AIDER_CHAT_HISTORY_FILE")
            .env_remove("AIDER_ANALYTICS_LOG")
            .env_remove("CURSOR_STATE_DB")
            .env_remove("CURSOR_GLOBAL_STORAGE_DIR")
            .env_remove("APPDATA");
        cmd
    }
}

fn materialize_cursor_vscdb(kv_json_path: &Path, vscdb_path: &Path) {
    let raw = fs::read_to_string(kv_json_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", kv_json_path.display()));
    let doc: serde_json::Value =
        serde_json::from_str(&raw).expect("valid cursor_kv.json JSON payload");
    let entries = doc
        .get("cursorDiskKV")
        .and_then(serde_json::Value::as_array)
        .expect("cursor_kv.json must contain a cursorDiskKV array");

    let conn = rusqlite::Connection::open(vscdb_path).expect("open staging state.vscdb");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
    )
    .expect("create cursorDiskKV table");

    for entry in entries {
        let key = entry
            .get("key")
            .and_then(serde_json::Value::as_str)
            .expect("cursorDiskKV entry must have string key");
        let value_obj = entry
            .get("value")
            .expect("cursorDiskKV entry must have value");
        let value_json = serde_json::to_string(value_obj).expect("serialize cursorDiskKV value");
        conn.execute(
            "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value_json],
        )
        .expect("insert cursorDiskKV row");
    }
}

fn copy_scenario_tree(root: &Path, cur: &Path, dst_root: &Path) {
    let entries = fs::read_dir(cur)
        .unwrap_or_else(|e| panic!("failed to read fixture directory {}: {e}", cur.display()));
    for entry in entries {
        let entry = entry.expect("valid fixture dir entry");
        let path = entry.path();
        if path.is_dir() {
            copy_scenario_tree(root, &path, dst_root);
        } else {
            let rel = path.strip_prefix(root).expect("strip scenario root");
            if rel == Path::new("expected.json") {
                continue;
            }
            let dst = dst_root.join(rel);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).expect("create staged fixture parent directory");
            }
            fs::copy(&path, &dst).unwrap_or_else(|e| {
                panic!(
                    "failed to stage fixture {} -> {}: {e}",
                    path.display(),
                    dst.display()
                )
            });
        }
    }
}

pub struct ReadOnlyGuard {
    targets: Vec<PathBuf>,
}

impl Drop for ReadOnlyGuard {
    fn drop(&mut self) {
        for target in &self.targets {
            restore_tree_writable_preorder(target);
        }
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
