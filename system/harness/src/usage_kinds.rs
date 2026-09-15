//! Canonical list of `hex usage collect` source kinds.
//!
//! Lives in the lib crate (not `usage.rs`, which is bin-only via `mod usage;`
//! in `main.rs`) so both the bin's `usage collect` command and in-crate
//! worker modules (`modules/usage_tracking.worker.rs`) can share one
//! definition instead of duplicating the list and drifting apart.

/// Every source kind `hex usage collect` knows, in the order a bare
/// invocation runs them. Adding a kind = add it here and in `usage.rs`'s
/// `collect` match, and note it in docs/hex-ops.md "Usage sources".
pub const ALL_SOURCE_KINDS: &[&str] = &[
    "codex-jsonl",
    "claude-transcripts",
    "boi-phase-runs",
    "harness-llm-cost",
    "headless-claude-json",
];
