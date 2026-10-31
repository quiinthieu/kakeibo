//! Pure deterministic root-cause flag configuration and v1 rule engine (`PLAN.md` §0 #7, §6.3, §10 T07).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::attrib::{SessionAttribution, TokenSplit, TurnAttribution};
use crate::model::{BlockKind, Confidence, Timestamp, TurnRecord};

const BUNDLED_FLAGS_TOML: &str = include_str!("../../../data/flags.toml");
const BUNDLED_MODELS_TOML: &str = include_str!("../../../data/models.toml");
const EXPECTED_SCHEMA_VERSION: u32 = 1;

/// Errors returned when parsing or validating `flags.toml` or `models.toml`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlagConfigError {
    /// Underlying TOML syntax or deserialisation failure.
    #[error("invalid TOML configuration: {0}")]
    Toml(String),
    /// Configuration declared an unsupported `schema_version`.
    #[error("unsupported config schema_version {found} (expected {expected})")]
    UnsupportedSchemaVersion { found: u32, expected: u32 },
    /// Required provenance metadata (`source` or `checked`) was blank.
    #[error("entry `{entry}` is missing non-empty `{field}` provenance metadata")]
    MissingProvenance { entry: String, field: &'static str },
    /// A numeric threshold or context window value was out of valid bounds.
    #[error("invalid `{section}.{field}`: {reason}")]
    InvalidThreshold {
        section: &'static str,
        field: &'static str,
        reason: &'static str,
    },
}

/// Error returned when parsing an unknown [`FlagId`] slug.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown flag identifier `{0}`")]
pub struct UnknownFlagError(pub String);

/// Deterministic root-cause flag identifiers (`PLAN.md` §6.3, §10 T07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlagId {
    ColdResume,
    NearFullContext,
    ToolOutputBloat,
    CacheMissStorm,
    HeavyOverhead,
}

impl FlagId {
    /// All five v1 flags introduced in Task T07 in canonical order.
    pub const ALL_V1: [Self; 5] = [
        Self::ColdResume,
        Self::NearFullContext,
        Self::ToolOutputBloat,
        Self::CacheMissStorm,
        Self::HeavyOverhead,
    ];

    /// Canonical `kebab-case` wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ColdResume => "cold-resume",
            Self::NearFullContext => "near-full-context",
            Self::ToolOutputBloat => "tool-output-bloat",
            Self::CacheMissStorm => "cache-miss-storm",
            Self::HeavyOverhead => "heavy-overhead",
        }
    }
}

impl fmt::Display for FlagId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FlagId {
    type Err = UnknownFlagError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cold-resume" => Ok(Self::ColdResume),
            "near-full-context" => Ok(Self::NearFullContext),
            "tool-output-bloat" => Ok(Self::ToolOutputBloat),
            "cache-miss-storm" => Ok(Self::CacheMissStorm),
            "heavy-overhead" => Ok(Self::HeavyOverhead),
            other => Err(UnknownFlagError(other.to_string())),
        }
    }
}

/// Inclusive 1-based turn span (`start..=end`) identified by a [`FlagFinding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TurnRange {
    pub start: u32,
    pub end: u32,
}

impl TurnRange {
    /// Creates an inclusive turn range (`start <= end`).
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        if start <= end {
            Self { start, end }
        } else {
            Self {
                start: end,
                end: start,
            }
        }
    }

    /// Creates a single-turn range (`turn..=turn`).
    #[must_use]
    pub const fn single(turn: u32) -> Self {
        Self {
            start: turn,
            end: turn,
        }
    }

    /// Returns `true` when `start == end`.
    #[must_use]
    pub const fn is_single(self) -> bool {
        self.start == self.end
    }
}

impl fmt::Display for TurnRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.start == self.end {
            write!(f, "{}", self.start)
        } else {
            write!(f, "{}..={}", self.start, self.end)
        }
    }
}

/// Verified model entry loaded from `data/models.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub context_window_tokens: u64,
    pub source: String,
    pub checked: String,
}

/// Provenance source for a resolved model context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextWindowSource {
    /// Logged directly by the coding agent on the turn (`TurnHints.context_window_override`).
    AgentLogOverride,
    /// Matched against verified entries in `data/models.toml`.
    Catalog,
}

impl ContextWindowSource {
    /// Canonical `snake_case` wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentLogOverride => "agent_log_override",
            Self::Catalog => "catalog",
        }
    }
}

