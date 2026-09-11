//! Reproduce Codex's hook-trust `trusted_hash` computation.
//!
//! Codex persists a per-hook trust decision in the USER `config.toml` as
//! `[hooks.state."<key>"] trusted_hash = "sha256:<hex>"`. A discovered hook only
//! runs when its freshly computed identity hash equals that stored value. This
//! module recomputes that identity hash byte-for-byte so the hex harness can
//! decide whether a hook it is about to install is already trusted.
//!
//! Source of truth (github.com/openai/codex @ tag `rust-v0.153.4`, which
//! dereferences to commit `3d2ee51ca2d5db578f328aa75e20aa22c0197c9a`):
//!   - codex-rs/hooks/src/engine/discovery.rs  -> `NormalizedHookIdentity`, `hook_hash`
//!   - codex-rs/config/src/fingerprint.rs      -> `version_for_toml`, `canonical_json`
//!   - codex-rs/config/src/hook_config.rs      -> `MatcherGroup`, `HookHandlerConfig`
//!   - codex-rs/hooks/src/lib.rs               -> `hook_event_key_label`, `hook_key`
//!
//! The pipeline is identical to Codex:
//!   1. Build the identity struct `{ event_name, <flattened matcher + hooks> }`.
//!   2. `toml::Value::try_from(identity)` -> a `toml::Value`. TOML has no null,
//!      so its table serializer catches `Error::UnsupportedNone` and DROPS every
//!      `None` field (this also holds through `#[serde(flatten)]`). That is why
//!      `matcher`/`commandWindows`/`statusMessage`/`additionalContextLimit`
//!      vanish when unset instead of serializing as JSON `null`.
//!   3. `serde_json::to_value(&toml_value)` -> a `serde_json::Value`.
//!   4. `canonical_json` recursively sorts every object's keys (arrays keep order).
//!   5. `serde_json::to_vec` (compact, no whitespace) -> bytes -> SHA-256.
//!   6. Format as `"sha256:<lowercase hex>"`.
//!
//! Dependency note: codex-config resolves `toml` 0.9.11 while this harness pins
//! `toml` 0.8. For the scalar/table/array `Value` variants a hook identity uses
//! (string, integer, bool, array, table) the `Serialize` output is identical
//! across those versions, so the hash matches. That residual risk is retired by
//! the `#[ignore]` live comparison test below, which checks a hash Codex itself
//! wrote. `event_name` must be one of the twelve hook events in EITHER
//! CamelCase (`SessionStart`) or the snake_case key label (`session_start`);
//! both map to the same canonical label so callers cannot silently pick the
//! wrong form.

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use sha2::Digest;
use sha2::Sha256;

/// Replica of `codex-rs/config/src/hook_config.rs::MatcherGroup`. Serde
/// attributes are copied verbatim so the serialized shape matches Codex.
/// `matcher` deliberately has NO `skip_serializing_if`: the TOML layer is what
/// drops it when `None`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MatcherGroup {
    #[serde(default)]
    matcher: Option<String>,
    #[serde(default)]
    hooks: Vec<HookHandlerConfig>,
}

/// Replica of `codex-rs/config/src/hook_config.rs::HookHandlerConfig`.
/// Internally tagged by `type`; field renames match Codex exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum HookHandlerConfig {
    #[serde(rename = "command")]
    Command {
        command: String,
        #[serde(default, rename = "commandWindows", alias = "command_windows")]
        command_windows: Option<String>,
        #[serde(default, rename = "timeout")]
        timeout_sec: Option<u64>,
        #[serde(default)]
        r#async: bool,
        #[serde(default, rename = "statusMessage")]
        status_message: Option<String>,
        #[serde(
            default,
            rename = "additionalContextLimit",
            skip_serializing_if = "Option::is_none"
        )]
        additional_context_limit: Option<usize>,
    },
    #[serde(rename = "mcp_tool")]
    McpTool {
        server: String,
        tool: String,
        #[serde(default)]
        input: serde_json::Map<String, serde_json::Value>,
        #[serde(default, rename = "timeout")]
        timeout_sec: Option<u64>,
        #[serde(default, rename = "statusMessage")]
        status_message: Option<String>,
    },
    #[serde(rename = "prompt")]
    Prompt {},
    #[serde(rename = "agent")]
    Agent {},
}

