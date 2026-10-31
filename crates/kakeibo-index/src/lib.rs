//! Incremental index, user configuration, and menu bar snapshot management.
//!
//! Architectural invariant (`PLAN.md` §0 #5 & §3.1):
//! This crate is the sole writer in the workspace (writing only inside its own
//! cache/config directories).

pub mod paths;
pub mod pricing_store;
pub mod store;

pub use paths::AppPaths;
pub use pricing_store::{
    load_effective_pricing, save_user_pricing, EffectivePricing, PricingStoreError,
};
pub use store::{
    hash_source_id, CachedDiagnostics, CachedSessionTurns, IndexSnapshot, IndexStore,
    IndexedSourceRecord, PruneScope, RawIndexedRow, SourceFingerprint, SourceLookupOutcome,
    INDEX_SCHEMA_VERSION,
};
