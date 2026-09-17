//! hex-native abstraction over iii.
//!
//! This module — together with `iii_worker.rs` — is the ONLY place in the hex
//! harness that calls `iii_sdk::`. Everywhere else in the binary uses the
//! hex-native surface exposed here (`emit`, `emit_target`). That keeps iii
//! swappable: the seam is small, named, and grep-able.

use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

/// Pure description of the state write a given event maps to.
///
/// `emit(event, data)` connects to the engine and writes
/// `state::set { scope, key, value }`. State-trigger workers subscribed to
/// that scope fire as a result. Keeping the mapping in a pure struct lets
/// us unit-test the addressing without a live engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitTarget {
    pub scope: String,
    pub key: String,
    pub value: Value,
}

/// Pure mapping: event name + producer + ts + data → the (scope, key, value)
/// state write, where `value` is the hex-native 4-field envelope
/// `{event, producer, ts, data}`.
///
/// All hex events land under the `events` scope (one state surface that
/// iii triggers subscribe to). The event name is used verbatim as
/// the key so distinct events fire distinct triggers.
///
/// PURE: no clock, no env reads — the caller supplies `ts` and `producer`.
/// This is what makes the function unit-testable and deterministic.
pub fn emit_target(event: &str, producer: &str, ts: &str, data: &Value) -> EmitTarget {
    EmitTarget {
        scope: "events".to_string(),
        key: event.to_string(),
        value: json!({
            "event": event,
            "producer": producer,
            "ts": ts,
            "data": data.clone(),
        }),
    }
}

/// Resolve the producer attribution for an emit call.
///
/// Precedence: explicit `--producer` flag > `HEX_PRODUCER` env var > literal `"cli"`.
pub fn resolve_producer(explicit: Option<&str>) -> String {
    if let Some(s) = explicit {
        return s.to_string();
    }
    match std::env::var("HEX_PRODUCER") {
        Ok(v) if !v.is_empty() => v,
        _ => "cli".to_string(),
    }
}

/// Pure: the `{scope,key[,value]}` payload for a `state::*` builtin call.
/// `value=Some` (set) includes the value; `value=None` (get/delete) omits it.
pub fn state_payload(scope: &str, key: &str, value: Option<&Value>) -> Value {
    match value {
        Some(v) => json!({ "scope": scope, "key": key, "value": v.clone() }),
        None => json!({ "scope": scope, "key": key }),
    }
}

/// Connect to the iii engine and invoke a builtin `function_id` with `payload`.
/// Returns the engine's result Value. LOUD on failure (S6). The ONE place
/// state/* builtins (and emit) cross into `iii_sdk`.
fn call_builtin(function_id: &str, payload: Value) -> Result<Value, String> {
    call_builtin_with_timeout(function_id, payload, None)
}

/// Upper bound on how long a `call_builtin` caller waits for the SDK client
/// to shut down after the trigger completes. In the normal paths (engine
/// reachable, or connect refused) the connection thread exits within ~2s;
/// the budget only fires when the engine accepts TCP but never finishes the
/// WebSocket handshake, where the SDK's `connect_async` has no timeout.
const SHUTDOWN_JOIN_BUDGET: Duration = Duration::from_secs(5);

/// Process-wide shared client `serve` owns, installed once by
/// [`install_shared_client`]. Not a tuple with a separately-tracked url:
/// `III::address()` already returns the exact url the client connects to,
/// so reading it back from the client at call time can never drift out of
/// sync with what actually answers.
static SHARED_CLIENT: OnceLock<iii_sdk::III> = OnceLock::new();

/// Install the process-wide shared client that `serve` owns (KTD8: `serve`
/// already creates one long-lived `iii_sdk::III` client at startup via
/// `worker::runtime::connect_engine_client`; this seam lets
/// `call_builtin_with_timeout_and_budget` reuse it instead of opening a new
/// client per call, which is the per-call-client residual this unit closes).
///
/// A second install is a loud no-op (S6): the first client installed keeps
/// serving every call, and the caller of a second install is told on stderr
/// instead of silently losing its client.
pub fn install_shared_client(iii: iii_sdk::III) {
    if SHARED_CLIENT.set(iii).is_err() {
        eprintln!(
            "ops::install_shared_client: WARN a shared client is already installed (url={}); \
             keeping the first one and ignoring this install",
            SHARED_CLIENT
                .get()
                .map(|c| c.address())
                .unwrap_or("<unknown>")
        );
    }
}

