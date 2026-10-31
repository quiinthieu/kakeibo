//! Shared report view-models for CLI tables, Markdown reports, and JSON exports (`PLAN.md` §6.4).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use kakeibo_core::attrib::{SessionAttribution, TokenSplit};
use kakeibo_core::flags::FlagFinding;
use kakeibo_core::{AgentId, BlockKind, Confidence, Timestamp, TurnRecord};
use serde::{Deserialize, Serialize};

/// Cost availability status before/after user pricing tables are applied (`PLAN.md` §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostStatus {
    /// No configured user price matched; never rendered as `$0`.
    Unpriced,
    /// Cost figure was logged directly by the coding agent (`agent_reported_cost_usd`).
    AgentReported,
}

impl CostStatus {
    /// Canonical `snake_case` wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unpriced => "unpriced",
            Self::AgentReported => "agent_reported",
        }
    }
}

/// Session-level API-equivalent or agent-reported cost summary (`PLAN.md` §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CostSummary {
    pub status: CostStatus,
    pub usd: Option<f64>,
}

impl CostSummary {
    /// Default unpriced summary (`"unpriced"`, `usd: None`).
    pub const UNPRICED: Self = Self {
        status: CostStatus::Unpriced,
        usd: None,
    };

    /// Human-readable display label (`"unpriced"` or `"$X.XXXX (agent-reported)"`).
    #[must_use]
    pub fn display_label(&self) -> String {
        match (self.status, self.usd) {
            (CostStatus::Unpriced, _) | (_, None) => "unpriced".to_string(),
            (CostStatus::AgentReported, Some(amount)) => {
                format!("${amount:.4} (agent-reported)")
            }
        }
    }
}

/// Aggregated token carry and invocation counts for one tool within a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpendSummary {
    pub tool: String,
    pub calls: u32,
    pub results: u32,
    pub errors: u32,
    pub bytes: u64,
    pub initial_tokens: u64,
    pub carried_tokens: u64,
    pub lifetime_tokens: u64,
    pub confidence: Confidence,
    pub estimation_method: &'static str,
}

/// Complete attributed and flagged view of a single primary coding-agent session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionReport {
    pub agent: AgentId,
    pub session_id: String,
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
    pub models: Vec<String>,
    pub project_hash: Option<String>,
    /// Populated exclusively when `export --show-paths` is explicitly requested.
    pub project_path: Option<String>,
    pub cost: CostSummary,
    pub turns: Vec<TurnRecord>,
    pub attribution: SessionAttribution,
    pub flags: Vec<FlagFinding>,
    pub tools: Vec<ToolSpendSummary>,
}

#[derive(Default)]
struct ToolAccumulator {
    calls: u32,
    results: u32,
    errors: u32,
    bytes: u64,
    initial_tokens: u64,
    carried_tokens: u64,
}

impl SessionReport {
    /// Builds a [`SessionReport`] from a non-empty chronological slice of [`TurnRecord`]s,
    /// its [`SessionAttribution`], and its ranked [`FlagFinding`]s.
    #[must_use]
    pub fn build(
        turns: Vec<TurnRecord>,
        attribution: SessionAttribution,
        flags: Vec<FlagFinding>,
        project_path: Option<String>,
    ) -> Option<Self> {
        let first = turns.first()?;
        let agent = first.agent;
        let session_id = first.session_id.clone();

        let mut started_at = first.ts;
        let mut ended_at = first.ts;
        let mut model_set = BTreeSet::new();
        let mut project_hash: Option<String> = None;
        let mut reported_cost_sum = 0.0_f64;
        let mut saw_reported_cost = false;

        for tr in &turns {
            if tr.ts < started_at {
                started_at = tr.ts;
            }
            if tr.ts > ended_at {
                ended_at = tr.ts;
            }
            if let Some(model) = tr.model.as_deref().filter(|m| !m.is_empty()) {
                model_set.insert(model.to_string());
            }
            if tr.cwd_hash.is_some() {
                project_hash.clone_from(&tr.cwd_hash);
            }
            if let Some(cost_usd) = tr
                .usage
                .agent_reported_cost_usd
                .filter(|v| v.is_finite() && *v >= 0.0)
            {
                reported_cost_sum += cost_usd;
                saw_reported_cost = true;
            }
        }

        let cost = if saw_reported_cost {
            CostSummary {
                status: CostStatus::AgentReported,
                usd: Some(reported_cost_sum),
            }
        } else {
            CostSummary::UNPRICED
        };

        let mut tool_map: BTreeMap<String, ToolAccumulator> = BTreeMap::new();
        for blk in &attribution.carried_blocks {
            if !matches!(blk.kind, BlockKind::ToolUse | BlockKind::ToolResult) {
                continue;
            }
            let name = blk.tool.clone().unwrap_or_else(|| "unknown".to_string());
            let entry = tool_map.entry(name).or_default();
            if blk.kind == BlockKind::ToolUse {
                entry.calls = entry.calls.saturating_add(1);
            } else {
                entry.results = entry.results.saturating_add(1);
            }
            if blk.is_error {
                entry.errors = entry.errors.saturating_add(1);
            }
            entry.bytes = entry.bytes.saturating_add(blk.bytes);
            entry.initial_tokens = entry
                .initial_tokens
                .saturating_add(blk.initial_split.prompt_tokens());
            entry.carried_tokens = entry
                .carried_tokens
                .saturating_add(blk.carried_prompt_tokens());
        }

        let mut tools: Vec<ToolSpendSummary> = tool_map
            .into_iter()
            .map(|(tool, acc)| {
                let lifetime_tokens = acc.initial_tokens.saturating_add(acc.carried_tokens);
                ToolSpendSummary {
                    tool,
                    calls: acc.calls,
                    results: acc.results,
                    errors: acc.errors,
                    bytes: acc.bytes,
                    initial_tokens: acc.initial_tokens,
                    carried_tokens: acc.carried_tokens,
                    lifetime_tokens,
                    confidence: Confidence::Estimated,
                    estimation_method: attribution.estimation_method,
                }
            })
            .collect();
        tools.sort_by(|a, b| {
            b.lifetime_tokens
                .cmp(&a.lifetime_tokens)
                .then_with(|| a.tool.cmp(&b.tool))
        });

        Some(Self {
            agent,
            session_id,
            started_at,
            ended_at,
            models: model_set.into_iter().collect(),
            project_hash,
            project_path,
            cost,
            turns,
            attribution,
            flags,
            tools,
        })
    }