/// Replica of `codex-rs/hooks/src/engine/discovery.rs::NormalizedHookIdentity`.
#[derive(Serialize)]
struct NormalizedHookIdentity<'a> {
    event_name: &'a str,
    #[serde(flatten)]
    group: MatcherGroup,
}

/// Map a hook event name (CamelCase or snake_case) to the snake_case key label
/// Codex hashes. Mirrors `codex-rs/hooks/src/lib.rs::hook_event_key_label`.
pub fn event_key_label(event_name: &str) -> Result<&'static str, String> {
    let label = match event_name {
        "PreToolUse" | "pre_tool_use" => "pre_tool_use",
        "PermissionRequest" | "permission_request" => "permission_request",
        "PostToolUse" | "post_tool_use" => "post_tool_use",
        "PreCompact" | "pre_compact" => "pre_compact",
        "PostCompact" | "post_compact" => "post_compact",
        "SessionStart" | "session_start" => "session_start",
        "SessionEnd" | "session_end" => "session_end",
        "UserPromptSubmit" | "user_prompt_submit" => "user_prompt_submit",
        "SubagentStart" | "subagent_start" => "subagent_start",
        "SubagentStop" | "subagent_stop" => "subagent_stop",
        "Stop" | "stop" => "stop",
        "Interrupt" | "interrupt" => "interrupt",
        other => return Err(format!("unknown hook event name: {other:?}")),
    };
    Ok(label)
}

