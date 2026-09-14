//! hex-native abstraction over iii.
//!
//! This module — together with `iii_worker.rs` — is the ONLY place in the hex
//! harness that calls `iii_sdk::`. Everywhere else in the binary uses the
//! hex-native surface exposed here (`emit`, `emit_target`). That keeps iii
//! swappable: the seam is small, named, and grep-able.

use serde_json::{json, Value};

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
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("ops::call_builtin: failed to start tokio runtime: {e}"))?;
    rt.block_on(async move {
        let url = std::env::var("III_URL").unwrap_or_else(|_| "ws://127.0.0.1:49134".to_string());
        let iii = iii_sdk::register_worker(&url, iii_sdk::InitOptions::default());
        iii.trigger(iii_sdk::protocol::TriggerRequest {
            function_id: function_id.to_string(),
            payload,
            action: None,
            timeout_ms: None,
        })
        .await
        .map_err(|e| format!("{function_id} failed (url={url}): {e}"))
    })
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
    #[ignore]
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
