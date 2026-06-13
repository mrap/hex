//! The reusable tap seam. A [`LineObserver`] turns one raw log line into an
//! optional [`Observation`]. This is the ONLY upstream-specific surface: adding a
//! new upstream is a new impl (sibling file) + a registry entry in
//! [`super::observer_registry`] — never a runtime config DSL. Observers are pure:
//! no I/O, no emit, no clock — so they are trivially unit-tested.

/// Severity of a single observed line, by latency band. Declaration order is the
/// `Ord` order (`Normal < Degraded < Stall`), so episodes can track the peak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Below the degraded floor — no per-line event and no coalescer feed.
    Normal,
    /// At or above the degraded floor, below the stall floor.
    Degraded,
    /// At or above the stall floor.
    Stall,
}

impl Severity {
    /// Telemetry `status` string for this severity.
    pub fn status(&self) -> &'static str {
        match self {
            Severity::Normal => "ok",
            Severity::Degraded => "degraded",
            Severity::Stall => "stall",
        }
    }

    /// Classify a latency (ms) against a degraded floor and a stall floor.
    /// `stall_ms` is expected to be >= `degraded_ms`.
    pub fn classify(value_ms: f64, degraded_ms: f64, stall_ms: f64) -> Severity {
        if value_ms >= stall_ms {
            Severity::Stall
        } else if value_ms >= degraded_ms {
            Severity::Degraded
        } else {
            Severity::Normal
        }
    }
}

/// One parsed, classified data point from a single log line.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// The headline latency this observer measures, in **milliseconds**.
    pub value_ms: f64,
    /// The dominant/most-relevant stage name, when the source breaks work into
    /// stages (used for attribution); `None` otherwise.
    pub stage: Option<String>,
    /// Severity by latency band.
    pub severity: Severity,
}

/// The reusable tap. One impl per upstream log format. Pure — no I/O, no emit.
pub trait LineObserver: Send {
    /// Stable identifier used by the registry and the `--observer` flag.
    fn name(&self) -> &'static str;

    /// Parse + classify one raw line. `None` = the line is not relevant (skip it).
    /// MUST NOT panic on malformed input — return `None` instead (a corrupt or
    /// truncated line must never take the daemon down).
    fn observe(&self, line: &str) -> Option<Observation>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_bands() {
        assert_eq!(Severity::classify(10.0, 500.0, 2000.0), Severity::Normal);
        assert_eq!(Severity::classify(500.0, 500.0, 2000.0), Severity::Degraded);
        assert_eq!(
            Severity::classify(1999.9, 500.0, 2000.0),
            Severity::Degraded
        );
        assert_eq!(Severity::classify(2000.0, 500.0, 2000.0), Severity::Stall);
        assert_eq!(Severity::classify(8800.0, 500.0, 2000.0), Severity::Stall);
    }

    #[test]
    fn status_strings() {
        assert_eq!(Severity::Normal.status(), "ok");
        assert_eq!(Severity::Degraded.status(), "degraded");
        assert_eq!(Severity::Stall.status(), "stall");
    }
}
