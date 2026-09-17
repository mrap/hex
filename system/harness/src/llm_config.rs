//! Per-use-case LLM configuration registry.
//!
//! See spec Sy23e65a2 (initial registry) and spec Sbe8m4886 task T66j95j4d
//! (transport + claude_settings_file additions). This module is the source of
//! truth for which model, max_tokens, base_url, api_key_env, transport, and
//! optional claude-cli settings file each LLM-backed hex use case should call.
//! Resolution order (highest wins):
//!   1. env vars HEX_LLM_MODEL_<USE_CASE_UPPER> (model only) and
//!      HEX_LLM_TRANSPORT_<USE_CASE_UPPER> (transport only)
//!      - HEX_CONSOLIDATE_MODEL is honored as an alias for consolidate_audit
//!   2. [use_cases.<name>] in $HEX_DIR/.hex/config/llm.toml
//!   3. [defaults]            in $HEX_DIR/.hex/config/llm.toml
//!   4. built-in registry defaults below
//!
//! Missing llm.toml → built-ins (zero behavior change). Malformed llm.toml →
//! LOUD error returned from `resolve()`. Unknown [use_cases.*] table names →
//! warning to stderr but tolerated (per spec). Unknown `transport` values are a
//! hard error from `resolve()` — see S6 "no quiet failures".

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLlm {
    pub model: String,
    pub max_tokens: u32,
    pub base_url: String,
    pub api_key_env: String,
    /// Optional cap on estimated INPUT tokens per call. Used by distill to
    /// slice oversize spans. None means "no input cap configured".
    pub max_input_tokens: Option<u32>,
    /// Transport seam for this use case. Always one of:
    ///   * "http" — call an OpenAI-compatible HTTP endpoint (default,
    ///     matches every deployment from before spec Sbe8m4886).
    ///   * "claude-cli" — shell out to a headless `claude -p` process,
    ///     authenticated via the macOS login keychain.
    ///
    /// Unknown values cause `resolve()` to return a loud error.
    pub transport: String,
    /// Optional path to a `--settings` JSON file for the `claude-cli`
    /// transport. None means "use the built-in inline default settings".
    /// Meaningful only when `transport == "claude-cli"`.
    pub claude_settings_file: Option<String>,
}

/// Transport identifiers accepted in llm.toml and HEX_LLM_TRANSPORT_*.
pub const TRANSPORT_HTTP: &str = "http";
pub const TRANSPORT_CLAUDE_CLI: &str = "claude-cli";

fn known_transports() -> &'static [&'static str] {
    &[TRANSPORT_HTTP, TRANSPORT_CLAUDE_CLI]
}

/// Validate a transport string from either llm.toml or env. `source` is a
/// short tag included in the error message so the operator can find the
/// offending knob fast (S6 — no quiet failures).
fn validate_transport(value: &str, source: &str) -> Result<()> {
    if known_transports().contains(&value) {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "invalid transport `{value}` in {source} — known transports are: {}",
            known_transports().join(", ")
        ))
    }
}

/// Built-in default for a known use case. These are the values that were
/// hardcoded at call sites before this module existed.
#[derive(Debug, Clone)]
struct BuiltIn {
    model: &'static str,
    max_tokens: u32,
    max_input_tokens: Option<u32>,
}

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEFAULT_API_KEY_ENV: &str = "OPENROUTER_API_KEY";

fn builtin(use_case: &str) -> Option<BuiltIn> {
    match use_case {
        "memory_extract" => Some(BuiltIn {
            model: "anthropic/claude-sonnet-5",
            max_tokens: 16384,
            // 48k input cap leaves comfortable headroom under the model's
            // input limit alongside the 16384-token output cap. Anything
            // larger gets sliced by `memory::distill::cap::cap_span`.
            max_input_tokens: Some(48_000),
        }),
        "memory_judge" => Some(BuiltIn {
            model: "anthropic/claude-sonnet-5",
            max_tokens: 256,
            max_input_tokens: None,
        }),
        // consolidate_audit runs on anthropic/claude-sonnet-5 via OpenRouter,
        // whose hidden reasoning tokens count against max_tokens. At the previous
        // 4k cap the entire output budget was consumed by reasoning, so the audit
        // returned EMPTY content while still billing (~$0.21/night, observed
        // 2026-08-16..19). Raise to 16384 so the response has real content
        // headroom above the reasoning spend.
        "consolidate_audit" => Some(BuiltIn {
            model: "anthropic/claude-sonnet-5",
            max_tokens: 16384,
            max_input_tokens: None,
        }),
        "health_check" => Some(BuiltIn {
            model: "anthropic/claude-haiku-4.5",
            max_tokens: 64,
            max_input_tokens: None,
        }),
        _ => None,
    }
}

/// All known use cases — used by doctor to list resolved configs.
pub fn known_use_cases() -> &'static [&'static str] {
    &[
        "memory_extract",
        "memory_judge",
        "consolidate_audit",
        "health_check",
    ]
}