/// Resolved context window size and provenance for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedContextWindow {
    pub tokens: u64,
    pub source: ContextWindowSource,
}

/// Verified catalog of model context windows (`data/models.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalog {
    pub schema_version: u32,
    pub models: BTreeMap<String, ModelSpec>,
}

impl ModelCatalog {
    /// Parses and validates the bundled `data/models.toml` catalog.
    #[must_use]
    pub fn bundled() -> Self {
        Self::from_toml_str(BUNDLED_MODELS_TOML)
            .expect("bundled data/models.toml must be valid TOML with provenance")
    }

    /// Parses and validates a `models.toml` document.
    pub fn from_toml_str(raw: &str) -> Result<Self, FlagConfigError> {
        let catalog: Self =
            toml::from_str(raw).map_err(|err| FlagConfigError::Toml(err.to_string()))?;
        if catalog.schema_version != EXPECTED_SCHEMA_VERSION {
            return Err(FlagConfigError::UnsupportedSchemaVersion {
                found: catalog.schema_version,
                expected: EXPECTED_SCHEMA_VERSION,
            });
        }
        for (name, spec) in &catalog.models {
            if spec.context_window_tokens == 0 {
                return Err(FlagConfigError::InvalidThreshold {
                    section: "models",
                    field: "context_window_tokens",
                    reason: "must be > 0",
                });
            }
            if spec.source.trim().is_empty() {
                return Err(FlagConfigError::MissingProvenance {
                    entry: name.clone(),
                    field: "source",
                });
            }
            if spec.checked.trim().is_empty() {
                return Err(FlagConfigError::MissingProvenance {
                    entry: name.clone(),
                    field: "checked",
                });
            }
        }
        Ok(catalog)
    }

    /// Resolves a turn's context window using `override_window` first, then exact model match,
    /// then longest separator-bounded prefix match (e.g. `claude-sonnet-4-5-20250929`).
    /// Returns `None` rather than guessing when the model is unknown (`PLAN.md` §0 #7).
    #[must_use]
    pub fn resolve_context_window(
        &self,
        model: Option<&str>,
        override_window: Option<u64>,
    ) -> Option<ResolvedContextWindow> {
        if let Some(tokens) = override_window.filter(|&w| w > 0) {
            return Some(ResolvedContextWindow {
                tokens,
                source: ContextWindowSource::AgentLogOverride,
            });
        }
        let model_name = model?.trim();
        if model_name.is_empty() {
            return None;
        }
        if let Some(spec) = self.models.get(model_name) {
            return Some(ResolvedContextWindow {
                tokens: spec.context_window_tokens,
                source: ContextWindowSource::Catalog,
            });
        }

        let mut best: Option<(&str, &ModelSpec)> = None;
        for (key, spec) in &self.models {
            if model_name
                .strip_prefix(key.as_str())
                .is_some_and(|rest| rest.starts_with(['-', '@', ':']))
            {
                match best {
                    Some((prev_key, _)) if prev_key.len() >= key.len() => {}
                    _ => best = Some((key.as_str(), spec)),
                }
            }
        }

        best.map(|(_, spec)| ResolvedContextWindow {
            tokens: spec.context_window_tokens,
            source: ContextWindowSource::Catalog,
        })
    }
}

/// Thresholds for `cold-resume` (`data/flags.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColdResumeRuleConfig {
    pub cache_ttl_seconds: u64,
    pub min_cache_write_pct: u64,
    pub min_prompt_tokens: u64,
    pub reference_url: String,
    pub source: String,
    pub checked: String,
}

/// Thresholds for `near-full-context` (`data/flags.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NearFullContextRuleConfig {
    pub min_window_pct: u64,
    pub min_requests: u32,
    pub source: String,
    pub checked: String,
}

/// Thresholds for `tool-output-bloat` (`data/flags.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputBloatRuleConfig {
    pub min_tool_tokens: u64,
    pub min_carried_turns: u32,
    pub source: String,
    pub checked: String,
}

/// Thresholds for `cache-miss-storm` (`data/flags.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheMissStormRuleConfig {
    pub min_requests: u32,
    pub min_prompt_tokens: u64,
    pub min_write_tokens: u64,
    pub write_to_read_ratio: u64,
    pub max_gap_seconds: u64,
    pub source: String,
    pub checked: String,
}