/// Recursive object-key sort. Mirrors `fingerprint.rs::canonical_json`.
fn canonical_json(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(map) => {
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            for key in keys {
                if let Some(val) = map.get(&key) {
                    sorted.insert(key, canonical_json(val));
                }
            }
            JsonValue::Object(sorted)
        }
        JsonValue::Array(items) => JsonValue::Array(items.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

fn sha256_prefixed(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

/// Compute both the canonical JSON string that gets hashed and the resulting
/// `sha256:` digest. Returning the JSON lets tests assert the exact bytes, which
/// makes a mismatch debuggable instead of an opaque hex diff.
fn hook_hash_with_canonical(
    event_name: &str,
    matcher: Option<&str>,
    handlers_json: &str,
) -> Result<(String, String), String> {
    let label = event_key_label(event_name)?;
    let hooks: Vec<HookHandlerConfig> =
        serde_json::from_str(handlers_json).map_err(|e| format!("parse handlers_json: {e}"))?;
    let identity = NormalizedHookIdentity {
        event_name: label,
        group: MatcherGroup {
            matcher: matcher.map(ToOwned::to_owned),
            hooks,
        },
    };
    let toml_value =
        toml::Value::try_from(&identity).map_err(|e| format!("serialize identity to TOML: {e}"))?;
    let json =
        serde_json::to_value(&toml_value).map_err(|e| format!("convert TOML to JSON: {e}"))?;
    let canonical = canonical_json(&json);
    let bytes = serde_json::to_vec(&canonical).map_err(|e| format!("serialize canonical: {e}"))?;
    let canonical_str =
        String::from_utf8(bytes.clone()).map_err(|e| format!("canonical not UTF-8: {e}"))?;
    Ok((canonical_str, sha256_prefixed(&bytes)))
}

/// Compute Codex's hook identity `trusted_hash` for one normalized handler.
///
/// - `event_name`: hook event, CamelCase or snake_case (see `event_key_label`).
/// - `matcher`: the (already event-adjusted) matcher pattern, or `None`.
/// - `handlers_json`: a JSON array of handler objects, shaped like the `hooks`
///   array inside a `hooks.json` matcher group (each carries a `type` tag).
///
/// Returns `"sha256:<hex>"` identical to what Codex 0.153.4 computes, or an
/// `Err(String)` describing the first failure.
pub fn hook_hash(
    event_name: &str,
    matcher: Option<&str>,
    handlers_json: &str,
) -> Result<String, String> {
    hook_hash_with_canonical(event_name, matcher, handlers_json).map(|(_, hash)| hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Fixture 1 - SessionStart, no matcher, one command handler.
    //
    // Identity (after normalization): event_name = "session_start",
    //   matcher = None, hooks = [ Command { command: "/bin/echo hi",
    //   command_windows: None, timeout_sec: Some(600), async: false,
    //   status_message: None, additional_context_limit: None } ].
    //
    // toml::Value::try_from drops every None field (TOML has no null). The
    // remaining value, converted to JSON, canonicalized (keys sorted) and
    // serialized compactly, is exactly:
    //   {"event_name":"session_start","hooks":[{"async":false,"command":"/bin/echo hi","timeout":600,"type":"command"}]}
    //
    // Expected hash derived INDEPENDENTLY (not from this code) with:
    //   python3 -c "import hashlib,json; s=json.dumps({'event_name':'session_start','hooks':[{'type':'command','command':'/bin/echo hi','timeout':600,'async':False}]},separators=(',',':'),sort_keys=True); print('sha256:'+hashlib.sha256(s.encode()).hexdigest())"
    //   => sha256:3524dc80a43d23e5b183b4775038027cc6e152a7d9a8f8b0cd49c90a3410ccdf
    // -------------------------------------------------------------------------
    const F1_CANONICAL: &str = "{\"event_name\":\"session_start\",\"hooks\":[{\"async\":false,\"command\":\"/bin/echo hi\",\"timeout\":600,\"type\":\"command\"}]}";
    const F1_HASH: &str = "sha256:3524dc80a43d23e5b183b4775038027cc6e152a7d9a8f8b0cd49c90a3410ccdf";

    // -------------------------------------------------------------------------
    // Fixture 2 - PreToolUse WITH a matcher, exercising the flatten path when
    // the optional field is present.
    //
    //   {"event_name":"pre_tool_use","hooks":[{"async":false,"command":"/bin/echo tool","timeout":600,"type":"command"}],"matcher":"Bash"}
    //
    // Derived independently with:
    //   python3 -c "import hashlib,json; s=json.dumps({'event_name':'pre_tool_use','matcher':'Bash','hooks':[{'type':'command','command':'/bin/echo tool','timeout':600,'async':False}]},separators=(',',':'),sort_keys=True); print('sha256:'+hashlib.sha256(s.encode()).hexdigest())"
    //   => sha256:a0fed18c4c7a2b85d069b4b7afb578daa0c412c668f819ee9e14b894a11156cb
    // -------------------------------------------------------------------------
    const F2_CANONICAL: &str = "{\"event_name\":\"pre_tool_use\",\"hooks\":[{\"async\":false,\"command\":\"/bin/echo tool\",\"timeout\":600,\"type\":\"command\"}],\"matcher\":\"Bash\"}";
    const F2_HASH: &str = "sha256:a0fed18c4c7a2b85d069b4b7afb578daa0c412c668f819ee9e14b894a11156cb";

    #[test]
    fn session_start_command_hash_matches_hand_derivation() {
        let handlers = r#"[{"type":"command","command":"/bin/echo hi","timeout":600}]"#;
        let (canonical, hash) = hook_hash_with_canonical("session_start", None, handlers).unwrap();
        assert_eq!(
            canonical, F1_CANONICAL,
            "canonical JSON must match the by-hand derivation (None fields dropped by TOML)"
        );
        assert_eq!(hash, F1_HASH);
        // CamelCase event name must resolve to the same label and hash.
        assert_eq!(hook_hash("SessionStart", None, handlers).unwrap(), F1_HASH);
    }

    #[test]
    fn pre_tool_use_with_matcher_hash_matches_hand_derivation() {
        let handlers = r#"[{"type":"command","command":"/bin/echo tool","timeout":600}]"#;
        let (canonical, hash) =
            hook_hash_with_canonical("pre_tool_use", Some("Bash"), handlers).unwrap();
        assert_eq!(canonical, F2_CANONICAL);
        assert_eq!(hash, F2_HASH);
        assert_eq!(
            hook_hash("PreToolUse", Some("Bash"), handlers).unwrap(),
            F2_HASH
        );
    }

    #[test]
    fn absent_async_defaults_to_false_and_matches() {
        // Omitting "async" in the input must serde-default to false and produce
        // the same hash as fixture 1 (serialization always emits async).
        let handlers = r#"[{"command":"/bin/echo hi","timeout":600,"type":"command"}]"#;
        assert_eq!(hook_hash("session_start", None, handlers).unwrap(), F1_HASH);
    }

    #[test]
    fn unknown_event_name_is_error() {
        let handlers = r#"[{"type":"command","command":"x","timeout":600}]"#;
        assert!(hook_hash("NotAnEvent", None, handlers).is_err());
    }

    // -------------------------------------------------------------------------
    // Live cross-check against a hash Codex itself wrote. This is the ground
    // truth that retires the toml-version divergence risk. It is `#[ignore]`d
    // because it depends on Mike having trusted at least one JSON hook on this
    // machine (which writes `[hooks.state."<key>"] trusted_hash = ...` into
    // ~/.codex/config.toml). PENDING: run `cargo test --manifest-path
    // system/harness/Cargo.toml codex_hook_hash -- --ignored` after a hook is
    // trusted. See docs/research/2026-09-07-codex-parity-spikes.md (S0.3).
    //
    // State key format (discovery.rs line 174 + hook_key): the key is
    //   "<abs path to hooks.json>:<event_label>:<group_index>:<handler_index>".
    // Codex normalizes each handler before hashing (discovery.rs
    // append_matcher_groups): command_windows -> None, timeout defaulted
    // (600 for most events; 1 clamped to [1,3] for session_end/interrupt),
    // additional_context_limit kept only for pre_tool_use/post_tool_use/
    // session_start/user_prompt_submit/subagent_start and dropped when equal to
    // the 2500-token default, and the matcher forced to None for
    // user_prompt_submit/stop/interrupt (events/common.rs
    // matcher_pattern_for_event). We replicate that here.
    // -------------------------------------------------------------------------
    fn camel_for_label(label: &str) -> Option<&'static str> {
        Some(match label {
            "pre_tool_use" => "PreToolUse",
            "permission_request" => "PermissionRequest",
            "post_tool_use" => "PostToolUse",
            "pre_compact" => "PreCompact",
            "post_compact" => "PostCompact",
            "session_start" => "SessionStart",
            "session_end" => "SessionEnd",
            "user_prompt_submit" => "UserPromptSubmit",
            "subagent_start" => "SubagentStart",
            "subagent_stop" => "SubagentStop",
            "stop" => "Stop",
            "interrupt" => "Interrupt",
            _ => return None,
        })
    }

    fn normalize_timeout(event_label: &str, raw: Option<u64>) -> u64 {
        match event_label {
            "session_end" | "interrupt" => raw.unwrap_or(1).clamp(1, 3),
            _ => raw.unwrap_or(600).max(1),
        }
    }

    fn matcher_pattern_for_event<'a>(
        event_label: &str,
        matcher: Option<&'a str>,
    ) -> Option<&'a str> {
        match event_label {
            "user_prompt_submit" | "stop" | "interrupt" => None,
            _ => matcher,
        }
    }

    fn context_limit_event(event_label: &str) -> bool {
        matches!(
            event_label,
            "pre_tool_use"
                | "post_tool_use"
                | "session_start"
                | "user_prompt_submit"
                | "subagent_start"
        )
    }

    #[test]
    #[ignore]
    fn trusted_hash_matches_codex_written_entry() {
        let home = std::env::var("HOME").expect("HOME must be set");
        let config_path = std::path::Path::new(&home).join(".codex/config.toml");
        let text = std::fs::read_to_string(&config_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", config_path.display()));
        let root: toml::Value = toml::from_str(&text).expect("parse ~/.codex/config.toml");

        let state = root
            .get("hooks")
            .and_then(|h| h.get("state"))
            .and_then(|s| s.as_table());
        let Some(state) = state else {
            panic!(
                "no [hooks.state] table in {} - trust a JSON hook first (PENDING Mike)",
                config_path.display()
            );
        };

        let mut checked = 0usize;
        for (key, entry) in state {
            let Some(trusted) = entry.get("trusted_hash").and_then(|v| v.as_str()) else {
                continue;
            };
            // key = "<path>:<event_label>:<group_index>:<handler_index>"
            let mut parts = key.rsplitn(4, ':');
            let handler_idx: usize = match parts.next().and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => continue,
            };
            let group_idx: usize = match parts.next().and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => continue,
            };
            let Some(event_label) = parts.next() else {
                continue;
            };
            let Some(path) = parts.next() else { continue };
            let p = std::path::Path::new(path);
            let is_hooks_json = p
                .file_name()
                .and_then(|f| f.to_str())
                .map(|f| f == "hooks.json")
                .unwrap_or(false);
            if !(is_hooks_json && p.exists()) {
                continue; // only JSON hooks that still exist on disk
            }

            let camel = camel_for_label(event_label).expect("known event label");
            let hj: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
            let group = &hj["hooks"][camel][group_idx];
            let raw_matcher = group.get("matcher").and_then(|m| m.as_str());
            let handler = &group["hooks"][handler_idx];

            // Only command hooks are normalized here (what these spikes use).
            if handler.get("type").and_then(|t| t.as_str()) != Some("command") {
                continue;
            }
            let command = handler
                .get("command")
                .and_then(|c| c.as_str())
                .expect("command hook has a command");
            // Distinguish "absent" (default) from "present but wrong type"
            // (a bug in our reproduction assumptions) so the pending run gives
            // a clear signal instead of a silently-wrong hash.
            let raw_timeout = match handler.get("timeout") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => Some(
                    v.as_u64()
                        .unwrap_or_else(|| panic!("timeout for state key {key} is not a u64: {v}")),
                ),
            };
            let runs_async = match handler.get("async") {
                None | Some(serde_json::Value::Null) => false,
                Some(v) => v
                    .as_bool()
                    .unwrap_or_else(|| panic!("async for state key {key} is not a bool: {v}")),
            };

            let mut norm = serde_json::Map::new();
            norm.insert("type".into(), serde_json::json!("command"));
            norm.insert("command".into(), serde_json::json!(command));
            norm.insert(
                "timeout".into(),
                serde_json::json!(normalize_timeout(event_label, raw_timeout)),
            );
            norm.insert("async".into(), serde_json::json!(runs_async));
            if let Some(sm) = handler.get("statusMessage").and_then(|v| v.as_str()) {
                norm.insert("statusMessage".into(), serde_json::json!(sm));
            }
            if context_limit_event(event_label) {
                if let Some(limit) = handler
                    .get("additionalContextLimit")
                    .and_then(|v| v.as_u64())
                {
                    if limit != 2500 {
                        norm.insert("additionalContextLimit".into(), serde_json::json!(limit));
                    }
                }
            }

            let handlers_json =
                serde_json::to_string(&serde_json::Value::Array(vec![JsonValue::Object(norm)]))
                    .unwrap();
            let matcher = matcher_pattern_for_event(event_label, raw_matcher);
            let recomputed = hook_hash(event_label, matcher, &handlers_json).unwrap();
            assert_eq!(
                &recomputed, trusted,
                "recomputed hash != Codex trusted_hash for state key {key}"
            );
            checked += 1;
        }

        assert!(
            checked > 0,
            "no JSON-hook [hooks.state] entries with a trusted_hash were found \
             (PENDING: Mike must trust a JSON hook, then run with --ignored)"
        );
    }
}
