use std::env;
use std::path::PathBuf;

#[derive(Debug)]
pub enum ProviderError {
    /// Configuration problem (no key, no config file). Recoverable: ops fix the config.
    Deferred(String),
    /// Network or API error. Same handling — defer.
    Upstream(String),
    /// The provider cut the response off (`finish_reason == "length"`) before
    /// a complete answer was produced, distinct from a generic parse/upstream
    /// failure so callers (e.g. distill::judge) can name the incident
    /// precisely instead of reporting a JSON parse error on truncated JSON.
    Truncated(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::Deferred(msg) => write!(f, "provider DEFERRED: {msg}"),
            ProviderError::Upstream(msg) => write!(f, "provider upstream error: {msg}"),
            ProviderError::Truncated(msg) => write!(f, "provider truncated: {msg}"),
        }
    }
}

impl std::error::Error for ProviderError {}

pub fn hex_root() -> PathBuf {
    env::var("HEX_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|h| PathBuf::from(h).join("hex"))
            .unwrap_or_else(|_| PathBuf::from("/tmp/hex"))
    })
}

pub fn load_openrouter_key() -> Option<String> {
    load_api_key("OPENROUTER_API_KEY")
}

/// Load an API key for the given env var name. When the requested env var is
/// the default `OPENROUTER_API_KEY`, also falls back to the
/// `$HEX_DIR/.hex/secrets/openrouter.env` file (back-compat with the original
/// behavior). For any other api_key_env, only the env var is consulted.
pub fn load_api_key(api_key_env: &str) -> Option<String> {
    if let Ok(k) = env::var(api_key_env) {
        if !k.is_empty() {
            return Some(k);
        }
    }
    if api_key_env == "OPENROUTER_API_KEY" {
        // Only check the secrets file if HEX_DIR is explicitly set — no hardcoded fallback
        // so that test/verify environments without HEX_DIR correctly return None (Deferred).
        if let Ok(hex_dir) = env::var("HEX_DIR") {
            let env_file = PathBuf::from(&hex_dir).join(".hex/secrets/openrouter.env");
            if let Ok(content) = std::fs::read_to_string(env_file) {
                for line in content.lines() {
                    if let Some((k, v)) = line.split_once('=') {
                        // Tolerate shell-sourceable files (`export KEY=...`) —
                        // a live box silently deferred for days on this prefix.
                        let key = k.trim();
                        let key = key.strip_prefix("export ").unwrap_or(key).trim();
                        if key == "OPENROUTER_API_KEY" {
                            let val = v.trim().trim_matches('"').to_string();
                            if !val.is_empty() {
                                return Some(val);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Build the JSON request body for an OpenRouter chat completion request.
/// When the model starts with "anthropic/", forces Anthropic-direct routing
/// to avoid AWS Bedrock/GCP Vertex content filter rejections on personal content.
pub fn build_request_body(prompt: &str, model: &str, max_tokens: u32) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
    });
    if model.starts_with("anthropic/") {
        body["provider"] = serde_json::json!({
            "order": ["anthropic"],
            "allow_fallbacks": false,
        });
    }
    body
}

pub fn generate(prompt: &str, model: &str, max_tokens: u32) -> Result<String, ProviderError> {
    generate_inner(
        "generate",
        prompt,
        model,
        max_tokens,
        "https://openrouter.ai/api/v1/chat/completions",
        "OPENROUTER_API_KEY",
    )
}

/// Resolve LLM config for `use_case` (via `llm_config::resolve`) and call the
/// underlying transport. This is the preferred entry point for new code —
/// honors per-use-case overrides for model, max_tokens, base_url,
/// api_key_env, and (since spec Sbe8m4886) the `transport` seam from
/// `$HEX_DIR/.hex/config/llm.toml`.
///
/// Transport branch: when `transport == "claude-cli"`, shells out to a
/// headless `claude -p` process (see `memory::claude_cli`); otherwise the
/// default HTTP path via `generate_inner`.
pub fn generate_for(use_case: &str, prompt: &str) -> Result<String, ProviderError> {
    let cfg = crate::llm_config::resolve(use_case)
        .map_err(|e| ProviderError::Deferred(format!("llm_config::resolve({use_case}): {e:#}")))?;
    if cfg.transport == crate::llm_config::TRANSPORT_CLAUDE_CLI {
        return crate::memory::claude_cli::generate(
            use_case,
            prompt,
            &cfg.model,
            cfg.max_tokens,
            cfg.claude_settings_file.as_deref(),
        );
    }
    generate_inner(
        use_case,
        prompt,
        &cfg.model,
        cfg.max_tokens,
        &cfg.base_url,
        &cfg.api_key_env,
    )
}

/// Extract the assistant's message content from an OpenRouter-shape chat
/// completion response JSON.
///
/// PHASE A STUB (KTD6 / U4, not yet wired into `generate_inner`): mirrors
/// today's `generate_inner` behavior exactly — it ignores `finish_reason` and
/// `use_case` — so the truncation-handling tests below are red against
/// today's behavior rather than red against an unimplemented body. The next
/// phase makes this the real body of the `choices[0].message.content`
/// extraction in `generate_inner` and adds the `finish_reason == "length"` ->
/// `ProviderError::Truncated` branch (incident: 3 `distill::judge-error` rows
/// on 2026-09-16, `finish_reason: length`, truncated JSON that failed to
/// parse).
pub(crate) fn parse_chat_response(
    _use_case: &str,
    json: &serde_json::Value,
) -> Result<String, ProviderError> {
    json["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ProviderError::Upstream(format!("no content in response: {json}")))
}

fn generate_inner(
    use_case: &str,
    prompt: &str,
    model: &str,
    max_tokens: u32,
    base_url: &str,
    api_key_env: &str,
) -> Result<String, ProviderError> {
    let key = load_api_key(api_key_env).ok_or_else(|| {
        if api_key_env == "OPENROUTER_API_KEY" {
            ProviderError::Deferred(
                "OPENROUTER_API_KEY not set and .hex/secrets/openrouter.env missing or empty"
                    .into(),
            )
        } else {
            ProviderError::Deferred(format!(
                "API key env var `{api_key_env}` not set (resolved from llm.toml)"
            ))
        }
    })?;

    // Only force anthropic-direct routing when actually posting to OpenRouter.
    let body = if base_url.contains("openrouter.ai") {
        build_request_body(prompt, model, max_tokens)
    } else {
        serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
        })
    };

    let resp = ureq::post(base_url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|e| ProviderError::Upstream(format!("{base_url}: {e}")))?;

    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| ProviderError::Upstream(format!("response parse: {e}")))?;

    // OBS-024: OpenRouter-shape usage (prompt_tokens/completion_tokens;
    // `cost` is OpenRouter-specific and absent on other gateways → 0.0).
    if let Some(usage) = json.get("usage") {
        let in_tok = usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let out_tok = usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cost = usage.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
        crate::llm_cost::record_llm_cost(
            "openrouter",
            use_case,
            in_tok,
            out_tok,
            cost,
            Some(model),
        );
    } else {
        // S6 / OBS-024: a response with no usage block must NOT vanish from the
        // cost ledger — that's the exact silent under-report this seam exists to
        // kill. Be loud, and still count the call (0/0/0.0) so the gap is visible
        // as an anomaly (a real call with zero tokens) rather than absent.
        eprintln!(
            "provider: WARNING OpenRouter response for use_case={use_case} model={model} \
             had no `usage` block — cost unrecordable, logging a zero-token marker row"
        );
        crate::llm_cost::record_llm_cost("openrouter", use_case, 0, 0, 0.0, Some(model));
    }

    json["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ProviderError::Upstream(format!("no content in response: {json}")))
}

pub fn health_check() -> Result<String, ProviderError> {
    generate_for("health_check", "Respond with a single word: OK")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defer_when_no_key_present() {
        // HEX_DIR is process-global; serialize with every other test that mutates
        // it (see telemetry::test_support) so parallel tests don't stomp it.
        let _guard = crate::telemetry::test_support::lock_env();
        std::env::remove_var("OPENROUTER_API_KEY");
        // Point HEX_DIR at a directory with no secrets file so the file
        // fallback also finds nothing.
        std::env::set_var("HEX_DIR", "/tmp/hex-provider-test-empty-dir");
        let result = generate("hi", "anthropic/claude-sonnet-4.5", 10);
        std::env::remove_var("HEX_DIR");
        match result {
            Err(ProviderError::Deferred(_)) => {}
            _ => panic!("expected Deferred"),
        }
    }

    #[test]
    fn load_api_key_tolerates_export_prefix_in_secrets_file() {
        let _guard = crate::telemetry::test_support::lock_env();
        std::env::remove_var("OPENROUTER_API_KEY");
        let dir = std::env::temp_dir().join("hex-provider-test-export-prefix");
        let secrets = dir.join(".hex/secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        std::fs::write(
            secrets.join("openrouter.env"),
            "# comment line\nexport OPENROUTER_API_KEY=test-key-123\n",
        )
        .unwrap();
        std::env::set_var("HEX_DIR", &dir);
        let key = load_api_key("OPENROUTER_API_KEY");
        std::env::remove_var("HEX_DIR");
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(key.as_deref(), Some("test-key-123"));
    }

    #[test]
    fn anthropic_model_includes_provider_routing() {
        let body = build_request_body("hello", "anthropic/claude-sonnet-4-5", 100);
        let provider = &body["provider"];
        assert!(
            !provider.is_null(),
            "provider field must be present for anthropic/* models"
        );
        assert_eq!(provider["allow_fallbacks"], serde_json::json!(false));
        let order = provider["order"]
            .as_array()
            .expect("order must be an array");
        assert_eq!(order.len(), 1);
        assert_eq!(order[0], serde_json::json!("anthropic"));
    }

    #[test]
    fn openai_model_excludes_provider_routing() {
        let body = build_request_body("hello", "openai/gpt-4o", 100);
        assert!(
            body["provider"].is_null(),
            "provider field must NOT be present for non-anthropic models"
        );
    }

    #[test]
    fn generate_for_uses_resolved_api_key_env() {
        // Red test for T13qxa0dp: provider::generate_for(use_case, prompt) must
        // resolve config via llm_config and honor the resolved api_key_env when
        // looking for the API key. With a custom api_key_env set in llm.toml
        // and no value for that env var (nor OPENROUTER_API_KEY), the call
        // must return Deferred — proving it consulted the resolved config
        // rather than only the hardcoded OPENROUTER_API_KEY path.
        let _guard = crate::telemetry::test_support::lock_env();
        std::env::remove_var("OPENROUTER_API_KEY");
        std::env::remove_var("MY_CUSTOM_LLM_KEY");
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("HEX_DIR", td.path());
        std::fs::create_dir_all(td.path().join(".hex/config")).unwrap();
        std::fs::write(
            td.path().join(".hex/config/llm.toml"),
            r#"
[use_cases.memory_extract]
api_key_env = "MY_CUSTOM_LLM_KEY"
"#,
        )
        .unwrap();

        let result = generate_for("memory_extract", "hi");
        std::env::remove_var("HEX_DIR");
        match result {
            Err(ProviderError::Deferred(msg)) => {
                assert!(
                    msg.contains("MY_CUSTOM_LLM_KEY"),
                    "Deferred error should mention the resolved api_key_env \
                     `MY_CUSTOM_LLM_KEY`, got: {msg}"
                );
            }
            other => panic!("expected Deferred mentioning MY_CUSTOM_LLM_KEY, got: {other:?}"),
        }
    }

    #[test]
    fn non_prefixed_model_excludes_provider_routing() {
        let body = build_request_body("hello", "meta-llama/llama-3", 100);
        assert!(
            body["provider"].is_null(),
            "provider field must NOT be present for non-anthropic models"
        );
    }

    // Red tests for U4 / KTD6 (2026-09-16 distill::judge-error incident:
    // finish_reason "length" truncated the judge's JSON before it could be
    // parsed). `parse_chat_response` is still a Phase A stub that ignores
    // `finish_reason`, so the first two fail against today's behavior.

    #[test]
    fn parse_chat_response_truncated_with_partial_content_is_err_truncated() {
        let json = serde_json::json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"content": "{\"action\":\"ADD\""}
            }]
        });
        match parse_chat_response("memory_judge", &json) {
            Err(ProviderError::Truncated(msg)) => {
                assert!(
                    msg.contains("memory_judge"),
                    "expected use case name `memory_judge` in message, got: {msg}"
                );
                assert!(
                    msg.to_lowercase().contains("truncat"),
                    "expected 'truncated' in message, got: {msg}"
                );
            }
            other => panic!("expected Err(Truncated(_)), got: {other:?}"),
        }
    }

    #[test]
    fn parse_chat_response_truncated_with_no_content_is_truncated_not_upstream() {
        let json = serde_json::json!({
            "choices": [{
                "finish_reason": "length",
                "message": {}
            }]
        });
        match parse_chat_response("memory_judge", &json) {
            Err(ProviderError::Truncated(_)) => {}
            other => panic!("expected Err(Truncated(_)), got: {other:?}"),
        }
    }

    #[test]
    fn parse_chat_response_stop_with_content_returns_content() {
        let json = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": "hello"}
            }]
        });
        assert_eq!(parse_chat_response("memory_judge", &json).unwrap(), "hello");
    }

    #[test]
    fn parse_chat_response_no_choices_is_upstream() {
        let json = serde_json::json!({});
        match parse_chat_response("memory_judge", &json) {
            Err(ProviderError::Upstream(_)) => {}
            other => panic!("expected Err(Upstream(_)), got: {other:?}"),
        }
    }
}
