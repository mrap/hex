//! Folded in from the former `hex memory llm-check` subcommand.
//! Probes LLM provider reachability via memory::provider::health_check().
//! Deferred (no key / not configured / test env) → SKIP; upstream error → WARN.

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
            // Compile-only arm for U4/KTD6 phase A: Truncated is not yet
            // produced by any real call path (parse_chat_response isn't wired
            // into generate_inner yet). Reported the same as Upstream for now.
            Err(ProviderError::Truncated(msg)) => {
                CheckResult::warn(format!("LLM provider truncated response — {msg}"))
            }
        }
    }
}
