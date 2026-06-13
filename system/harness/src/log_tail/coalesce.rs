//! Episode coalescer. A single event-loop stall makes many queued requests all
//! report high latency at once; counting them as N independent incidents misleads
//! the "is this getting worse?" decision and floods alerting. This collapses a
//! burst of Degraded/Stall observations into ONE [`StallEpisode`], closed after
//! `quiet_window` elapses with no further breach.
//!
//! Pure state machine: the caller supplies monotonic timestamps ([`Instant`]),
//! so it is unit-testable without a real clock.

use super::observer::Severity;
use std::time::{Duration, Instant};

/// One coalesced stall episode (a contiguous burst of breaches).
#[derive(Debug, Clone, PartialEq)]
pub struct StallEpisode {
    /// Number of breaching lines in the episode.
    pub count: u64,
    /// Worst latency seen in the episode (ms).
    pub peak_ms: f64,
    /// Worst severity seen in the episode (an all-Degraded burst stays Degraded;
    /// any Stall makes the whole episode Stall).
    pub peak_severity: Severity,
    /// Dominant stage at the peak, when known.
    pub dominant_stage: Option<String>,
}

pub struct EpisodeCoalescer {
    quiet_window: Duration,
    open: Option<OpenEpisode>,
}

struct OpenEpisode {
    count: u64,
    peak_ms: f64,
    peak_severity: Severity,
    dominant_stage: Option<String>,
    last_breach: Instant,
}

impl EpisodeCoalescer {
    pub fn new(quiet_window: Duration) -> Self {
        EpisodeCoalescer {
            quiet_window,
            open: None,
        }
    }

    /// Feed one breach (Degraded/Stall) observed at `now`. If this breach arrives
    /// after the prior episode's quiet window already elapsed, that prior episode
    /// is closed and returned (and a fresh one opens for this breach).
    pub fn record_breach(
        &mut self,
        now: Instant,
        value_ms: f64,
        severity: Severity,
        stage: Option<String>,
    ) -> Option<StallEpisode> {
        let gap_exceeded = self
            .open
            .as_ref()
            .is_some_and(|ep| now.duration_since(ep.last_breach) > self.quiet_window);
        let closed = if gap_exceeded {
            self.take_closed()
        } else {
            None
        };

        match &mut self.open {
            Some(ep) => {
                ep.count += 1;
                if value_ms > ep.peak_ms {
                    ep.peak_ms = value_ms;
                    ep.dominant_stage = stage;
                }
                ep.peak_severity = ep.peak_severity.max(severity);
                ep.last_breach = now;
            }
            None => {
                self.open = Some(OpenEpisode {
                    count: 1,
                    peak_ms: value_ms,
                    peak_severity: severity,
                    dominant_stage: stage,
                    last_breach: now,
                });
            }
        }
        closed
    }

    /// Call when idle (no new breaches): closes and returns an open episode whose
    /// quiet window has elapsed. Returns `None` if nothing is open or the window
    /// has not yet passed.
    pub fn tick(&mut self, now: Instant) -> Option<StallEpisode> {
        let elapsed = self
            .open
            .as_ref()
            .is_some_and(|ep| now.duration_since(ep.last_breach) > self.quiet_window);
        if elapsed {
            self.take_closed()
        } else {
            None
        }
    }

    fn take_closed(&mut self) -> Option<StallEpisode> {
        self.open.take().map(|ep| StallEpisode {
            count: ep.count,
            peak_ms: ep.peak_ms,
            peak_severity: ep.peak_severity,
            dominant_stage: ep.dominant_stage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_breach_then_quiet_closes_episode_of_one() {
        let mut c = EpisodeCoalescer::new(Duration::from_millis(100));
        let t0 = Instant::now();
        assert_eq!(
            c.record_breach(
                t0,
                600.0,
                Severity::Degraded,
                Some("read_request_json".into())
            ),
            None
        );
        // Not yet elapsed.
        assert_eq!(c.tick(t0 + Duration::from_millis(50)), None);
        // Elapsed → closes.
        let ep = c.tick(t0 + Duration::from_millis(200)).unwrap();
        assert_eq!(ep.count, 1);
        assert_eq!(ep.peak_ms, 600.0);
        assert_eq!(ep.peak_severity, Severity::Degraded);
        assert_eq!(ep.dominant_stage.as_deref(), Some("read_request_json"));
    }

    #[test]
    fn burst_within_window_coalesces_into_one_with_peak_and_count() {
        let mut c = EpisodeCoalescer::new(Duration::from_millis(100));
        let t0 = Instant::now();
        assert_eq!(
            c.record_breach(t0, 600.0, Severity::Degraded, Some("a".into())),
            None
        );
        assert_eq!(
            c.record_breach(
                t0 + Duration::from_millis(10),
                8800.0,
                Severity::Stall,
                Some("deep_copy".into())
            ),
            None
        );
        assert_eq!(
            c.record_breach(
                t0 + Duration::from_millis(20),
                700.0,
                Severity::Degraded,
                Some("a".into())
            ),
            None
        );
        let ep = c.tick(t0 + Duration::from_millis(200)).unwrap();
        assert_eq!(
            ep.count, 3,
            "three breaches in one burst → one episode of 3"
        );
        assert_eq!(ep.peak_ms, 8800.0, "peak is the worst in the burst");
        assert_eq!(
            ep.peak_severity,
            Severity::Stall,
            "any Stall makes the episode Stall"
        );
        assert_eq!(
            ep.dominant_stage.as_deref(),
            Some("deep_copy"),
            "dominant stage tracks the peak"
        );
    }

    #[test]
    fn all_degraded_burst_stays_degraded() {
        let mut c = EpisodeCoalescer::new(Duration::from_millis(100));
        let t0 = Instant::now();
        c.record_breach(t0, 600.0, Severity::Degraded, None);
        c.record_breach(
            t0 + Duration::from_millis(10),
            700.0,
            Severity::Degraded,
            None,
        );
        let ep = c.tick(t0 + Duration::from_millis(200)).unwrap();
        assert_eq!(
            ep.peak_severity,
            Severity::Degraded,
            "no Stall in the burst → episode is Degraded, not Stall"
        );
    }

    #[test]
    fn gap_closes_prior_and_opens_new() {
        let mut c = EpisodeCoalescer::new(Duration::from_millis(100));
        let t0 = Instant::now();
        assert_eq!(c.record_breach(t0, 600.0, Severity::Degraded, None), None);
        // Next breach after the quiet window → returns the closed prior episode.
        let closed = c
            .record_breach(
                t0 + Duration::from_millis(300),
                900.0,
                Severity::Degraded,
                None,
            )
            .expect("prior episode should close");
        assert_eq!(closed.count, 1);
        assert_eq!(closed.peak_ms, 600.0);
        // The new episode is still open until its own quiet window passes.
        let next = c.tick(t0 + Duration::from_millis(500)).unwrap();
        assert_eq!(next.count, 1);
        assert_eq!(next.peak_ms, 900.0);
    }

    #[test]
    fn tick_with_nothing_open_is_none() {
        let mut c = EpisodeCoalescer::new(Duration::from_millis(100));
        assert_eq!(c.tick(Instant::now()), None);
    }
}
