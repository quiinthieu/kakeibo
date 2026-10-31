//! Pure core library for Kakeibo.
//!
//! Architectural invariant (`PLAN.md` §0 #3):
//! This crate performs no I/O, reads no clock, accesses no runtime environment
//! variables, and makes no network calls.

pub mod product;