#[derive(Debug, Deserialize, Default)]
struct LlmTomlFile {
    #[serde(default)]
    defaults: Option<SectionFields>,
    #[serde(default)]
    use_cases: HashMap<String, SectionFields>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
struct SectionFields {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    max_input_tokens: Option<u32>,
    /// Optional transport override. See ResolvedLlm::transport for legal
    /// values. Validated at resolve() time, not at deserialize, so the error
    /// message can name [defaults] vs [use_cases.X] vs env.
    #[serde(default)]
    transport: Option<String>,
    /// Optional path to a `--settings` JSON file passed to claude-cli.
    /// Inert for the http transport.
    #[serde(default)]
    claude_settings_file: Option<String>,
}

fn config_path() -> PathBuf {
    let hex_dir = std::env::var("HEX_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("hex")
        });
    hex_dir.join(".hex/config/llm.toml")
}

fn load_file() -> Result<Option<LlmTomlFile>> {
    let path = config_path();
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("reading llm.toml at {}", path.display()))?;
    let parsed: LlmTomlFile =
        toml::from_str(&body).with_context(|| format!("parsing llm.toml at {}", path.display()))?;

    // Warn (do not fail) on unknown use-case table names.
    for name in parsed.use_cases.keys() {
        if !known_use_cases().contains(&name.as_str()) {
            eprintln!(
                "warning: llm.toml contains unknown [use_cases.{}] — \
                 known use cases are: {}",
                name,
                known_use_cases().join(", ")
            );
        }
    }
    Ok(Some(parsed))
}