/// Thresholds for `heavy-overhead` (`data/flags.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeavyOverheadRuleConfig {
    pub max_first_prompt_tokens: u64,
    pub median_multiplier_pct: u64,
    pub min_median_floor_tokens: u64,
    pub remediation_hint: String,
    pub source: String,
    pub checked: String,
}

/// Tunable threshold table for v1 root-cause flags (`data/flags.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagsConfig {
    pub schema_version: u32,
    pub cold_resume: ColdResumeRuleConfig,
    pub near_full_context: NearFullContextRuleConfig,
    pub tool_output_bloat: ToolOutputBloatRuleConfig,
    pub cache_miss_storm: CacheMissStormRuleConfig,
    pub heavy_overhead: HeavyOverheadRuleConfig,
}

fn check_provenance(entry: &str, source: &str, checked: &str) -> Result<(), FlagConfigError> {
    if source.trim().is_empty() {
        return Err(FlagConfigError::MissingProvenance {
            entry: entry.to_string(),
            field: "source",
        });
    }
    if checked.trim().is_empty() {
        return Err(FlagConfigError::MissingProvenance {
            entry: entry.to_string(),
            field: "checked",
        });
    }
    Ok(())
}

impl FlagsConfig {
    /// Parses and validates the bundled `data/flags.toml` configuration.
    #[must_use]
    pub fn bundled() -> Self {
        Self::from_toml_str(BUNDLED_FLAGS_TOML)
            .expect("bundled data/flags.toml must be valid TOML with provenance")
    }

    /// Parses and validates a `flags.toml` document.
    pub fn from_toml_str(raw: &str) -> Result<Self, FlagConfigError> {
        let cfg: Self =
            toml::from_str(raw).map_err(|err| FlagConfigError::Toml(err.to_string()))?;
        if cfg.schema_version != EXPECTED_SCHEMA_VERSION {
            return Err(FlagConfigError::UnsupportedSchemaVersion {
                found: cfg.schema_version,
                expected: EXPECTED_SCHEMA_VERSION,
            });
        }

        check_provenance(
            "cold_resume",
            &cfg.cold_resume.source,
            &cfg.cold_resume.checked,
        )?;
        check_provenance(
            "near_full_context",
            &cfg.near_full_context.source,
            &cfg.near_full_context.checked,
        )?;
        check_provenance(
            "tool_output_bloat",
            &cfg.tool_output_bloat.source,
            &cfg.tool_output_bloat.checked,
        )?;
        check_provenance(
            "cache_miss_storm",
            &cfg.cache_miss_storm.source,
            &cfg.cache_miss_storm.checked,
        )?;
        check_provenance(
            "heavy_overhead",
            &cfg.heavy_overhead.source,
            &cfg.heavy_overhead.checked,
        )?;

        if cfg.cold_resume.min_cache_write_pct == 0 || cfg.cold_resume.min_cache_write_pct > 100 {
            return Err(FlagConfigError::InvalidThreshold {
                section: "cold_resume",
                field: "min_cache_write_pct",
                reason: "must be in 1..=100",
            });
        }
        if cfg.cold_resume.min_prompt_tokens == 0 {
            return Err(FlagConfigError::InvalidThreshold {
                section: "cold_resume",
                field: "min_prompt_tokens",
                reason: "must be > 0",
            });
        }
        if cfg.near_full_context.min_window_pct == 0 || cfg.near_full_context.min_window_pct > 100 {
            return Err(FlagConfigError::InvalidThreshold {
                section: "near_full_context",
                field: "min_window_pct",
                reason: "must be in 1..=100",
            });
        }
        if cfg.near_full_context.min_requests == 0 {
            return Err(FlagConfigError::InvalidThreshold {
                section: "near_full_context",
                field: "min_requests",
                reason: "must be > 0",
            });
        }
        if cfg.tool_output_bloat.min_tool_tokens == 0 {
            return Err(FlagConfigError::InvalidThreshold {
                section: "tool_output_bloat",
                field: "min_tool_tokens",
                reason: "must be > 0",
            });
        }
        if cfg.tool_output_bloat.min_carried_turns == 0 {
            return Err(FlagConfigError::InvalidThreshold {
                section: "tool_output_bloat",
                field: "min_carried_turns",
                reason: "must be > 0",
            });
        }
        if cfg.cache_miss_storm.min_requests == 0 {
            return Err(FlagConfigError::InvalidThreshold {
                section: "cache_miss_storm",
                field: "min_requests",
                reason: "must be > 0",
            });
        }
        if cfg.cache_miss_storm.write_to_read_ratio == 0 {
            return Err(FlagConfigError::InvalidThreshold {
                section: "cache_miss_storm",
                field: "write_to_read_ratio",
                reason: "must be > 0",
            });
        }
        if cfg.heavy_overhead.max_first_prompt_tokens == 0 {
            return Err(FlagConfigError::InvalidThreshold {
                section: "heavy_overhead",
                field: "max_first_prompt_tokens",
                reason: "must be > 0",
            });
        }
        if cfg.heavy_overhead.median_multiplier_pct < 100 {
            return Err(FlagConfigError::InvalidThreshold {
                section: "heavy_overhead",
                field: "median_multiplier_pct",
                reason: "must be >= 100",
            });
        }

        Ok(cfg)
    }
}

