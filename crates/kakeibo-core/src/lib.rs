//! Pure core library for the token budget diary workspace.
//!
//! Architectural invariant (`PLAN.md` §0 #3):
//! This crate performs no I/O, reads no clock, accesses no runtime environment
//! variables, and makes no network calls.

pub mod attrib;
pub mod flags;
pub mod ledger;
pub mod model;
pub mod product;
pub mod usage;

pub use attrib::{
    attribute_session, attribute_session_with_subagents, estimate_tokens_from_bytes,
    CarriedBlockRecord, CategoryBreakdown, SessionAttribution, SubagentSessionSummary, TokenSplit,
    TurnAttribution, ESTIMATION_METHOD_SCALED_BYTES_DIV4,
};
pub use flags::{
    evaluate_flags_v1, evaluate_flags_v1_default, CacheMissStormRuleConfig, CacheMissTrigger,
    ColdResumeRuleConfig, ColdResumeTrigger, ContextWindowSource, FlagConfigError, FlagEvidence,
    FlagFinding, FlagId, FlagsConfig, HeavyOverheadRuleConfig, ModelCatalog, ModelSpec,
    NearFullContextRuleConfig, ResolvedContextWindow, ToolOutputBloatRuleConfig, TurnRange,
    UnknownFlagError,
};
pub use model::{
    AgentId, BlockKind, Category, Confidence, ContentBlock, Timestamp, TimestampError, TurnHints,
    TurnRecord, UnknownAgentError,
};
pub use usage::{CacheWriteSplit, DisjointUsageInput, InclusiveUsageInput, Usage, UsageError};
