// Self-alias so module files compiled into this crate via `#[path] mod` (the
// `*.worker.rs` overlay) can `use hex::…` uniformly, whether they live in-crate
// (core modules) or out-of-crate (personal modules).
extern crate self as hex;

pub mod act_evidence;
pub mod alert;
pub mod backup;
pub mod claude_runs;
pub mod dial;
pub mod doctor;
pub mod failures;
pub mod gatekeeper;
pub mod harness;
pub mod ledger;
pub mod lint_gates;
pub mod llm_cost;
pub mod log_tail;
pub mod reconciler;
pub mod capability_exec;
pub mod charter;
pub mod audit;
pub mod capability_guard;
pub mod llm_config;
pub mod memory;
pub mod messages;
pub mod module_state;
pub mod ops;
pub mod reaper;
pub mod registry;
pub mod release;
pub mod resources;
pub mod sanitize;
pub mod telemetry;
pub mod types;
pub mod wild;
pub mod worker;
pub mod workers;
