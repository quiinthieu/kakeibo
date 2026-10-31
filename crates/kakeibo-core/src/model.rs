//! Pure domain data model (`PLAN.md` §4).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::usage::Usage;

fn is_false(value: &bool) -> bool {
    !*value
}

/// Errors returned when constructing or parsing a [`Timestamp`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimestampError {
    /// Input string was not a valid RFC 3339 timestamp.
    #[error("invalid RFC 3339 timestamp `{input}`: {reason}")]
    InvalidRfc3339 { input: String, reason: String },
    /// Unix timestamp integer fell outside the representable UTC range.
    #[error("unix timestamp out of range: {value}")]
    UnixTimestampOutOfRange { value: i128 },
}

/// Pure UTC instant timestamp (`ADR 001`).
///
/// Wraps `time::OffsetDateTime` normalised to `UtcOffset::UTC` with no
/// wall-clock or local-timezone constructors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    /// Parses an RFC 3339 timestamp string and normalises its offset to UTC.
    pub fn parse_rfc3339(input: &str) -> Result<Self, TimestampError> {
        OffsetDateTime::parse(input, &Rfc3339)
            .map(|dt| Self(dt.to_offset(UtcOffset::UTC)))
            .map_err(|err| TimestampError::InvalidRfc3339 {
                input: input.to_string(),
                reason: err.to_string(),
            })
    }

    /// Constructs a UTC [`Timestamp`] from whole Unix epoch seconds.
    pub fn from_unix_timestamp(seconds: i64) -> Result<Self, TimestampError> {
        OffsetDateTime::from_unix_timestamp(seconds)
            .map(|dt| Self(dt.to_offset(UtcOffset::UTC)))
            .map_err(|_| TimestampError::UnixTimestampOutOfRange {
                value: i128::from(seconds),
            })
    }

    /// Constructs a UTC [`Timestamp`] from Unix epoch nanoseconds.
    pub fn from_unix_timestamp_nanos(nanos: i128) -> Result<Self, TimestampError> {
        OffsetDateTime::from_unix_timestamp_nanos(nanos)
            .map(|dt| Self(dt.to_offset(UtcOffset::UTC)))
            .map_err(|_| TimestampError::UnixTimestampOutOfRange { value: nanos })
    }

    /// Returns whole seconds since the Unix epoch (`1970-01-01T00:00:00Z`).
    #[must_use]
    pub const fn unix_timestamp(self) -> i64 {
        self.0.unix_timestamp()
    }

    /// Returns nanoseconds since the Unix epoch (`1970-01-01T00:00:00Z`).
    #[must_use]
    pub const fn unix_timestamp_nanos(self) -> i128 {
        self.0.unix_timestamp_nanos()
    }

    /// Returns signed whole seconds elapsed from `earlier` to `self`.
    #[must_use]
    pub const fn duration_seconds_since(self, earlier: Self) -> i64 {
        self.unix_timestamp()
            .saturating_sub(earlier.unix_timestamp())
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let formatted = self.0.format(&Rfc3339).map_err(|_| fmt::Error)?;
        // Canonicalise `+00:00` suffix (if ever emitted) to `Z` and strip
        // redundant trailing fractional zeros while keeping non-zero subseconds.
        let stem = formatted
            .strip_suffix('Z')
            .or_else(|| formatted.strip_suffix("+00:00"))
            .unwrap_or(&formatted);

        if let Some((date_time, frac)) = stem.split_once('.') {
            let trimmed = frac.trim_end_matches('0');
            if trimmed.is_empty() {
                write!(f, "{date_time}Z")
            } else {
                write!(f, "{date_time}.{trimmed}Z")
            }
        } else {
            write!(f, "{stem}Z")
        }
    }
}

impl FromStr for Timestamp {
    type Err = TimestampError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_rfc3339(s)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse_rfc3339(&raw).map_err(serde::de::Error::custom)
    }
}

/// Error returned when parsing an unknown [`AgentId`] slug.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown agent identifier `{0}`")]
pub struct UnknownAgentError(pub String);

/// Supported coding-agent source identifiers (`PLAN.md` §1.1, §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentId {
    ClaudeCode,
    Codex,
    GeminiCli,
    Opencode,
    Aider,
    Cursor,
}

impl AgentId {
    /// All supported v1 agent identifiers in canonical order.
    pub const ALL: [Self; 6] = [
        Self::ClaudeCode,
        Self::Codex,
        Self::GeminiCli,
        Self::Opencode,
        Self::Aider,
        Self::Cursor,
    ];

