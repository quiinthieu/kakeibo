//! Incremental index, user configuration, and menu bar snapshot management for Kakeibo.
//!
//! Architectural invariant (`PLAN.md` §0 #5 & §3.1):
//! This crate is the sole writer in Kakeibo (writing only inside Kakeibo's own
//! cache/config directories).
