//! Folded in from the former `hex memory llm-check` subcommand.
//! Probes LLM provider reachability via memory::provider::health_check().
//! Deferred (no key / not configured / test env) → SKIP; upstream error, or a
//! response cut off by the output cap (U4/KTD6) → WARN, with distinct wording
//! so an operator reading `hex doctor` output can tell a truncation (raise
//! max_tokens for the use case) from a real upstream/network failure at a
//! glance, without reading the message body.

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use crate::memory::provider::{self, ProviderError};

pub struct LlmProviderReachable;

impl DoctorCheck for LlmProviderReachable {
    fn name(&self) -> &str {
        "llm-provider"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, _ctx: &Context) -> CheckResult {
        match provider::health_check() {
            Ok(_) => CheckResult::pass("LLM provider reachable"),
            Err(ProviderError::Deferred(msg)) => {
                CheckResult::skip(format!("LLM provider not configured — {msg}"))
            }
            Err(ProviderError::Upstream(msg)) => {
                CheckResult::warn(format!("LLM provider upstream error — {msg}"))
            }
            // Decision (U4/KTD6): kept distinct from Upstream, not merged.
            // health_check runs on its own use case at its own max_tokens; a
            // truncation here means that use case's cap is too low, an
            // operator-actionable config problem, not a network/API failure —
            // the wording says which one happened.
            Err(ProviderError::Truncated(msg)) => {
                CheckResult::warn(format!("LLM provider truncated response — {msg}"))
            }
        }
    }
}