/// True once [`install_shared_client`] has installed the process-wide shared
/// client.
pub fn shared_client_installed() -> bool {
    SHARED_CLIENT.get().is_some()
}

/// `call_builtin` with an explicit invocation timeout (`None` = SDK default,
/// 30s). `pub` so the `fd_limits` integration binary can drive the
/// unreachable-engine path quickly; production callers use `state_*`/`emit`.
pub fn call_builtin_with_timeout(
    function_id: &str,
    payload: Value,
    timeout_ms: Option<u64>,
) -> Result<Value, String> {
    call_builtin_with_timeout_and_budget(function_id, payload, timeout_ms, SHUTDOWN_JOIN_BUDGET)
}

/// `call_builtin_with_timeout` with an explicit shutdown-join budget. `pub`
/// only so the `fd_limits` and `ops_shared_client` integration binaries can
/// prove the bound with a sub-second budget; production always uses
/// [`SHUTDOWN_JOIN_BUDGET`].
///
/// Two paths (KTD8):
///
/// - **Inside `hex harness serve`.** `worker::runtime::connect_engine_client`
///   already installed `serve`'s own long-lived `iii_sdk::III` client via
///   [`install_shared_client`] before any handler could run. When
///   [`SHARED_CLIENT`] is set, this function reuses it: a fresh
///   `Builder::new_current_thread().enable_all()` runtime (the SDK's
///   `trigger` awaits `tokio::time::timeout`, which needs a runtime with the
///   timer driver — any runtime works, not only the client's own
///   connection-thread runtime) just `block_on`s `shared.trigger(...)` and
///   returns. No `register_worker`, no `shutdown()`: the shared client and
///   its connection thread outlive this call, same as they outlive every
///   other call `serve` makes. `shutdown_budget` is unused on this path —
///   there is nothing to shut down per call.
/// - **Every other caller** (a `hex` CLI subprocess, or a test that never
///   installs a shared client) takes the ORIGINAL per-call path: the client
///   is torn down on EVERY return path. `iii_sdk::register_worker` spawns a
///   dedicated OS thread (`iii-connection`) with its own tokio runtime and a
///   reconnect loop, and `III` has no `Drop` — dropping the handle (or the
///   caller's runtime) leaves that thread, its kqueue, and both ends of the
///   loopback socket alive. Inside the long-lived `hex harness serve`
///   process that was a leak of ~6 fds per call (2026-09-17: hex-watch polls
///   two event watches per tick → 256-fd soft limit in ~100 min → EMFILE
///   storm across every worker; incident `failures-storm-...Too-many-open-
///   files`) — closed for the serve path by the shared-client branch above;
///   a CLI subprocess still opens one client per call, but it relies on
///   process exit rather than a long-lived loop, so the residual there is
///   wasted setup cost, not a leak. `shutdown()` flips `running=false` and
///   joins the thread; the reconnect loop honors that within ~2s even when
///   connect keeps failing.
pub fn call_builtin_with_timeout_and_budget(
    function_id: &str,
    payload: Value,
    timeout_ms: Option<u64>,
    shutdown_budget: Duration,
) -> Result<Value, String> {
    if let Some(shared) = SHARED_CLIENT.get() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("ops::call_builtin: failed to start tokio runtime: {e}"))?;
        let result = rt.block_on(shared.trigger(iii_sdk::protocol::TriggerRequest {
            function_id: function_id.to_string(),
            payload,
            action: None,
            timeout_ms,
        }));
        return result.map_err(|e| format!("{function_id} failed (url={}): {e}", shared.address()));
    }

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("ops::call_builtin: failed to start tokio runtime: {e}"))?;
    let url = std::env::var("III_URL").unwrap_or_else(|_| "ws://127.0.0.1:49134".to_string());
    let iii = iii_sdk::register_worker(&url, iii_sdk::InitOptions::default());
    let result = rt.block_on(iii.trigger(iii_sdk::protocol::TriggerRequest {
        function_id: function_id.to_string(),
        payload,
        action: None,
        timeout_ms,
    }));
    // Release the connection thread + sockets before returning, success or not.
    shutdown_within(&rt, &iii, shutdown_budget, &url);
    result.map_err(|e| format!("{function_id} failed (url={url}): {e}"))
}