    /// Number of deduplicated turns in this session.
    #[must_use]
    pub fn turn_count(&self) -> u32 {
        u32::try_from(self.turns.len()).unwrap_or(u32::MAX)
    }

    /// Short display session identifier (`8` characters for UUID-like IDs).
    #[must_use]
    pub fn short_id(&self) -> &str {
        if self.session_id.len() > 12 {
            let end = self
                .session_id
                .char_indices()
                .nth(8)
                .map_or(self.session_id.len(), |(idx, _)| idx);
            &self.session_id[..end]
        } else {
            &self.session_id
        }
    }

    /// Renders the privacy-safe project label (`proj:<hash>` by default, or raw path when `show_paths`).
    #[must_use]
    pub fn project_label(&self, show_paths: bool) -> String {
        if let Some(raw) = self.project_path.as_deref().filter(|_| show_paths) {
            return raw.to_string();
        }
        match &self.project_hash {
            Some(hash) => format!("proj:{hash}"),
            None => "unknown".to_string(),
        }
    }

    /// Session-wide measured + subagent token totals.
    #[must_use]
    pub const fn totals(&self) -> TokenSplit {
        self.attribution.totals
    }
}

/// Sorts session reports deterministically newest-first (`ended_at DESC, session_id ASC`).
pub fn sort_session_reports(sessions: &mut [SessionReport]) {
    sessions.sort_by(|a, b| {
        b.ended_at
            .cmp(&a.ended_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
}

/// Computes integer basis points (`0..=10_000`) of `part` relative to `whole`.
#[must_use]
pub fn share_basis_points(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        return 0;
    }
    u64::try_from((u128::from(part) * 10_000) / u128::from(whole)).unwrap_or(10_000)
}

/// Formats integer basis points (`0..=10_000`) as a fixed-precision percentage (`"12.34%"`).
#[must_use]
pub fn format_share_pct(bps: u64) -> String {
    format!("{}.{:02}%", bps / 100, bps % 100)
}

/// Grouping dimension for `kakeibo top --by <dimension>` (`PLAN.md` §6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopDimension {
    Category,
    Tool,
    Project,
    Flag,
}

impl TopDimension {
    /// Canonical lowercase CLI slug.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Category => "category",
            Self::Tool => "tool",
            Self::Project => "project",
            Self::Flag => "flag",
        }
    }
}

impl fmt::Display for TopDimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown [`TopDimension`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown `--by` dimension `{0}` (expected category, tool, project, or flag)")]
pub struct UnknownTopDimensionError(pub String);

impl FromStr for TopDimension {
    type Err = UnknownTopDimensionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "category" => Ok(Self::Category),
            "tool" => Ok(Self::Tool),
            "project" => Ok(Self::Project),
            "flag" => Ok(Self::Flag),
            other => Err(UnknownTopDimensionError(other.to_string())),
        }
    }
}

/// Per-agent discovery and diagnostic summary rendered by `kakeibo doctor` (`PLAN.md` §6.4).
///
/// Pathless by design so `doctor` output can be pasted into bug reports safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorSourceSummary {
    pub agent: AgentId,
    pub root_exists: bool,
    pub log_files: usize,
    pub subagent_files: usize,
    pub sessions: usize,
    pub turns: usize,
    pub cli_versions: Vec<String>,
    pub ignored_records: u64,
    pub unknown_record_types: BTreeMap<String, u64>,
    pub parse_issues: usize,
}

/// Aggregated cross-agent diagnostic report for `kakeibo doctor`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorReport {
    pub sources: Vec<DoctorSourceSummary>,
}
