//! Read-only per-agent log discovery (`PathCatalog`) and file opening helpers (`PLAN.md` §0 #5, §3.1, §5).

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use kakeibo_core::AgentId;
use serde::Deserialize;

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
    gemini_cli_home: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
    opencode_data_dir: Option<PathBuf>,
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

/// Collects Gemini CLI `.jsonl` and legacy `.json` chat files directly inside `dir`,
/// preferring `<stem>.jsonl` whenever both `<stem>.jsonl` and `<stem>.json` coexist
/// (migrated resumed sessions in `@google/gemini-cli 0.41.2+`).
fn collect_deduped_gemini_chat_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut by_stem: BTreeMap<String, (Option<PathBuf>, Option<PathBuf>)> = BTreeMap::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if file_name.starts_with('.') {
            continue;
        }
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        match path.extension().and_then(|e| e.to_str()) {
            Some("jsonl") => {
                by_stem.entry(stem).or_default().0 = Some(path);
            }
            Some("json") => {
                by_stem.entry(stem).or_default().1 = Some(path);
            }
            _ => {}
        }
    }

    let mut chosen = Vec::with_capacity(by_stem.len());
    for (_stem, (jsonl_opt, json_opt)) in by_stem {
        if let Some(jsonl_path) = jsonl_opt {
            chosen.push(jsonl_path);
        } else if let Some(json_path) = json_opt {
            chosen.push(json_path);
        }
    }
    chosen.sort();
    Ok(chosen)
}

#[derive(Deserialize)]
struct GeminiProjectsFile {
    #[serde(default)]
    projects: BTreeMap<String, String>,
}