/// Shut the SDK client down, waiting at most `budget` for its connection
/// thread to be joined. `III::shutdown()` joins unconditionally, and the
/// SDK's reconnect loop awaits `connect_async` with no handshake timeout, so
/// against an engine that accepts TCP but never answers the upgrade the join
/// would block the caller forever (review finding on the v0.53.2 fix). The
/// join therefore runs on a helper thread; on overrun the caller logs LOUD
/// (S6) and returns, and the helper finishes whenever the connect resolves.
fn shutdown_within(rt: &tokio::runtime::Runtime, iii: &iii_sdk::III, budget: Duration, url: &str) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let client = iii.clone();
    let spawned = std::thread::Builder::new()
        .name("iii-shutdown".into())
        .spawn(move || {
            client.shutdown();
            let _ = tx.send(());
        });
    if let Err(e) = spawned {
        // No helper thread means no bounded join is possible; signal the
        // shutdown without joining (never blocks) so the caller still returns.
        let detail = format!("could not spawn iii-shutdown thread ({e}); shutdown signalled without join (url={url})");
        eprintln!("ops::call_builtin: WARN {detail}");
        record_shutdown_failure(detail);
        rt.block_on(iii.shutdown_async());
        return;
    }
    if rx.recv_timeout(budget).is_err() {
        let detail = format!(
            "iii client shutdown exceeded {budget:?} (url={url}); connection thread detached (it exits when the engine's connect resolves)"
        );
        eprintln!("ops::call_builtin: WARN {detail}");
        record_shutdown_failure(detail);
    }
}

/// A teardown that could not be bounded is loud on both S6 channels: stderr
/// (above) and a telemetry error row, so a stalling engine shows up in
/// `hex failures` before it exhausts threads or fds again.
fn record_shutdown_failure(detail: String) {
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "harness".into(),
        event: "iii::shutdown".into(),
        status: "error".into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(detail),
    });
}

/// Write a value into iii state. LOUD on failure (S6).
pub fn state_set(scope: &str, key: &str, value: &Value) -> Result<(), String> {
    call_builtin("state::set", state_payload(scope, key, Some(value))).map(|_| ())
}

/// Read a value from iii state. `Ok(None)` when the engine returns JSON null —
/// normally "key absent". NOTE: a key whose stored value is literally `null` is
/// also surfaced as `None` (the engine returns null for both), so absent and
/// stored-null are indistinguishable here. LOUD on transport failure (S6).
pub fn state_get(scope: &str, key: &str) -> Result<Option<Value>, String> {
    let v = call_builtin("state::get", state_payload(scope, key, None))?;
    Ok(if v.is_null() { None } else { Some(v) })
}

/// Delete a key from iii state. LOUD on failure (S6).
pub fn state_delete(scope: &str, key: &str) -> Result<(), String> {
    call_builtin("state::delete", state_payload(scope, key, None)).map(|_| ())
}

