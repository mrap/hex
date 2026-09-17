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

/// Turn a `health_check()` outcome into the doctor's reported result. Pure
/// (no I/O) so the wording for each `ProviderError` arm — in particular the
/// Truncated/Upstream distinction (U4/KTD6) — can be pinned by a unit test
/// without a live provider.
fn classify(result: Result<String, ProviderError>) -> CheckResult {
    match result {
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

impl DoctorCheck for LlmProviderReachable {
    fn name(&self) -> &str {
        "llm-provider"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, _ctx: &Context) -> CheckResult {
        classify(provider::health_check())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::check::Status;

    #[test]
    fn classify_ok_passes() {
        let result = classify(Ok("all good".to_string()));
        assert_eq!(result.status, Status::Pass);
        assert_eq!(result.message, "LLM provider reachable");
    }

    #[test]
    fn classify_deferred_skips_with_message() {
        let result = classify(Err(ProviderError::Deferred(
            "no key configured".to_string(),
        )));
        assert_eq!(result.status, Status::Skip);
        assert_eq!(
            result.message,
            "LLM provider not configured — no key configured"
        );
    }

    #[test]
    fn classify_upstream_warns_with_upstream_wording() {
        let result = classify(Err(ProviderError::Upstream("connection reset".to_string())));
        assert_eq!(result.status, Status::Warn);
        assert_eq!(
            result.message,
            "LLM provider upstream error — connection reset"
        );
    }

    #[test]
    fn classify_truncated_warns_with_distinct_truncation_wording() {
        // U4/KTD6: truncation must read differently from a generic upstream
        // failure so an operator can tell "raise max_tokens" from "network/API
        // is down" at a glance, without reading past the status line.
        let result = classify(Err(ProviderError::Truncated(
            "finish_reason=length".to_string(),
        )));
        assert_eq!(result.status, Status::Warn);
        assert_eq!(
            result.message,
            "LLM provider truncated response — finish_reason=length"
        );
    }
}
