//! `HeadroomStageTimings` — the first [`LineObserver`]. Parses headroom's
//! per-request `STAGE_TIMINGS {...}` log line and reports `total_pre_upstream`
//! (the proxy's single-threaded event-loop overhead, in **milliseconds**).
//!
//! headroom stays opaque/swappable: we read only the log line it already writes —
//! its documented stable parse surface (`headroom perf` depends on it). No proxy
//! code is touched, no internals are reached into. The contract test below pins
//! the format against a captured golden line so silent format drift fails loud.

use super::observer::{LineObserver, Observation, Severity};
use serde_json::Value;

/// The marker that precedes the JSON payload in a headroom STAGE_TIMINGS log line.
const MARKER: &str = "STAGE_TIMINGS ";

/// Stages that run synchronously on headroom's event loop before upstream hand-off
/// — i.e. the stages that can block `/livez`. The largest is reported as dominant.
const BLOCKING_STAGES: &[&str] = &["read_request_json", "deep_copy", "compression_first_stage"];

pub struct HeadroomStageTimings {
    /// Degraded floor (ms). Below this is Normal.
    pub degraded_ms: f64,
    /// Stall floor (ms). At/above this is Stall.
    pub stall_ms: f64,
}

impl Default for HeadroomStageTimings {
    fn default() -> Self {
        // 0.86% of requests exceed 500ms (the degraded floor); the observed tail
        // reaches several seconds (the stall floor). Both overridable via the CLI.
        HeadroomStageTimings {
            degraded_ms: 500.0,
            stall_ms: 2000.0,
        }
    }
}

impl LineObserver for HeadroomStageTimings {
    fn name(&self) -> &'static str {
        "headroom-stage-timings"
    }

    fn observe(&self, line: &str) -> Option<Observation> {
        // Fast reject: only STAGE_TIMINGS lines carry stage data.
        let after_marker = &line[line.find(MARKER)? + MARKER.len()..];
        // The payload is a JSON object after the marker (the log line prefixes a
        // timestamp/logger header, which we skip by locating the first '{').
        let json_start = after_marker.find('{')?;
        let v: Value = serde_json::from_str(after_marker[json_start..].trim()).ok()?;
        let stages = v.get("stages")?.as_object()?;

        // `total_pre_upstream` is already in milliseconds (verified against the
        // `headroom_stage_timing_ms_*` Prometheus series — do NOT scale).
        let value_ms = stages.get("total_pre_upstream")?.as_f64()?;

        let dominant = BLOCKING_STAGES
            .iter()
            .filter_map(|k| stages.get(*k).and_then(|x| x.as_f64()).map(|s| (*k, s)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(k, _)| k.to_string());

        Some(Observation {
            value_ms,
            stage: dominant,
            severity: Severity::classify(value_ms, self.degraded_ms, self.stall_ms),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs() -> HeadroomStageTimings {
        HeadroomStageTimings::default()
    }

    #[test]
    fn ignores_non_stage_timings_lines() {
        assert!(obs()
            .observe("2026-06-13 - headroom.proxy - INFO - some other log")
            .is_none());
        assert!(obs().observe("").is_none());
    }

    #[test]
    fn malformed_json_does_not_panic_and_returns_none() {
        assert!(obs()
            .observe("[req] STAGE_TIMINGS {not valid json")
            .is_none());
        assert!(obs().observe("[req] STAGE_TIMINGS {}").is_none()); // no `stages`
        assert!(obs()
            .observe(r#"[req] STAGE_TIMINGS {"stages": {}}"#)
            .is_none()); // no total_pre_upstream
    }

    #[test]
    fn parses_value_in_ms_without_scaling() {
        let line = r#"... STAGE_TIMINGS {"stages": {"total_pre_upstream": 1.54, "read_request_json": 0.6, "deep_copy": 0.1}}"#;
        let o = obs().observe(line).unwrap();
        assert!(
            (o.value_ms - 1.54).abs() < 1e-9,
            "value must be ms verbatim, got {}",
            o.value_ms
        );
        assert_eq!(o.severity, Severity::Normal);
        assert_eq!(
            o.stage.as_deref(),
            Some("read_request_json"),
            "dominant blocking stage"
        );
    }

    #[test]
    fn classifies_degraded_and_stall() {
        let degraded = r#"x STAGE_TIMINGS {"stages": {"total_pre_upstream": 750.0}}"#;
        assert_eq!(
            obs().observe(degraded).unwrap().severity,
            Severity::Degraded
        );
        let stall = r#"x STAGE_TIMINGS {"stages": {"total_pre_upstream": 8800.0}}"#;
        assert_eq!(obs().observe(stall).unwrap().severity, Severity::Stall);
    }

    /// Contract test: a real captured STAGE_TIMINGS line must still parse. If
    /// headroom changes its log format on upgrade, this fails loud BEFORE the
    /// telemetry silently goes dark (the silent-telemetry-death guard).
    #[test]
    fn golden_fixture_parses() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/headroom-stage-timings.jsonl"
        );
        let content = std::fs::read_to_string(fixture)
            .unwrap_or_else(|e| panic!("cannot read golden fixture {fixture}: {e}"));
        let line = content
            .lines()
            .next()
            .expect("fixture has at least one line");
        let o = obs()
            .observe(line)
            .expect("golden STAGE_TIMINGS line must parse — headroom log format may have drifted");
        // The captured line's total_pre_upstream is ~1.54 ms (a Normal request);
        // the contract is that it parses to a finite ms value, not its exact magnitude.
        assert!(o.value_ms.is_finite() && o.value_ms >= 0.0);
    }
}