/// Reads `<gemini_root>/projects.json` (`abs_path -> slug`) and per-slug `.project_root`
/// files strictly read-only, returning `slug -> raw_cwd`.
pub(crate) fn read_gemini_slug_registry_at(
    gemini_root: &Path,
) -> io::Result<BTreeMap<String, String>> {
    let mut slug_to_cwd = BTreeMap::new();

    let projects_json = gemini_root.join("projects.json");
    if projects_json.is_file() {
        if let Ok(file) = open_readonly(&projects_json) {
            let mut raw = String::new();
            if file.take(4 * 1024 * 1024).read_to_string(&mut raw).is_ok() {
                if let Ok(parsed) = serde_json::from_str::<GeminiProjectsFile>(&raw) {
                    for (abs_path, slug) in parsed.projects {
                        let abs_trimmed = abs_path.trim();
                        let slug_trimmed = slug.trim();
                        if !abs_trimmed.is_empty() && !slug_trimmed.is_empty() {
                            slug_to_cwd.insert(slug_trimmed.to_string(), abs_trimmed.to_string());
                        }
                    }
                }
            }
        }
    }

    let tmp_dir = gemini_root.join("tmp");
    if tmp_dir.is_dir() {
        for entry in fs::read_dir(&tmp_dir)? {
            let slug_dir = entry?.path();
            if !slug_dir.is_dir() {
                continue;
            }
            let Some(slug) = slug_dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if slug_to_cwd.contains_key(slug) {
                continue;
            }
            let marker = slug_dir.join(".project_root");
            if marker.is_file() {
                if let Ok(file) = open_readonly(&marker) {
                    let mut raw = String::new();
                    if file.take(16 * 1024).read_to_string(&mut raw).is_ok() {
                        let trimmed = raw.trim();
                        if !trimmed.is_empty() {
                            slug_to_cwd.insert(slug.to_string(), trimmed.to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(slug_to_cwd)
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
            gemini_cli_home: None,
            xdg_data_home: None,
            opencode_data_dir: None,
        }
    }

    /// Overrides the `GEMINI_CLI_HOME` root on this [`PathCatalog`].
    #[must_use]
    pub fn with_gemini_cli_home(mut self, gemini_cli_home: Option<PathBuf>) -> Self {
        self.gemini_cli_home = gemini_cli_home;
        self
    }

    /// Overrides `XDG_DATA_HOME` on this [`PathCatalog`].
    #[must_use]
    pub fn with_xdg_data_home(mut self, xdg_data_home: Option<PathBuf>) -> Self {
        self.xdg_data_home = xdg_data_home;
        self
    }

    /// Overrides `OPENCODE_DATA_DIR` on this [`PathCatalog`].
    #[must_use]
    pub fn with_opencode_data_dir(mut self, opencode_data_dir: Option<PathBuf>) -> Self {
        self.opencode_data_dir = opencode_data_dir;
        self
    }

    /// Creates a [`PathCatalog`] populated from standard process environment variables.
    #[must_use]
    pub fn from_env() -> Self {
        let home_dir = non_empty_env_path("HOME").or_else(|| non_empty_env_path("USERPROFILE"));
        let claude_config_dir = non_empty_env_path("CLAUDE_CONFIG_DIR");
        let codex_home = non_empty_env_path("CODEX_HOME");
        let gemini_cli_home = non_empty_env_path("GEMINI_CLI_HOME");
        let xdg_data_home =
            non_empty_env_path("XDG_DATA_HOME").or_else(|| non_empty_env_path("LOCALAPPDATA"));
        let opencode_data_dir = non_empty_env_path("OPENCODE_DATA_DIR");
        Self {
            home_dir,
            claude_config_dir,
            codex_home,
            gemini_cli_home,
            xdg_data_home,
            opencode_data_dir,
        }
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

    /// Resolves the Gemini CLI `.gemini` root directory (`GEMINI_CLI_HOME` or `~/.gemini`).
    #[must_use]
    pub fn gemini_root(&self) -> Option<PathBuf> {
        if let Some(override_dir) = &self.gemini_cli_home {
            if !override_dir.as_os_str().is_empty() {
                if override_dir.file_name().and_then(|s| s.to_str()) == Some(".gemini")
                    || override_dir.join("tmp").is_dir()
                    || override_dir.join("projects.json").is_file()
                {
                    return Some(override_dir.clone());
                }
                return Some(override_dir.join(".gemini"));
            }
        }
        self.home_dir.as_ref().map(|home| home.join(".gemini"))
    }

    /// Resolves the OpenCode data root directory (`OPENCODE_DATA_DIR`, `${XDG_DATA_HOME}/opencode`,
    /// or `~/.local/share/opencode`).
    #[must_use]
    pub fn opencode_root(&self) -> Option<PathBuf> {
        if let Some(override_dir) = &self.opencode_data_dir {
            if !override_dir.as_os_str().is_empty() {
                return Some(override_dir.clone());
            }
        }
        if let Some(xdg) = &self.xdg_data_home {
            if !xdg.as_os_str().is_empty() {
                return Some(xdg.join("opencode"));
            }
        }
        self.home_dir
            .as_ref()
            .map(|home| home.join(".local/share/opencode"))
    }

    /// Resolves `<opencode_root>/opencode.db`.
    #[must_use]
    pub fn opencode_db_path(&self) -> Option<PathBuf> {
        self.opencode_root().map(|root| root.join("opencode.db"))
    }

    /// Resolves `<opencode_root>/storage` (legacy JSON hierarchy).
    #[must_use]
    pub fn opencode_legacy_storage_dir(&self) -> Option<PathBuf> {
        self.opencode_root().map(|root| root.join("storage"))
    }

    /// Loads the Gemini CLI `slug -> raw_cwd` mapping from `<gemini_root>/projects.json`
    /// and `<gemini_root>/tmp/<slug>/.project_root` strictly read-only.
    pub fn load_gemini_slug_registry(&self) -> io::Result<BTreeMap<String, String>> {
        let Some(root) = self.gemini_root() else {
            return Ok(BTreeMap::new());
        };
        read_gemini_slug_registry_at(&root)
    }

    /// Discovers top-level Gemini CLI session transcripts (`<gemini_root>/tmp/<slug>/chats/session-*.{jsonl,json}`),
    /// deduplicating migrated `.json` files when `<same-stem>.jsonl` exists (`PLAN.md` §5, T10).
    pub fn discover_gemini_top_level(&self) -> io::Result<Vec<DiscoveredLog>> {
        let Some(root) = self.gemini_root() else {
            return Ok(Vec::new());
        };
        let tmp_dir = root.join("tmp");
        if !tmp_dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut logs = Vec::new();
        for slug_entry in fs::read_dir(&tmp_dir)? {
            let slug_dir = slug_entry?.path();
            if !slug_dir.is_dir() {
                continue;
            }
            let chats_dir = slug_dir.join("chats");
            for path in collect_deduped_gemini_chat_files(&chats_dir)? {
                logs.push(DiscoveredLog {
                    agent: AgentId::GeminiCli,
                    path,
                });
            }
        }

        logs.sort();
        Ok(logs)
    }

    /// Discovers nested Gemini CLI subagent transcripts (`<gemini_root>/tmp/<slug>/chats/<parent_session_id>/*.{jsonl,json}`),
    /// deduplicating migrated `.json` twins when `<same-stem>.jsonl` exists.
    pub fn discover_gemini_subagents(&self) -> io::Result<Vec<DiscoveredSubagentLog>> {
        let Some(root) = self.gemini_root() else {
            return Ok(Vec::new());
        };
        let tmp_dir = root.join("tmp");
        if !tmp_dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut subagents = Vec::new();
        for slug_entry in fs::read_dir(&tmp_dir)? {
            let slug_dir = slug_entry?.path();
            if !slug_dir.is_dir() {
                continue;
            }
            let chats_dir = slug_dir.join("chats");
            if !chats_dir.is_dir() {
                continue;
            }
            for chat_entry in fs::read_dir(&chats_dir)? {
                let parent_dir = chat_entry?.path();
                if !parent_dir.is_dir() {
                    continue;
                }
                let Some(parent_session_id) = parent_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|s| !s.starts_with('.'))
                    .map(str::to_string)
                else {
                    continue;
                };
                for sub_path in collect_deduped_gemini_chat_files(&parent_dir)? {
                    subagents.push(DiscoveredSubagentLog {
                        agent: AgentId::GeminiCli,
                        parent_session_id: parent_session_id.clone(),
                        path: sub_path,
                    });
                }
            }
        }

        subagents.sort();
        Ok(subagents)
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
