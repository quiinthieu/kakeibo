//! Pure core library for Kakeibo.
//!
//! Architectural invariant (`PLAN.md` §0 #3):
//! This crate performs no I/O, reads no clock, accesses no runtime environment
//! variables, and makes no network calls.

pub mod ledger;
pub mod model;
pub mod product;
pub mod usage;

pub use model::{
    AgentId, BlockKind, Category, Confidence, ContentBlock, Timestamp, TimestampError, TurnHints,
    TurnRecord, UnknownAgentError,
};
pub use usage::{CacheWriteSplit, DisjointUsageInput, InclusiveUsageInput, Usage, UsageError};
