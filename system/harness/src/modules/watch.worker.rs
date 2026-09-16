//! `hex-watch` — the general watcher's loop (Standing Order S10: hex owns
//! the wait). Every 5 minutes: one `watch::tick::run` over every pending
//! watch. Canonical spec: `docs/hex-watch.md`.
//!
//! Runs IN-PROCESS (not `ctx.run(["hex","watch","tick"])`): the tick emits
//! `hex.watch.<outcome>` events, and emitting through this worker's `Ctx`
//! rides the harness outbox during a drain instead of a separate process
//! that has none. Returns `Err` when any watch's poll failed this tick, so
//! the telemetry row is `status=error` and `hex failures` sees a streak.
//! Per-watch outcomes (fired/failed/expired) are not errors; they are the
//! watcher doing its job and are already alerted and emitted inside the tick.
//!
//! - id `hex-watch` cron `0 */5 * * * * *`

use anyhow::anyhow;
use serde_json::Value;

use hex::watch::tick::{self, Env, RealAlerter, ShShell, Substrate};
use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Every 5 minutes, on the minute (same cadence as the Python daemon it
/// replaced; latency 0 to 5 min for every source, spec limit 1).
pub const CRON_5MIN: &str = "0 */5 * * * * *";

/// Substrate over the worker's `Ctx`: emissions divert to the outbox during
/// a drain; state reads go straight to iii.
struct CtxSubstrate<'a>(&'a Ctx);
impl Substrate for CtxSubstrate<'_> {
    fn get_event(&self, name: &str) -> std::result::Result<Option<Value>, String> {
        self.0.state().get("events", name).map_err(|e| e.to_string())
    }
    fn emit(&self, event: &str, data: Value) -> std::result::Result<(), String> {
        self.0.emit(event, data).map_err(|e| e.to_string())
    }
}

fn hex_dir() -> Result<std::path::PathBuf> {
    if let Ok(v) = std::env::var("HEX_DIR") {
        return Ok(std::path::PathBuf::from(v));
    }
    dirs::home_dir()
        .map(|h| h.join("hex"))
        .ok_or_else(|| anyhow!("hex-watch: neither HEX_DIR nor a home directory is set"))
}

fn run_tick(_e: Event, ctx: Ctx) -> Result<()> {
    let hex_dir = hex_dir()?;
    let config = hex::watch::load_config(&hex_dir).map_err(|e| anyhow!(e))?;
    let substrate = CtxSubstrate(&ctx);
    let shell = ShShell;
    let alerter = RealAlerter { hex_dir: hex_dir.clone() };
    let env = Env {
        hex_dir: hex_dir.clone(),
        config,
        substrate: &substrate,
        shell: &shell,
        alerter: &alerter,
        parent_env: tick::parent_env(),
        dry_run: false,
        log: &tick::log_stderr,
    };
    let rep = tick::run(&env, chrono::Utc::now()).map_err(|e| anyhow!(e))?;
    eprintln!(
        "hex-watch: tick pending={} fired={} failed={} expired={} poll_failures={} streak={}",
        rep.pending, rep.fired, rep.failed, rep.expired, rep.poll_failures, rep.streak
    );
    if rep.poll_failures > 0 {
        return Err(anyhow!(
            "hex-watch: {} watch(es) could not be polled this tick (streak {}); see harness log",
            rep.poll_failures,
            rep.streak
        ));
    }
    Ok(())
}

pub fn worker() -> Worker {
    Worker::new("hex-watch").on_cron_named("tick", CRON_5MIN, run_tick)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_on_the_five_minute_cron() {
        let w = worker();
        assert_eq!(w.name, "hex-watch");
        assert_eq!(w.handlers.len(), 1);
        let (name, spec, _) = &w.handlers[0];
        assert_eq!(name.as_deref(), Some("tick"));
        match spec {
            hex::worker::TriggerSpec::Cron { expression } => assert_eq!(expression, CRON_5MIN),
            other => panic!("expected a cron trigger, got {other:?}"),
        }
    }
}
