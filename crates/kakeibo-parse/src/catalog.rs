//! Read-only per-agent log discovery (`PathCatalog`) and file opening helpers (`PLAN.md` §0 #5, §3.1, §5).

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use kakeibo_core::AgentId;

/// A discovered top-level primary session log file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiscoveredLog {
    pub agent: AgentId,
    pub path: PathBuf,
}

/// A discovered child subagent session log file linked to its parent session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiscoveredSubagentLog {
    pub agent: AgentId,
    pub parent_session_id: String,
    pub path: PathBuf,
}

/// Resolves per-agent log directories without touching global environment variables
/// when constructed explicitly (e.g. inside `TestEnv`).
#[derive(Debug, Clone, Default)]
pub struct PathCatalog {
    home_dir: Option<PathBuf>,
    claude_config_dir: Option<PathBuf>,
    codex_home: Option<PathBuf>,
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|val| !val.is_empty())
        .map(PathBuf::from)
}

fn is_jsonl_file(path: &Path) -> bool {
    path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
}

fn is_codex_rollout_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with(".jsonl") || name.ends_with(".jsonl.zst")
}

fn collect_codex_rollouts_recursive(dir: &Path, out: &mut Vec<DiscoveredLog>) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_codex_rollouts_recursive(&path, out)?;
        } else if is_codex_rollout_file(&path) {
            out.push(DiscoveredLog {
                agent: AgentId::Codex,
                path,
            });
        }
    }
    Ok(())
}

impl PathCatalog {
    /// Creates an explicit [`PathCatalog`] for isolated testing or custom roots.
    #[must_use]
    pub const fn new(
        home_dir: Option<PathBuf>,
        claude_config_dir: Option<PathBuf>,
        codex_home: Option<PathBuf>,
    ) -> Self {
        Self {
            home_dir,
            claude_config_dir,
            codex_home,
        }
    }

    /// Creates a [`PathCatalog`] populated from standard process environment variables.
    #[must_use]
    pub fn from_env() -> Self {
        let home_dir = non_empty_env_path("HOME").or_else(|| non_empty_env_path("USERPROFILE"));
        let claude_config_dir = non_empty_env_path("CLAUDE_CONFIG_DIR");
        let codex_home = non_empty_env_path("CODEX_HOME");
        Self::new(home_dir, claude_config_dir, codex_home)
    }

    /// Resolves the Claude Code root directory (`CLAUDE_CONFIG_DIR` or `~/.claude`).
    #[must_use]
    pub fn claude_root(&self) -> Option<PathBuf> {
        if let Some(override_dir) = &self.claude_config_dir {
            if !override_dir.as_os_str().is_empty() {
                return Some(override_dir.clone());
            }
        }
        self.home_dir.as_ref().map(|home| home.join(".claude"))
    }

    /// Resolves the Codex CLI root directory (`CODEX_HOME` or `~/.codex`).
    #[must_use]
    pub fn codex_root(&self) -> Option<PathBuf> {
        if let Some(override_dir) = &self.codex_home {
            if !override_dir.as_os_str().is_empty() {
                return Some(override_dir.clone());
            }
        }
        self.home_dir.as_ref().map(|home| home.join(".codex"))
    }

    /// Discovers top-level Claude Code session logs (`<claude_root>/projects/<cwd-slug>/<session>.jsonl`),
    /// excluding nested `<session>/subagents/*.jsonl` files (`PLAN.md` §5).
    pub fn discover_claude_top_level(&self) -> io::Result<Vec<DiscoveredLog>> {
        let Some(root) = self.claude_root() else {
            return Ok(Vec::new());
        };
        let projects_dir = root.join("projects");
        if !projects_dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut logs = Vec::new();
        for slug_entry in fs::read_dir(&projects_dir)? {
            let slug_path = slug_entry?.path();
            if !slug_path.is_dir() {
                continue;
            }
            for session_entry in fs::read_dir(&slug_path)? {
                let candidate = session_entry?.path();
                if is_jsonl_file(&candidate) {
                    logs.push(DiscoveredLog {
                        agent: AgentId::ClaudeCode,
                        path: candidate,
                    });
                }
            }
        }

        logs.sort();
        Ok(logs)
    }

    /// Discovers nested Claude Code subagent logs (`<claude_root>/projects/<cwd-slug>/<session>/subagents/*.jsonl`).
    pub fn discover_claude_subagents(&self) -> io::Result<Vec<DiscoveredSubagentLog>> {
        let Some(root) = self.claude_root() else {
            return Ok(Vec::new());
        };
        let projects_dir = root.join("projects");
        if !projects_dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut subagents = Vec::new();
        for slug_entry in fs::read_dir(&projects_dir)? {
            let slug_path = slug_entry?.path();
            if !slug_path.is_dir() {
                continue;
            }
            for session_dir_entry in fs::read_dir(&slug_path)? {
                let session_dir = session_dir_entry?.path();
                if !session_dir.is_dir() {
                    continue;
                }
                let Some(parent_session_id) = session_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                let subagents_dir = session_dir.join("subagents");
                if !subagents_dir.is_dir() {
                    continue;
                }
                for sub_entry in fs::read_dir(&subagents_dir)? {
                    let sub_path = sub_entry?.path();
                    if is_jsonl_file(&sub_path) {
                        subagents.push(DiscoveredSubagentLog {
                            agent: AgentId::ClaudeCode,
                            parent_session_id: parent_session_id.clone(),
                            path: sub_path,
                        });
                    }
                }
            }
        }

        subagents.sort();
        Ok(subagents)
    }

    /// Discovers Codex CLI rollout logs under `<codex_root>/sessions/**` and
    /// `<codex_root>/archived_sessions/**` (`*.jsonl` and `*.jsonl.zst`, `PLAN.md` §5).
    pub fn discover_codex_rollouts(&self) -> io::Result<Vec<DiscoveredLog>> {
        let Some(root) = self.codex_root() else {
            return Ok(Vec::new());
        };
        let mut logs = Vec::new();
        collect_codex_rollouts_recursive(&root.join("sessions"), &mut logs)?;
        collect_codex_rollouts_recursive(&root.join("archived_sessions"), &mut logs)?;
        logs.sort();
        Ok(logs)
    }
}

/// Opens an agent log file strictly read-only (`PLAN.md` §0 #5).
pub fn open_readonly(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(false)
        .create(false)
        .open(path)
}