/// Boundary condition that triggered a `cold-resume` finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColdResumeTrigger {
    /// Explicit resume or fork marker without an observable prior turn timestamp.
    ResumeOrFork,
    /// Idle gap between consecutive main-chain turns exceeded `cache_ttl_seconds`.
    IdleGap,
    /// Both an explicit resume/fork marker and an idle gap exceeding `cache_ttl_seconds`.
    ResumeAfterIdleGap,
}

impl ColdResumeTrigger {
    /// Canonical `snake_case` wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResumeOrFork => "resume_or_fork",
            Self::IdleGap => "idle_gap",
            Self::ResumeAfterIdleGap => "resume_after_idle_gap",
        }
    }
}

/// Client-side structural trigger inferred for a `cache-miss-storm` (never claims a provider-side cause).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMissTrigger {
    /// `TurnRecord.model` changed across turns in the storm range.
    ModelSwitch,
    /// Unattributed prompt injection or `SystemContext` block shifted the prompt prefix.
    SystemOrMcpChange,
    /// Repeated mid-session prefix invalidation (edited rules, tools, or MCP config).
    PrefixInvalidation,
}

impl CacheMissTrigger {
    /// Human-readable neutral description (never claims a provider-side cause).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ModelSwitch => "mid-session model switch",
            Self::SystemOrMcpChange => "unlogged prompt or MCP schema change",
            Self::PrefixInvalidation => {
                "repeated prompt prefix change (edited rules, tools, or MCP config)"
            }
        }
    }
}

/// Typed evidence payload for each v1 [`FlagFinding`] (`PLAN.md` §6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FlagEvidence {
    ColdResume {
        trigger: ColdResumeTrigger,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap_seconds: Option<u64>,
        prompt_tokens: u64,
        rewritten_tokens: u64,
        cache_write_pct: u64,
        reference_url: String,
    },
    NearFullContext {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        request_count: u32,
        context_window_tokens: u64,
        window_source: ContextWindowSource,
        peak_prompt_tokens: u64,
        peak_window_pct: u64,
        tokens_resent: u64,
        cache_read_tokens: u64,
    },
    ToolOutputBloat {
        tool: String,
        source_turn: u32,
        bytes: u64,
        size_tokens: u64,
        carried_turns: u32,
        carried_tokens: u64,
        carried_split: TokenSplit,
    },
    CacheMissStorm {
        miss_requests: u32,
        rewritten_tokens: u64,
        cache_read_tokens: u64,
        likely_trigger: CacheMissTrigger,
    },
    HeavyOverhead {
        first_prompt_tokens: u64,
        fixed_overhead_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_median_p1: Option<u64>,
        exceeded_threshold: bool,
        exceeded_user_median: bool,
        remediation_hint: String,
    },
}

/// One deterministic root-cause flag finding ranked by `impact_tokens` (`PLAN.md` §6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagFinding {
    pub id: FlagId,
    pub turn_range: TurnRange,
    pub impact_tokens: u64,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimation_method: Option<&'static str>,
    pub evidence: FlagEvidence,
}

/// Evaluates all five v1 root-cause flags using bundled `data/flags.toml` and `data/models.toml`.
#[must_use]
pub fn evaluate_flags_v1_default(
    turns: &[TurnRecord],
    attrib: &SessionAttribution,
) -> Vec<FlagFinding> {
    let flags_cfg = FlagsConfig::bundled();
    let models = ModelCatalog::bundled();
    evaluate_flags_v1(turns, attrib, &flags_cfg, &models, None)
}