    /// Canonical `kebab-case` wire slug.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::GeminiCli => "gemini-cli",
            Self::Opencode => "opencode",
            Self::Aider => "aider",
            Self::Cursor => "cursor",
        }
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentId {
    type Err = UnknownAgentError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude-code" => Ok(Self::ClaudeCode),
            "codex" => Ok(Self::Codex),
            "gemini-cli" => Ok(Self::GeminiCli),
            "opencode" => Ok(Self::Opencode),
            "aider" => Ok(Self::Aider),
            "cursor" => Ok(Self::Cursor),
            other => Err(UnknownAgentError(other.to_string())),
        }
    }
}

/// Provenance confidence label attached to every token figure (`PLAN.md` §0 #7, §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Exact counts reported directly in the agent log.
    Measured,
    /// Heuristic or rounded counts (pair with `Usage::estimation_method`).
    Estimated,
    /// Agent record omitted or zeroed token counts (e.g. Cursor `0` counts).
    NotRecorded,
}

impl Confidence {
    /// Canonical `snake_case` wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::Estimated => "estimated",
            Self::NotRecorded => "not_recorded",
        }
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Structural classification of a single conversation block (`PLAN.md` §4, §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    UserPrompt,
    AssistantText,
    AssistantThinking,
    ToolUse,
    ToolResult,
    SubagentResult,
    CompactionSummary,
    SystemContext,
}

impl BlockKind {
    /// All block kinds in canonical order.
    pub const ALL: [Self; 8] = [
        Self::UserPrompt,
        Self::AssistantText,
        Self::AssistantThinking,
        Self::ToolUse,
        Self::ToolResult,
        Self::SubagentResult,
        Self::CompactionSummary,
        Self::SystemContext,
    ];

    /// Canonical `snake_case` wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserPrompt => "user_prompt",
            Self::AssistantText => "assistant_text",
            Self::AssistantThinking => "assistant_thinking",
            Self::ToolUse => "tool_use",
            Self::ToolResult => "tool_result",
            Self::SubagentResult => "subagent_result",
            Self::CompactionSummary => "compaction_summary",
            Self::SystemContext => "system_context",
        }
    }
}

impl fmt::Display for BlockKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Derived size and metadata for one block added since the prior request (`PLAN.md` §4).
///
/// Raw transcript text is dropped at parse time; only byte/token sizes, tool
/// names, and irreversible `key_hash` digests are retained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlock {
    pub kind: BlockKind,
    pub est_tokens: u64,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_hash: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
}

/// Structural hints observed on a turn (`PLAN.md` §4, §5, §6.3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnHints {
    #[serde(default)]
    pub resume_start: bool,
    #[serde(default)]
    pub compaction: bool,
    #[serde(default)]
    pub sidechain: bool,
    #[serde(default)]
    pub api_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_used_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_override: Option<u64>,
}

/// One deduplicated model API request (`PLAN.md` §4); contains no transcript text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub agent: AgentId,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub turn: u32,
    pub ts: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd_hash: Option<String>,
    pub usage: Usage,
    pub added: Vec<ContentBlock>,
    pub hints: TurnHints,
}

/// Attribution categories explaining where a session's tokens went (`PLAN.md` §4, §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    FixedOverhead,
    ResentHistory,
    ToolOutput,
    Retries,
    UserInput,
    AssistantOutput,
    Subagents,
    Compaction,
    Unattributed,
}

impl Category {
    /// All nine attribution categories in canonical order.
    pub const ALL: [Self; 9] = [
        Self::FixedOverhead,
        Self::ResentHistory,
        Self::ToolOutput,
        Self::Retries,
        Self::UserInput,
        Self::AssistantOutput,
        Self::Subagents,
        Self::Compaction,
        Self::Unattributed,
    ];

    /// Canonical `snake_case` wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FixedOverhead => "fixed_overhead",
            Self::ResentHistory => "resent_history",
            Self::ToolOutput => "tool_output",
            Self::Retries => "retries",
            Self::UserInput => "user_input",
            Self::AssistantOutput => "assistant_output",
            Self::Subagents => "subagents",
            Self::Compaction => "compaction",
            Self::Unattributed => "unattributed",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown [`Category`] slug.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown attribution category `{0}`")]
pub struct UnknownCategoryError(pub String);

impl FromStr for Category {
    type Err = UnknownCategoryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fixed_overhead" | "fixed-overhead" => Ok(Self::FixedOverhead),
            "resent_history" | "resent-history" => Ok(Self::ResentHistory),
            "tool_output" | "tool-output" => Ok(Self::ToolOutput),
            "retries" => Ok(Self::Retries),
            "user_input" | "user-input" => Ok(Self::UserInput),
            "assistant_output" | "assistant-output" => Ok(Self::AssistantOutput),
            "subagents" => Ok(Self::Subagents),
            "compaction" => Ok(Self::Compaction),
            "unattributed" => Ok(Self::Unattributed),
            other => Err(UnknownCategoryError(other.to_string())),
        }
    }
}