/// Connect to the iii engine (`III_URL`, default `ws://127.0.0.1:49134`) and
/// write the event into iii state via `state::set`. State-triggered workers
/// subscribed to the `events` scope fire as a result.
///
/// LOUD on failure (S6): any error is returned as a descriptive `Err(String)`.
/// Never silently swallowed.
pub fn emit(event: &str, data: Value, producer: Option<&str>) -> Result<(), String> {
    let producer = resolve_producer(producer);
    let ts = chrono::Utc::now().to_rfc3339();
    let target = emit_target(event, &producer, &ts, &data);

    call_builtin(
        "state::set",
        state_payload(&target.scope, &target.key, Some(&target.value)),
    )
    .map(|_| ())
    .map_err(|e| format!("hex triggers emit: {e} (event '{event}')"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_target_uses_events_scope_and_event_name_as_key() {
        let data = json!({"a": 1});
        let t = emit_target("foo.bar", "cli", "2026-06-04T00:00:00Z", &data);
        assert_eq!(t.scope, "events");
        assert_eq!(t.key, "foo.bar");
    }

    /// Moved from the removed tests/ops_emit_target_test.rs: pins the full
    /// EmitTarget shape (scope, key, envelope) for a realistic event, not
    /// just its individual fields.
    #[test]
    fn emit_target_maps_event_to_state_scope_key_envelope() {
        let data = json!({"spec_id": "Skt0r3dbg", "status": "ok"});
        let target = emit_target("boi.spec.complete", "cli", "2026-06-04T00:00:00Z", &data);

        assert_eq!(
            target,
            EmitTarget {
                scope: "events".to_string(),
                key: "boi.spec.complete".to_string(),
                value: json!({
                    "event": "boi.spec.complete",
                    "producer": "cli",
                    "ts": "2026-06-04T00:00:00Z",
                    "data": data,
                }),
            }
        );
    }

    #[test]
    fn emit_target_is_pure() {
        let data = json!({"k": "v"});
        let a = emit_target("e", "cli", "2026-06-04T00:00:00Z", &data);
        let b = emit_target("e", "cli", "2026-06-04T00:00:00Z", &data);
        assert_eq!(a, b);
    }

    #[test]
    fn emit_target_builds_four_field_envelope() {
        let data = json!({"x": 42});
        let t = emit_target("evt.name", "producer-a", "2026-06-04T12:34:56Z", &data);
        let obj = t.value.as_object().expect("value must be a JSON object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(|s| s.as_str()).collect();
        let expected: std::collections::BTreeSet<&str> =
            ["event", "producer", "ts", "data"].into_iter().collect();
        assert_eq!(keys, expected, "envelope must have exactly these 4 keys");
        assert_eq!(obj["event"], json!("evt.name"));
        assert_eq!(obj["producer"], json!("producer-a"));
        assert_eq!(obj["ts"], json!("2026-06-04T12:34:56Z"));
        assert_eq!(
            obj["data"], data,
            "data nested under `data` (not flattened)"
        );
    }

    #[test]
    fn state_payload_set_includes_value() {
        let v = json!({"n": 1});
        let p = state_payload("trading", "mids", Some(&v));
        assert_eq!(
            p,
            json!({"scope": "trading", "key": "mids", "value": {"n": 1}})
        );
    }

    #[test]
    fn state_payload_get_omits_value() {
        let p = state_payload("trading", "mids", None);
        assert_eq!(p, json!({"scope": "trading", "key": "mids"}));
    }

    #[test]
    fn resolve_producer_precedence_explicit_beats_env_beats_default() {
        let _guard = crate::telemetry::test_support::lock_env();

        // Save and clear
        let prev = std::env::var("HEX_PRODUCER").ok();
        std::env::remove_var("HEX_PRODUCER");

        // Default: "cli"
        assert_eq!(resolve_producer(None), "cli");

        // HEX_PRODUCER env beats default
        std::env::set_var("HEX_PRODUCER", "from-env");
        assert_eq!(resolve_producer(None), "from-env");

        // Explicit beats env
        assert_eq!(resolve_producer(Some("explicit-one")), "explicit-one");

        // Restore
        match prev {
            Some(v) => std::env::set_var("HEX_PRODUCER", v),
            None => std::env::remove_var("HEX_PRODUCER"),
        }
    }

    /// Live round-trip against a running engine. Run: `cargo test -p hex-harness
    /// -- --ignored state_roundtrip_live`.
    #[test]
    #[ignore = "needs a live iii engine reachable for state_set/state_get/state_delete; \
                host-only, run explicitly"]
    fn state_roundtrip_live() {
        let scope = "hex-test";
        let key = "ops-roundtrip";
        state_set(scope, key, &json!({"ok": true})).expect("set");
        assert_eq!(
            state_get(scope, key).expect("get"),
            Some(json!({"ok": true}))
        );
        state_delete(scope, key).expect("delete");
        assert_eq!(state_get(scope, key).expect("get-after-delete"), None);
    }
}
