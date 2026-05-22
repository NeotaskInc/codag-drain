//! # codag
//!
//! Real-time, evidence-preserving log **capsule engine**, built on top of the
//! base streaming Drain3 core (`drain3_rust`).
//!
//! It turns a stream (or window) of log lines into an `IncidentCapsule`: distinct
//! event templates with deterministic role tags, value distributions, kept signal
//! and value-outlier lines, and a chronological view — tunable from fully lossless
//! to a synchronous-agent-optimized budget.
//!
//! Layout (lands across Phase 1–2):
//! - `compress`  — deterministic compressor (normalize, guard, grouper, template,
//!   role, profile/keep-policy) + `compress()`/`render()`.
//! - `capsule`   — the `IncidentCapsule` product shape + builder.
//! - `stream`    — `StreamingIndex`: online structural grouping + incremental state.

// Phase 1+ modules are declared here as they land:
pub mod compress;
// pub mod capsule;
// pub mod stream;

// Public re-exports (Phase 1 compression library).
pub use compress::{
    compress, CompressionResult, CompressorConfig, GrouperKind, LogLine, Mode, OutputLine, Role,
    SlotStat,
};
