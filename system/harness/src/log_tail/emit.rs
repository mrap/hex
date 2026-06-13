//! Durable-first sink. The iii STREAM event bus is at-most-once and drops events
//! on disconnect/restart — exactly the moment a stall happens. So write the
//! durable telemetry row FIRST (SQLite), then emit to the bus best-effort. The
//! record is never lost to a bus drop. (A discipline worth propagating to any
//! loss-sensitive hex producer, not just this one.)
//!
//! **Delivery is at-least-once, not exactly-once.** The offset checkpoint is
//! persisted once per drain batch, so a crash between an emit and a successful
//! checkpoint re-emits that batch on respawn. Telemetry consumers must be
//! idempotent on `(source, event, ts, duration_ms)`.

use crate::telemetry::{record_loud, TelemetryEvent};
use serde_json::Value;

/// Named arguments for [`emit_durable_first`]. A struct (not positional args)
/// because `source/event/bus_event/status` are all `&str` and `detail/data` are
/// both `Value` — a positional transposition (especially `detail`↔`data`, which
/// would silently route the bus payload into the durable row and vice-versa)
/// would compile clean. Named fields make that class of bug impossible.
pub struct EmitArgs<'a> {
    /// Telemetry `source` + bus producer label (keeps upstreams attributable).
    pub source: &'a str,
    /// Telemetry `event` name (e.g. `overhead`, `stall_episode`).
    pub event: &'a str,
    /// Bus event name consumers subscribe to (e.g. `headroom.overhead`).
    pub bus_event: &'a str,
    /// Telemetry `status` (`degraded` / `stall`).
    pub status: &'a str,
    /// Headline latency stored as the row's `duration_ms`.
    pub value_ms: f64,
    /// Compact JSON stored in the durable telemetry row's `detail`.
    pub detail: Value,
    /// The (best-effort) bus event payload.
    pub data: Value,
}

/// Write a durable telemetry row, then emit a best-effort bus event.
pub fn emit_durable_first(args: EmitArgs<'_>) {
    // 1. Durable first (SQLite). `record_loud` logs on failure (S6: no quiet failures).
    record_loud(&TelemetryEvent {
        source: args.source.to_string(),
        event: args.event.to_string(),
        status: args.status.to_string(),
        duration_ms: Some(args.value_ms.round() as i64),
        exit_code: None,
        detail: Some(args.detail.to_string()),
    });

    // 2. Bus best-effort. A drop here is acceptable — the row above is durable.
    if let Err(e) = crate::ops::emit(args.bus_event, args.data, Some(args.source)) {
        eprintln!("hex log-tail: bus emit failed (durable row already written): {e}");
    }
}
