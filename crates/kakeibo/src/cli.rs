//! Command-line argument definitions (`PLAN.md` §3.2, §6.4, §10 T08).

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use kakeibo_core::{product, AgentId, Category};
use kakeibo_report::TopDimension;

/// Top-level CLI entrypoint.
#[derive(Parser, Debug)]
#[command(
    name = product::BINARY_NAME,
    version = product::VERSION,
    about = "Token budget diary for your coding agents",
    long_about = None,
)]
pub struct Cli {
    /// Bypass any incremental index cache and decode raw agent logs directly (`PLAN.md` §3.2).
    #[arg(long, global = true)]
    pub no_cache: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// MVP terminal subcommands (`PLAN.md` §6.4).
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// List discovered coding-agent sessions with token splits, cost status, and top flag.
    Sessions(SessionsArgs),
    /// Explain per-category and per-turn token spend and root-cause flags for one session.
    Autopsy(AutopsyArgs),
    /// List ranked root-cause flags across discovered sessions.
    Flags(FlagsArgs),
    /// Aggregate top token spenders by category, tool, project, or flag.
    Top(TopArgs),
    /// Export one session or a time window as JSON (`schemaVersion: 1`), Markdown, or NDJSON ledger.
    Export(ExportArgs),
    /// Report discovered agent log sources, observed CLI versions, and unknown-record diagnostics.
    Doctor,
}

/// Arguments for `sessions`.
#[derive(clap::Args, Debug, Default)]
pub struct SessionsArgs {
    /// Filter by source coding agent (`claude-code` or `codex`).
    #[arg(long)]
    pub agent: Option<AgentId>,

    /// Filter sessions active at or after `<N>{s|m|h|d|w}` ago or an RFC 3339 UTC timestamp.
    #[arg(long)]
    pub since: Option<String>,

    /// Filter by project working directory path or `cwd_hash` prefix.
    #[arg(long)]
    pub project: Option<String>,
}

/// Arguments for `autopsy`.
#[derive(clap::Args, Debug)]
pub struct AutopsyArgs {
    /// Session identifier, unique prefix, or `latest`.
    pub session: String,

    /// Include the chronological per-turn request breakdown table.
    #[arg(long)]
    pub turns: bool,

    /// Focus category breakdown on a single attribution category (`snake_case` or `kebab-case`).
    #[arg(long)]
    pub category: Option<Category>,
}

/// Arguments for `flags`.
#[derive(clap::Args, Debug, Default)]
pub struct FlagsArgs {
    /// Filter sessions active at or after `<N>{s|m|h|d|w}` ago or an RFC 3339 UTC timestamp.
    #[arg(long)]
    pub since: Option<String>,
}

/// Arguments for `top`.
#[derive(clap::Args, Debug)]
pub struct TopArgs {
    /// Grouping dimension (`category`, `tool`, `project`, or `flag`).
    #[arg(long)]
    pub by: TopDimension,

    /// Filter sessions active at or after `<N>{s|m|h|d|w}` ago or an RFC 3339 UTC timestamp.
    #[arg(long)]
    pub since: Option<String>,
}

/// Output format for `export`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Structured JSON report (`schemaVersion: 1`, `docs/cli-schema.md`).
    Json,
    /// GitHub-Flavored Markdown autopsy report (pathless by default).
    Md,
    /// Streaming `kakeibo.ledger/1` NDJSON turn ledger (`docs/ledger-schema.md`).
    Ndjson,
}

/// Arguments for `export`.
#[derive(clap::Args, Debug)]
pub struct ExportArgs {
    /// Session identifier, unique prefix, or `latest` (mutually exclusive with `--since`).
    pub session: Option<String>,

    /// Export all sessions active at or after `<N>{s|m|h|d|w}` ago or an RFC 3339 timestamp.
    #[arg(long)]
    pub since: Option<String>,

    /// Target export format (`json`, `md`, or `ndjson`).
    #[arg(long, value_enum)]
    pub format: ExportFormat,

    /// Write output to `<FILE>` instead of `stdout`.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Include unhashed project working directory paths in Markdown or JSON export.
    #[arg(long)]
    pub show_paths: bool,
}
