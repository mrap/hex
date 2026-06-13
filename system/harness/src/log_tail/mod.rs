//! `hex log-tail` — a generic, event-driven log-tail daemon.
//!
//! Follows an append-only log file via `notify` (FSEvents / inotify / kqueue —
//! blocks on kernel FS events, no polling), parses each new line through a
//! pluggable [`observer::LineObserver`], coalesces stall bursts into episodes,
//! and emits **durable-first** to hex telemetry + the event bus.
//!
//! ## Why a CLI daemon and not a `*.worker.rs`
//! The Rust worker SDK runs handlers on `spawn_blocking` — they must return; there
//! is no daemon/continuous-loop primitive. A continuous `tail -F` in a cron handler
//! would degrade to a poll. The hex-idiomatic continuous mechanism is the
//! `iii-exec` supervised daemon (the same pattern that runs the headroom proxy):
//! run `hex log-tail …` as a supervised long-lived process.
//!
//! ## Reusable seam
//! The only upstream-specific code is the [`observer::LineObserver`] impl selected
//! by `--observer`. A new upstream = a new impl + a [`observer_registry`] entry —
//! never a runtime config DSL (a typed Rust trait, checked at compile time).

pub mod coalesce;
pub mod emit;
pub mod headroom;
pub mod observer;
pub mod reader;

use coalesce::EpisodeCoalescer;
use emit::EmitArgs;
use notify::Watcher;
use observer::{LineObserver, Severity};
use reader::TailReader;
use serde_json::json;
use std::time::{Duration, Instant};

/// iii-state scope for the persisted byte-offset checkpoint.
const STATE_SCOPE: &str = "log-tail";

/// Names every registered observer understands — the single source for the
/// `--observer` registry below and the not-found error message.
pub const KNOWN_OBSERVERS: &[&str] = &["headroom-stage-timings"];

/// Smallest accepted quiet window (ms). Guards against `--quiet-ms 0`, which would
/// turn `recv_timeout` into a 100%-CPU busy-spin under iii-exec supervision.
const MIN_QUIET_MS: u64 = 50;

/// Daemon configuration (one tailer instance).
pub struct LogTailConfig {
    /// Absolute path of the log file to follow.
    pub path: String,
    /// Observer impl name (see [`KNOWN_OBSERVERS`]).
    pub observer: String,
    /// Bus event name to emit (e.g. `headroom.overhead`).
    pub event: String,
    /// Telemetry `source` + bus producer label (e.g. `headroom-proxy`).
    pub source: String,
    /// Degraded latency floor (ms). Stall floor = 4× this.
    pub threshold_ms: f64,
    /// Quiet window (ms) that closes a stall episode (clamped to >= 50).
    pub quiet_ms: u64,
    /// Read the whole file from byte 0 (default: only new lines from EOF).
    pub from_start: bool,
}

/// Resolve an observer impl by name. The reusable registry — extend here (and in
/// [`KNOWN_OBSERVERS`]) when a new upstream needs tailing.
pub fn observer_registry(name: &str, threshold_ms: f64) -> Option<Box<dyn LineObserver>> {
    match name {
        "headroom-stage-timings" => Some(Box::new(headroom::HeadroomStageTimings {
            degraded_ms: threshold_ms,
            stall_ms: threshold_ms * 4.0,
        })),
        _ => None,
    }
}

/// Run the daemon. Blocks until the watcher channel disconnects. Intended to run
/// under iii-exec supervision. Returns a process exit code (non-zero = the
/// supervisor should respawn).
pub fn run(cfg: LogTailConfig) -> i32 {
    let observer = match observer_registry(&cfg.observer, cfg.threshold_ms) {
        Some(o) => o,
        None => {
            eprintln!(
                "hex log-tail: unknown observer '{}' (known: {})",
                cfg.observer,
                KNOWN_OBSERVERS.join(", ")
            );
            return 2;
        }
    };

    let mut reader = TailReader::new(&cfg.path, cfg.from_start);
    // Restore the checkpoint if present — resume without replay or drop. A state
    // error is loud (S6): we proceed from EOF but the operator must see the outage.
    match crate::ops::state_get(STATE_SCOPE, &checkpoint_key(&cfg)) {
        Ok(Some(v)) => {
            if let Some(off) = v.as_u64() {
                reader.set_offset(off);
            }
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!("hex log-tail: failed to restore checkpoint (starting from EOF): {e}");
        }
    }

    let parent = match reader.parent_dir() {
        Some(p) => p.to_path_buf(),
        None => {
            eprintln!("hex log-tail: path '{}' has no parent directory", cfg.path);
            return 2;
        }
    };

    let quiet = Duration::from_millis(cfg.quiet_ms.max(MIN_QUIET_MS));
    let mut coalescer = EpisodeCoalescer::new(quiet);

    // Arm the watcher BEFORE the initial drain so lines appended during setup are
    // not missed (they'd otherwise wait for the next unrelated FS event).
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = match notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("hex log-tail: cannot create file watcher: {e}");
            return 1;
        }
    };
    // Watch the parent directory (non-recursive) so log rotation — the file being
    // recreated — is observed, not just appends to the current inode.
    if let Err(e) = watcher.watch(&parent, notify::RecursiveMode::NonRecursive) {
        eprintln!("hex log-tail: cannot watch '{}': {e}", parent.display());
        return 1;
    }

    eprintln!(
        "hex log-tail: following {} via observer '{}' → event '{}' (durable source '{}')",
        cfg.path,
        observer.name(),
        cfg.event,
        cfg.source
    );

    // Initial catch-up drain (watcher already armed → no startup race).
    drain(&mut reader, observer.as_ref(), &mut coalescer, &cfg);

    loop {
        match rx.recv_timeout(quiet) {
            // A real FS event → drain. (Inner notify errors are surfaced, not
            // silently treated as events — S6.)
            Ok(Ok(_event)) => drain(&mut reader, observer.as_ref(), &mut coalescer, &cfg),
            Ok(Err(e)) => eprintln!("hex log-tail: notify backend error (continuing): {e}"),
            // Idle for `quiet` — flush any open stall episode.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(ep) = coalescer.tick(Instant::now()) {
                    emit_episode(&cfg, &ep);
                }
            }
            // Abnormal watcher-thread death (normal teardown is SIGTERM, which never
            // reaches here). Non-zero so iii-exec respawns rather than silently dying.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("hex log-tail: watcher channel disconnected (watcher died); exiting non-zero for respawn");
                return 1;
            }
        }
    }
}