const fn merge_confidence(a: Confidence, b: Confidence) -> Confidence {
    match (a, b) {
        (Confidence::NotRecorded, _) | (_, Confidence::NotRecorded) => Confidence::NotRecorded,
        (Confidence::Estimated, _) | (_, Confidence::Estimated) => Confidence::Estimated,
        (Confidence::Measured, Confidence::Measured) => Confidence::Measured,
    }
}

struct ActiveNearFullSpan {
    start_turn: u32,
    end_turn: u32,
    model: Option<String>,
    request_count: u32,
    context_window_tokens: u64,
    window_source: ContextWindowSource,
    peak_prompt_tokens: u64,
    peak_window_pct: u64,
    tokens_resent: u64,
    cache_read_tokens: u64,
    confidence: Confidence,
}

fn flush_near_full_span(
    span: Option<ActiveNearFullSpan>,
    min_requests: u32,
    findings: &mut Vec<FlagFinding>,
) {
    if let Some(s) = span.filter(|s| s.request_count >= min_requests) {
        findings.push(FlagFinding {
            id: FlagId::NearFullContext,
            turn_range: TurnRange::new(s.start_turn, s.end_turn),
            impact_tokens: s.tokens_resent,
            confidence: s.confidence,
            estimation_method: None,
            evidence: FlagEvidence::NearFullContext {
                model: s.model,
                request_count: s.request_count,
                context_window_tokens: s.context_window_tokens,
                window_source: s.window_source,
                peak_prompt_tokens: s.peak_prompt_tokens,
                peak_window_pct: s.peak_window_pct,
                tokens_resent: s.tokens_resent,
                cache_read_tokens: s.cache_read_tokens,
            },
        });
    }
}