fn env_model_for(use_case: &str) -> Option<String> {
    let key = format!("HEX_LLM_MODEL_{}", use_case.to_uppercase());
    if let Ok(v) = std::env::var(&key) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    // Back-compat alias.
    if use_case == "consolidate_audit" {
        if let Ok(v) = std::env::var("HEX_CONSOLIDATE_MODEL") {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Read HEX_LLM_TRANSPORT_<USE_CASE_UPPER> if set and non-empty. Validation of
/// the value happens later in `resolve()` so the error can be uniform with
/// file-sourced misconfiguration.
fn env_transport_for(use_case: &str) -> Option<String> {
    let key = format!("HEX_LLM_TRANSPORT_{}", use_case.to_uppercase());
    match std::env::var(&key) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

pub fn resolve(use_case: &str) -> Result<ResolvedLlm> {
    let bi = builtin(use_case).with_context(|| {
        format!(
            "unknown LLM use case `{}` — known: {}",
            use_case,
            known_use_cases().join(", ")
        )
    })?;

    let file = load_file()?;

    let defaults = file
        .as_ref()
        .and_then(|f| f.defaults.clone())
        .unwrap_or_default();
    let uc = file
        .as_ref()
        .and_then(|f| f.use_cases.get(use_case).cloned())
        .unwrap_or_default();

    // Field resolution: env (model only) > use_case > defaults > built-in.
    let model = env_model_for(use_case)
        .or(uc.model)
        .or(defaults.model.clone())
        .unwrap_or_else(|| bi.model.to_string());

    let max_tokens = uc
        .max_tokens
        .or(defaults.max_tokens)
        .unwrap_or(bi.max_tokens);

    let base_url = uc
        .base_url
        .or(defaults.base_url.clone())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    let api_key_env = uc
        .api_key_env
        .or(defaults.api_key_env)
        .unwrap_or_else(|| DEFAULT_API_KEY_ENV.to_string());

    let max_input_tokens = uc
        .max_input_tokens
        .or(defaults.max_input_tokens)
        .or(bi.max_input_tokens);

    // Transport resolution. Order: env (HEX_LLM_TRANSPORT_<UC_UPPER>) >
    // [use_cases.<uc>].transport > [defaults].transport > "http".
    // Each layer is validated as it's chosen so the error message names the
    // actual source of the bad value.
    let transport = if let Some(env_val) = env_transport_for(use_case) {
        validate_transport(
            &env_val,
            &format!("env HEX_LLM_TRANSPORT_{}", use_case.to_uppercase()),
        )?;
        env_val
    } else if let Some(uc_val) = uc.transport.clone() {
        validate_transport(&uc_val, &format!("[use_cases.{use_case}].transport"))?;
        uc_val
    } else if let Some(def_val) = defaults.transport.clone() {
        validate_transport(&def_val, "[defaults].transport")?;
        def_val
    } else {
        TRANSPORT_HTTP.to_string()
    };

    // claude_settings_file flows through use_case > defaults. It's only
    // meaningful for the claude-cli transport, but we don't gate on transport
    // here — the seam may set it at [defaults] alongside a use-case-level
    // transport override.
    let claude_settings_file = uc.claude_settings_file.or(defaults.claude_settings_file);

    Ok(ResolvedLlm {
        model,
        max_tokens,
        base_url,
        api_key_env,
        max_input_tokens,
        transport,
        claude_settings_file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Tests mutate process env vars (HEX_DIR, HEX_LLM_MODEL_*,
    // HEX_CONSOLIDATE_MODEL) and must run serially — on the crate's ONE
    // shared HEX_DIR lock (telemetry::test_support), not a module-local
    // mutex: a local lock still races every other HEX_DIR-mutating test
    // in the lib target (observed: failures::missed_tests flaking when
    // these tests swapped HEX_DIR mid-run).
    use crate::telemetry::test_support::ENV_LOCK;

    const ENV_KEYS: &[&str] = &[
        "HEX_DIR",
        "HEX_LLM_MODEL_MEMORY_EXTRACT",
        "HEX_LLM_MODEL_MEMORY_JUDGE",
        "HEX_LLM_MODEL_CONSOLIDATE_AUDIT",
        "HEX_LLM_MODEL_HEALTH_CHECK",
        "HEX_CONSOLIDATE_MODEL",
        "HEX_LLM_TRANSPORT_MEMORY_EXTRACT",
        "HEX_LLM_TRANSPORT_MEMORY_JUDGE",
        "HEX_LLM_TRANSPORT_CONSOLIDATE_AUDIT",
        "HEX_LLM_TRANSPORT_HEALTH_CHECK",
    ];

    fn clear_env() {
        for k in ENV_KEYS {
            std::env::remove_var(k);
        }
    }

    fn setup_hex_dir() -> tempfile::TempDir {
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("HEX_DIR", td.path());
        std::fs::create_dir_all(td.path().join(".hex/config")).unwrap();
        td
    }

    fn write_llm_toml(hex_dir: &std::path::Path, body: &str) {
        let p = hex_dir.join(".hex/config/llm.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn missing_file_uses_builtins() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let _td = setup_hex_dir();

        let r = resolve("memory_extract").expect("resolve ok");
        assert_eq!(r.model, "anthropic/claude-sonnet-5");
        assert_eq!(r.max_tokens, 16384);

        let r = resolve("memory_judge").expect("resolve ok");
        assert_eq!(r.model, "anthropic/claude-sonnet-5");
        // Red for U4/KTD6: 2026-09-16 distill::judge-error incident (3 rows,
        // finish_reason: length) — hidden reasoning tokens ate the whole 256
        // cap before the judge's JSON decision could be written. Raise to
        // 4096, mirroring consolidate_audit's fix for the same failure mode.
        assert_eq!(r.max_tokens, 4096);

        let r = resolve("consolidate_audit").expect("resolve ok");
        assert_eq!(r.model, "anthropic/claude-sonnet-5");
        assert_eq!(r.max_tokens, 16384);

        let r = resolve("health_check").expect("resolve ok");
        assert_eq!(r.model, "anthropic/claude-haiku-4.5");
    }

    #[test]
    fn use_case_overrides_defaults() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let td = setup_hex_dir();
        write_llm_toml(
            td.path(),
            r#"
[defaults]
model = "defaults/model"

[use_cases.memory_extract]
model = "uc/extract-model"
max_tokens = 999
"#,
        );

        let r = resolve("memory_extract").expect("resolve ok");
        assert_eq!(r.model, "uc/extract-model");
        assert_eq!(r.max_tokens, 999);

        // memory_judge has no use_cases entry → inherits [defaults] model,
        // but max_tokens falls back to the built-in (256).
        let r = resolve("memory_judge").expect("resolve ok");
        assert_eq!(r.model, "defaults/model");
        assert_eq!(r.max_tokens, 256);
    }

    #[test]
    fn env_var_overrides_file() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let td = setup_hex_dir();
        write_llm_toml(
            td.path(),
            r#"
[use_cases.memory_extract]
model = "file/model"
"#,
        );
        std::env::set_var("HEX_LLM_MODEL_MEMORY_EXTRACT", "env/model");

        let r = resolve("memory_extract").expect("resolve ok");
        assert_eq!(r.model, "env/model");
    }

    #[test]
    fn hex_consolidate_model_alias_still_works() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let _td = setup_hex_dir();
        std::env::set_var("HEX_CONSOLIDATE_MODEL", "alias/consolidate");

        let r = resolve("consolidate_audit").expect("resolve ok");
        assert_eq!(r.model, "alias/consolidate");
    }

    #[test]
    fn malformed_toml_is_hard_error() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let td = setup_hex_dir();
        write_llm_toml(td.path(), "this is = = not valid toml [[[");

        let err = resolve("memory_extract")
            .expect_err("malformed TOML must be a loud error, not a silent fallback");
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("llm.toml") || msg.to_lowercase().contains("toml"),
            "error should mention llm.toml / TOML, got: {msg}"
        );
    }
}
