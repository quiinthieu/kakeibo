//! SIMD-accelerated `memchr::memmem::Finder` JSONL pre-filters and zero-allocation
//! borrowed `serde` envelopes (`PLAN.md` §7, §10 T12).
//!
//! Skips full DOM deserialization on non-turn metadata, snapshot, and streaming-delta
//! JSONL lines while still validating JSON syntax in a single zero-allocation pass
//! and preserving exact `ignored_records`, `cli_versions`, and `unknown_record_types`
//! diagnostics.

use std::borrow::Cow;
use std::sync::LazyLock;

use memchr::memmem::Finder;
use serde::Deserialize;

static FINDER_CLAUDE_ASSISTANT: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"assistant\""));
static FINDER_CLAUDE_USER: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"user\""));
static FINDER_CLAUDE_SYSTEM: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"system\""));

static FINDER_CODEX_SESSION_META: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"session_meta\""));
static FINDER_CODEX_TURN_CONTEXT: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"turn_context\""));
static FINDER_CODEX_RESPONSE_ITEM: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"response_item\""));
static FINDER_CODEX_TUR: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"token_usage_record\""));
static FINDER_CODEX_COMPACTED: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"compacted\""));
static FINDER_CODEX_EVENT_MSG: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"event_msg\""));
static FINDER_CODEX_TOKEN_COUNT: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"token_count\""));
static FINDER_CODEX_CONTEXT_COMPACTED: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"context_compacted\""));
static FINDER_CODEX_ERROR: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"error\""));
static FINDER_CODEX_STREAM_ERROR: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"stream_error\""));

static FINDER_AIDER_MESSAGE_SEND: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"message_send\""));
static FINDER_AIDER_LAUNCHED: LazyLock<Finder<'static>> =
    LazyLock::new(|| Finder::new(b"\"launched\""));

/// Fast classification for a Claude Code JSONL line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeLineClass {
    /// Line cannot be `"user"`, `"assistant"`, or `"system"`; handle via zero-allocation envelope.
    NonTurnFastPath,
    /// Line contains `"user"`, `"assistant"`, or `"system"`; run full turn/system decode.
    CandidateTurnOrSystem,
}

/// Classifies a trimmed Claude Code JSONL line using precompiled SIMD `memmem` finders.
#[must_use]
pub fn classify_claude_line(trimmed: &str) -> ClaudeLineClass {
    let bytes = trimmed.as_bytes();
    if FINDER_CLAUDE_ASSISTANT.find(bytes).is_some()
        || FINDER_CLAUDE_USER.find(bytes).is_some()
        || FINDER_CLAUDE_SYSTEM.find(bytes).is_some()
    {
        ClaudeLineClass::CandidateTurnOrSystem
    } else {
        ClaudeLineClass::NonTurnFastPath
    }
}

/// Zero-allocation borrowed top-level envelope for Claude Code JSONL lines.
#[derive(Debug, Deserialize)]
pub struct BorrowedClaudeEnvelope<'a> {
    #[serde(rename = "type", borrow)]
    pub record_type: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub version: Option<Cow<'a, str>>,
}

/// Fast classification for an OpenAI Codex CLI JSONL line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexLineClass {
    /// Line cannot affect session state or turn usage (e.g. `world_state` or streaming deltas).
    NonTurnFastPath,
    /// Line may carry `session_meta`, `turn_context`, `response_item`, `token_usage_record`,
    /// `compacted`, or an actionable `event_msg` (`token_count`, `context_compacted`, `*error`).
    CandidateTurnOrState,
}

/// Classifies a trimmed Codex CLI JSONL line using precompiled SIMD `memmem` finders.
#[must_use]
pub fn classify_codex_line(trimmed: &str) -> CodexLineClass {
    let bytes = trimmed.as_bytes();
    if FINDER_CODEX_TUR.find(bytes).is_some()
        || FINDER_CODEX_RESPONSE_ITEM.find(bytes).is_some()
        || FINDER_CODEX_TURN_CONTEXT.find(bytes).is_some()
        || FINDER_CODEX_SESSION_META.find(bytes).is_some()
        || FINDER_CODEX_COMPACTED.find(bytes).is_some()
    {
        return CodexLineClass::CandidateTurnOrState;
    }

    if FINDER_CODEX_EVENT_MSG.find(bytes).is_some()
        && (FINDER_CODEX_TOKEN_COUNT.find(bytes).is_some()
            || FINDER_CODEX_CONTEXT_COMPACTED.find(bytes).is_some()
            || FINDER_CODEX_ERROR.find(bytes).is_some()
            || FINDER_CODEX_STREAM_ERROR.find(bytes).is_some())
    {
        return CodexLineClass::CandidateTurnOrState;
    }

    CodexLineClass::NonTurnFastPath
}

/// Zero-allocation borrowed top-level envelope for Codex CLI JSONL lines.
#[derive(Debug, Deserialize)]
pub struct BorrowedCodexEnvelope<'a> {
    #[serde(rename = "type", borrow)]
    pub record_type: Option<Cow<'a, str>>,
    #[serde(default)]
    pub ordinal: Option<u64>,
    #[serde(default, borrow)]
    pub timestamp: Option<Cow<'a, str>>,
}

/// Fast classification for an Aider `--analytics-log` JSONL line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiderAnalyticsClass {
    /// Non-turn event (`command_*`, etc.); handle via zero-allocation envelope.
    NonTurnFastPath,
    /// Candidate `"launched"` or `"message_send"` event.
    CandidateSessionOrTurn,
}

/// Classifies a trimmed Aider `--analytics-log` JSONL line using precompiled SIMD `memmem` finders.
#[must_use]
pub fn classify_aider_analytics_line(trimmed: &str) -> AiderAnalyticsClass {
    let bytes = trimmed.as_bytes();
    if FINDER_AIDER_MESSAGE_SEND.find(bytes).is_some()
        || FINDER_AIDER_LAUNCHED.find(bytes).is_some()
    {
        AiderAnalyticsClass::CandidateSessionOrTurn
    } else {
        AiderAnalyticsClass::NonTurnFastPath
    }
}

/// Zero-allocation borrowed properties envelope for skipped Aider analytics events.
#[derive(Debug, Default, Deserialize)]
pub struct BorrowedAiderProperties<'a> {
    #[serde(default, borrow)]
    pub version: Option<Cow<'a, str>>,
}

/// Zero-allocation borrowed top-level envelope for skipped Aider analytics events.
#[derive(Debug, Deserialize)]
pub struct BorrowedAiderEnvelope<'a> {
    #[serde(default, borrow)]
    pub event: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    pub properties: BorrowedAiderProperties<'a>,
}