/// Evaluates all five v1 root-cause flags against `turns` and `attrib` (`PLAN.md` §6.3).
#[must_use]
pub fn evaluate_flags_v1(
    turns: &[TurnRecord],
    attrib: &SessionAttribution,
    flags_cfg: &FlagsConfig,
    models: &ModelCatalog,
    user_median_p1: Option<u64>,
) -> Vec<FlagFinding> {
    let main_turns: Vec<(&TurnRecord, &TurnAttribution)> = turns
        .iter()
        .zip(attrib.turns.iter())
        .filter(|(tr, _)| !tr.hints.sidechain)
        .collect();
    if main_turns.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();

    // 1. `heavy-overhead`: P(1) above threshold or user median.
    let (first_tr, first_ta) = main_turns[0];
    let p1 = first_ta.measured.prompt_tokens();
    let exceeded_threshold = p1 >= flags_cfg.heavy_overhead.max_first_prompt_tokens;
    let exceeded_user_median = match user_median_p1 {
        Some(med) if med > 0 && p1 >= flags_cfg.heavy_overhead.min_median_floor_tokens => {
            u128::from(p1) * 100
                >= u128::from(med) * u128::from(flags_cfg.heavy_overhead.median_multiplier_pct)
        }
        _ => false,
    };
    if exceeded_threshold || exceeded_user_median {
        findings.push(FlagFinding {
            id: FlagId::HeavyOverhead,
            turn_range: TurnRange::single(first_tr.turn),
            impact_tokens: p1,
            confidence: first_ta.measured_confidence,
            estimation_method: None,
            evidence: FlagEvidence::HeavyOverhead {
                first_prompt_tokens: p1,
                fixed_overhead_tokens: attrib.fixed_overhead_tokens,
                user_median_p1,
                exceeded_threshold,
                exceeded_user_median,
                remediation_hint: flags_cfg.heavy_overhead.remediation_hint.clone(),
            },
        });
    }

    // 2. `cold-resume`: first request after resume/fork or idle gap > TTL with high cache_write.
    let mut prev_ts: Option<Timestamp> = None;
    for (idx, &(tr, ta)) in main_turns.iter().enumerate() {
        let is_explicit_resume =
            tr.hints.resume_start || (idx == 0 && tr.parent_session_id.is_some());
        let gap_seconds = prev_ts.map(|prev| {
            u64::try_from(tr.ts.duration_seconds_since(prev).max(0)).unwrap_or(u64::MAX)
        });
        let is_idle_gap =
            gap_seconds.is_some_and(|gap| gap > flags_cfg.cold_resume.cache_ttl_seconds);
        prev_ts = Some(tr.ts);

        let trigger = match (is_explicit_resume, is_idle_gap) {
            (true, true) => Some(ColdResumeTrigger::ResumeAfterIdleGap),
            (false, true) => Some(ColdResumeTrigger::IdleGap),
            (true, false) => Some(ColdResumeTrigger::ResumeOrFork),
            (false, false) => None,
        };

        if let Some(trigger) = trigger {
            let p = ta.measured.prompt_tokens();
            let cw = ta.measured.cache_write;
            if p >= flags_cfg.cold_resume.min_prompt_tokens
                && u128::from(cw) * 100
                    >= u128::from(p) * u128::from(flags_cfg.cold_resume.min_cache_write_pct)
            {
                let cache_write_pct =
                    u64::try_from((u128::from(cw) * 100) / u128::from(p)).unwrap_or(100);
                findings.push(FlagFinding {
                    id: FlagId::ColdResume,
                    turn_range: TurnRange::single(tr.turn),
                    impact_tokens: cw,
                    confidence: ta.measured_confidence,
                    estimation_method: None,
                    evidence: FlagEvidence::ColdResume {
                        trigger,
                        gap_seconds,
                        prompt_tokens: p,
                        rewritten_tokens: cw,
                        cache_write_pct,
                        reference_url: flags_cfg.cold_resume.reference_url.clone(),
                    },
                });
            }
        }
    }

    // 3. `near-full-context`: P >= Y% of context window for >= K contiguous main-chain requests.
    let mut active_span: Option<ActiveNearFullSpan> = None;
    for &(tr, ta) in &main_turns {
        let p = ta.measured.prompt_tokens();
        let resolved =
            models.resolve_context_window(tr.model.as_deref(), tr.hints.context_window_override);
        let qualifies = resolved.filter(|win| {
            u128::from(p) * 100
                >= u128::from(win.tokens) * u128::from(flags_cfg.near_full_context.min_window_pct)
        });

        if let Some(win) = qualifies {
            let pct = u64::try_from((u128::from(p) * 100) / u128::from(win.tokens)).unwrap_or(100);
            match &mut active_span {
                Some(span) => {
                    span.end_turn = tr.turn;
                    span.request_count = span.request_count.saturating_add(1);
                    span.tokens_resent = span.tokens_resent.saturating_add(p);
                    span.cache_read_tokens = span
                        .cache_read_tokens
                        .saturating_add(ta.measured.cache_read);
                    span.confidence = merge_confidence(span.confidence, ta.measured_confidence);
                    if p > span.peak_prompt_tokens {
                        span.peak_prompt_tokens = p;
                    }
                    if pct > span.peak_window_pct {
                        span.peak_window_pct = pct;
                        span.context_window_tokens = win.tokens;
                        span.window_source = win.source;
                        if tr.model.is_some() {
                            span.model.clone_from(&tr.model);
                        }
                    }
                }
                None => {
                    active_span = Some(ActiveNearFullSpan {
                        start_turn: tr.turn,
                        end_turn: tr.turn,
                        model: tr.model.clone(),
                        request_count: 1,
                        context_window_tokens: win.tokens,
                        window_source: win.source,
                        peak_prompt_tokens: p,
                        peak_window_pct: pct,
                        tokens_resent: p,
                        cache_read_tokens: ta.measured.cache_read,
                        confidence: ta.measured_confidence,
                    });
                }
            }
        } else {
            flush_near_full_span(
                active_span.take(),
                flags_cfg.near_full_context.min_requests,
                &mut findings,
            );
        }
    }
    flush_near_full_span(
        active_span.take(),
        flags_cfg.near_full_context.min_requests,
        &mut findings,
    );

    // 4. `tool-output-bloat`: ToolResult >= N tokens carried >= K later requests.
    for blk in &attrib.carried_blocks {
        if blk.kind != BlockKind::ToolResult
            || blk.scaled_prompt_tokens < flags_cfg.tool_output_bloat.min_tool_tokens
            || blk.carried_turns < flags_cfg.tool_output_bloat.min_carried_turns
        {
            continue;
        }
        let Some(born) = blk.born_turn else {
            continue;
        };
        let last_turn = main_turns
            .iter()
            .filter(|(tr, _)| {
                tr.turn >= born && blk.evicted_before_turn.is_none_or(|ev| tr.turn < ev)
            })
            .map(|(tr, _)| tr.turn)
            .max()
            .unwrap_or(born);

        let tool = blk.tool.clone().unwrap_or_else(|| "tool".to_string());
        let carried_tokens = blk.carried_prompt_tokens();
        findings.push(FlagFinding {
            id: FlagId::ToolOutputBloat,
            turn_range: TurnRange::new(born, last_turn),
            impact_tokens: carried_tokens,
            confidence: Confidence::Estimated,
            estimation_method: Some(attrib.estimation_method),
            evidence: FlagEvidence::ToolOutputBloat {
                tool,
                source_turn: blk.source_turn,
                bytes: blk.bytes,
                size_tokens: blk.scaled_prompt_tokens,
                carried_turns: blk.carried_turns,
                carried_tokens,
                carried_split: blk.carried_split,
            },
        });
    }

    // 5. `cache-miss-storm`: >= M mid-session requests with cache_write >> cache_read.
    let mut storm_turns: Vec<(&TurnRecord, &TurnRecord, &TurnAttribution)> = Vec::new();
    for window in main_turns.windows(2) {
        let (prev_tr, _) = window[0];
        let (tr, ta) = window[1];
        if tr.hints.resume_start || tr.hints.compaction {
            continue;
        }
        let gap =
            u64::try_from(tr.ts.duration_seconds_since(prev_tr.ts).max(0)).unwrap_or(u64::MAX);
        if gap > flags_cfg.cache_miss_storm.max_gap_seconds {
            continue;
        }
        let p = ta.measured.prompt_tokens();
        let cw = ta.measured.cache_write;
        let cr = ta.measured.cache_read;
        if p >= flags_cfg.cache_miss_storm.min_prompt_tokens
            && cw >= flags_cfg.cache_miss_storm.min_write_tokens
            && u128::from(cw)
                >= u128::from(cr) * u128::from(flags_cfg.cache_miss_storm.write_to_read_ratio)
        {
            storm_turns.push((prev_tr, tr, ta));
        }
    }

    let storm_count = u32::try_from(storm_turns.len()).unwrap_or(u32::MAX);
    if storm_count >= flags_cfg.cache_miss_storm.min_requests {
        let first_turn = storm_turns[0].1.turn;
        let last_turn = storm_turns[storm_turns.len() - 1].1.turn;
        let mut rewritten_tokens: u64 = 0;
        let mut cache_read_tokens: u64 = 0;
        let mut saw_model_switch = false;
        let mut saw_sys_or_mcp_change = false;
        let mut conf = Confidence::Measured;

        for (prev_tr, tr, ta) in &storm_turns {
            rewritten_tokens = rewritten_tokens.saturating_add(ta.measured.cache_write);
            cache_read_tokens = cache_read_tokens.saturating_add(ta.measured.cache_read);
            conf = merge_confidence(conf, ta.measured_confidence);
            if prev_tr
                .model
                .as_deref()
                .zip(tr.model.as_deref())
                .is_some_and(|(prev_m, cur_m)| prev_m != cur_m)
            {
                saw_model_switch = true;
            }
            if ta.new_unattributed_prompt_tokens > 0
                || tr.added.iter().any(|b| b.kind == BlockKind::SystemContext)
            {
                saw_sys_or_mcp_change = true;
            }
        }

        let likely_trigger = if saw_model_switch {
            CacheMissTrigger::ModelSwitch
        } else if saw_sys_or_mcp_change {
            CacheMissTrigger::SystemOrMcpChange
        } else {
            CacheMissTrigger::PrefixInvalidation
        };

        findings.push(FlagFinding {
            id: FlagId::CacheMissStorm,
            turn_range: TurnRange::new(first_turn, last_turn),
            impact_tokens: rewritten_tokens,
            confidence: conf,
            estimation_method: None,
            evidence: FlagEvidence::CacheMissStorm {
                miss_requests: storm_count,
                rewritten_tokens,
                cache_read_tokens,
                likely_trigger,
            },
        });
    }

    // Deterministic ranking (`PLAN.md` §6.3): impact_tokens DESC, turn_range ASC, FlagId ASC.
    findings.sort_by(|a, b| {
        b.impact_tokens
            .cmp(&a.impact_tokens)
            .then_with(|| a.turn_range.cmp(&b.turn_range))
            .then_with(|| a.id.cmp(&b.id))
    });

    findings
}
