//! ZnonClip Core: shared storage, privacy filters, and secret scrubbing.
//!
//! This crate provides the foundational types and functions used by the
//! Mac app, CLI, and agent tools. All crates share the same storage format
//! and privacy guarantees.

pub mod scrub;
pub mod storage;

pub use scrub::scrub_secrets;
pub use storage::ClipStore;