fn checkpoint_key(cfg: &LogTailConfig) -> String {
    // Key by source so concurrent tailers (different upstreams) never collide.
    format!("offset:{}", cfg.source)
}

fn drain(
    reader: &mut TailReader,
    observer: &dyn LineObserver,
    coalescer: &mut EpisodeCoalescer,
    cfg: &LogTailConfig,
) {
    let before = reader.offset();
    let lines = match reader.read_new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("hex log-tail: read error on {}: {e}", cfg.path);
            return;
        }
    };

    for line in &lines {
        let Some(o) = observer.observe(line) else {
            continue;
        };
        // Normal lines are ignored entirely — only Degraded/Stall drive events and
        // episode coalescing (there is no Normal-feed path on the coalescer).
        if o.severity == Severity::Normal {
            continue;
        }
        emit::emit_durable_first(EmitArgs {
            source: &cfg.source,
            event: "overhead",
            bus_event: &cfg.event,
            status: o.severity.status(),
            value_ms: o.value_ms,
            detail: json!({ "stage": o.stage }),
            data: json!({
                "severity": o.severity.status(),
                "value_ms": o.value_ms,
                "stage": o.stage,
                "source": cfg.source,
            }),
        });
        if let Some(ep) =
            coalescer.record_breach(Instant::now(), o.value_ms, o.severity, o.stage.clone())
        {
            emit_episode(cfg, &ep);
        }
    }

    // Persist the checkpoint AFTER emitting (at-least-once: a crash before this
    // re-emits the batch on respawn rather than dropping it). Only when the offset
    // actually advanced — avoids spamming iii-state on no-op drains. Loud on
    // failure (S6): a silent checkpoint loss grows the replay/duplicate window.
    let after = reader.offset();
    if after != before {
        if let Err(e) = crate::ops::state_set(STATE_SCOPE, &checkpoint_key(cfg), &json!(after)) {
            eprintln!("hex log-tail: checkpoint persist failed (offset {after}): {e}");
        }
    }

    // Flush an episode whose quiet window has elapsed. Done on every drain (not
    // only on the idle Timeout arm) so steady Normal traffic after a burst still
    // closes the episode instead of leaving it open forever.
    if let Some(ep) = coalescer.tick(Instant::now()) {
        emit_episode(cfg, &ep);
    }
}

fn emit_episode(cfg: &LogTailConfig, ep: &coalesce::StallEpisode) {
    emit::emit_durable_first(EmitArgs {
        source: &cfg.source,
        event: "stall_episode",
        bus_event: &cfg.event,
        // Reflect the worst severity in the burst (an all-Degraded episode stays
        // "degraded"; any Stall makes it "stall").
        status: ep.peak_severity.status(),
        value_ms: ep.peak_ms,
        detail: json!({ "count": ep.count, "peak_ms": ep.peak_ms, "dominant_stage": ep.dominant_stage }),
        data: json!({
            "kind": "stall_episode",
            "count": ep.count,
            "peak_ms": ep.peak_ms,
            "severity": ep.peak_severity.status(),
            "dominant_stage": ep.dominant_stage,
            "source": cfg.source,
        }),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_headroom_and_rejects_unknown() {
        assert!(observer_registry("headroom-stage-timings", 500.0).is_some());
        assert!(observer_registry("does-not-exist", 500.0).is_none());
    }

    #[test]
    fn known_observers_all_resolve() {
        for name in KNOWN_OBSERVERS {
            assert!(
                observer_registry(name, 500.0).is_some(),
                "{name} must resolve"
            );
        }
    }

    #[test]
    fn registry_threshold_sets_stall_at_four_x() {
        let o = observer_registry("headroom-stage-timings", 500.0).unwrap();
        // Degraded at 500, so 1999 is Degraded and 2000 (4×500) is Stall.
        assert_eq!(
            o.observe(r#"x STAGE_TIMINGS {"stages":{"total_pre_upstream":1999.0}}"#)
                .unwrap()
                .severity,
            Severity::Degraded
        );
        assert_eq!(
            o.observe(r#"x STAGE_TIMINGS {"stages":{"total_pre_upstream":2000.0}}"#)
                .unwrap()
                .severity,
            Severity::Stall
        );
    }

    #[test]
    fn checkpoint_key_is_source_scoped() {
        let cfg = LogTailConfig {
            path: "/tmp/x".into(),
            observer: "headroom-stage-timings".into(),
            event: "headroom.overhead".into(),
            source: "headroom-proxy".into(),
            threshold_ms: 500.0,
            quiet_ms: 3000,
            from_start: false,
        };
        assert_eq!(checkpoint_key(&cfg), "offset:headroom-proxy");
    }
}
