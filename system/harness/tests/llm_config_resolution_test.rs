// Red test for llm_config::resolve — see spec Sy23e65a2 task T9jf9hyx2,
// extended by spec Sbe8m4886 task T66j95j4d (transport + claude_settings_file).
//
// Verifies:
//   * Missing llm.toml → built-in defaults (zero behavior change)
//   * [use_cases.X] overrides [defaults]
//   * HEX_LLM_MODEL_<USE_CASE_UPPER> env var beats file
//   * HEX_CONSOLIDATE_MODEL alias still resolves consolidate_audit
//   * `transport` defaults to "http"; resolves through [defaults] and
//     [use_cases.X]; HEX_LLM_TRANSPORT_<USE_CASE_UPPER> env beats file
//   * `claude_settings_file` flows through [use_cases.X]
//   * Unknown `transport` value (file or env) is a hard error
//   * Malformed TOML is a hard error (not a silent fallback)
//
// All cases share one #[test] because they mutate process-wide env vars
// (HEX_DIR / HEX_LLM_MODEL_* / HEX_LLM_TRANSPORT_*) and must run sequentially.

use std::fs;

use hex::llm_config;

fn clear_overrides() {
    for k in [
        "HEX_LLM_MODEL_MEMORY_EXTRACT",
        "HEX_LLM_MODEL_MEMORY_JUDGE",
        "HEX_LLM_MODEL_CONSOLIDATE_AUDIT",
        "HEX_LLM_MODEL_HEALTH_CHECK",
        "HEX_CONSOLIDATE_MODEL",
        "HEX_LLM_TRANSPORT_MEMORY_EXTRACT",
        "HEX_LLM_TRANSPORT_MEMORY_JUDGE",
        "HEX_LLM_TRANSPORT_CONSOLIDATE_AUDIT",
        "HEX_LLM_TRANSPORT_HEALTH_CHECK",
    ] {
        std::env::remove_var(k);
    }
}

#[test]
fn llm_config_resolution_layers() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HEX_DIR", tmp.path());
    clear_overrides();

    // ---- 1. Missing llm.toml → built-in defaults ----------------------------
    let r = llm_config::resolve("memory_extract").expect("built-in default");
    assert_eq!(r.model, "anthropic/claude-sonnet-5");
    assert_eq!(r.max_tokens, 16384);

    let r = llm_config::resolve("memory_judge").expect("built-in default");
    assert_eq!(r.model, "anthropic/claude-sonnet-5");
    assert_eq!(r.max_tokens, 4096);

    let r = llm_config::resolve("consolidate_audit").expect("built-in default");
    assert_eq!(r.model, "anthropic/claude-sonnet-5");
    assert_eq!(r.max_tokens, 16384);

    let r = llm_config::resolve("health_check").expect("built-in default");
    assert_eq!(r.model, "anthropic/claude-haiku-4.5");

    // ---- 2. [use_cases.X] overrides [defaults] -----------------------------
    let cfg_dir = tmp.path().join(".hex/config");
    fs::create_dir_all(&cfg_dir).unwrap();
    let cfg_path = cfg_dir.join("llm.toml");
    fs::write(
        &cfg_path,
        r#"
[defaults]
model = "openrouter/default-model"

[use_cases.memory_extract]
model = "anthropic/claude-opus-4.1"
max_tokens = 9999
"#,
    )
    .unwrap();

    let r = llm_config::resolve("memory_extract").expect("file resolve");
    assert_eq!(r.model, "anthropic/claude-opus-4.1");
    assert_eq!(r.max_tokens, 9999);

    // use_case without override picks up [defaults].model, but max_tokens
    // still falls back to the built-in for that use case.
    let r = llm_config::resolve("memory_judge").expect("defaults fallback");
    assert_eq!(r.model, "openrouter/default-model");
    assert_eq!(r.max_tokens, 4096);

    // ---- 3. Env var beats file --------------------------------------------
    std::env::set_var("HEX_LLM_MODEL_MEMORY_EXTRACT", "anthropic/from-env-extract");
    let r = llm_config::resolve("memory_extract").expect("env override");
    assert_eq!(r.model, "anthropic/from-env-extract");
    std::env::remove_var("HEX_LLM_MODEL_MEMORY_EXTRACT");

    // ---- 4. HEX_CONSOLIDATE_MODEL alias for consolidate_audit -------------
    std::env::set_var("HEX_CONSOLIDATE_MODEL", "anthropic/legacy-alias");
    let r = llm_config::resolve("consolidate_audit").expect("alias env override");
    assert_eq!(r.model, "anthropic/legacy-alias");
    std::env::remove_var("HEX_CONSOLIDATE_MODEL");

    // ---- 5. Transport + claude_settings_file resolution -------------------
    //
    // T66j95j4d: add `transport` (default "http") and `claude_settings_file`
    // (default None) to ResolvedLlm. Both are settable per-use-case and at
    // [defaults]; transport additionally honors HEX_LLM_TRANSPORT_<UC_UPPER>
    // env override. Unknown transport values must be a hard error.

    // 5a. Reset the config file to a minimal valid TOML; verify built-in
    //     defaults give transport="http" and claude_settings_file=None.
    fs::write(&cfg_path, "").unwrap();
    let r = llm_config::resolve("memory_extract").expect("built-in transport default");
    assert_eq!(
        r.transport, "http",
        "transport must default to \"http\" so existing deployments are unchanged"
    );
    assert!(
        r.claude_settings_file.is_none(),
        "claude_settings_file must default to None"
    );

    // 5b. transport in [defaults] flows through to a use case that does not
    //     set its own transport.
    fs::write(
        &cfg_path,
        r#"
[defaults]
transport = "claude-cli"
"#,
    )
    .unwrap();
    let r = llm_config::resolve("memory_judge").expect("defaults transport flows through");
    assert_eq!(r.transport, "claude-cli");

    // 5c. transport + claude_settings_file in [use_cases.X] override
    //     [defaults]. settings file path is preserved verbatim.
    fs::write(
        &cfg_path,
        r#"
[defaults]
transport = "http"

[use_cases.memory_extract]
transport = "claude-cli"
claude_settings_file = "/tmp/hex-claude-settings.json"
"#,
    )
    .unwrap();
    let r = llm_config::resolve("memory_extract").expect("use_case transport override");
    assert_eq!(r.transport, "claude-cli");
    assert_eq!(
        r.claude_settings_file.as_deref(),
        Some("/tmp/hex-claude-settings.json")
    );

    // 5d. HEX_LLM_TRANSPORT_<UC_UPPER> beats the file (same precedence as
    //     HEX_LLM_MODEL_*). Set env to "http" while file says "claude-cli"
    //     to prove the env actually wins both directions.
    std::env::set_var("HEX_LLM_TRANSPORT_MEMORY_EXTRACT", "http");
    let r = llm_config::resolve("memory_extract").expect("env transport override");
    assert_eq!(
        r.transport, "http",
        "HEX_LLM_TRANSPORT_MEMORY_EXTRACT must override [use_cases.memory_extract].transport"
    );
    std::env::remove_var("HEX_LLM_TRANSPORT_MEMORY_EXTRACT");

    // 5e. Unknown transport value in the file is a loud, hard error
    //     (no silent fallback per S6).
    fs::write(
        &cfg_path,
        r#"
[use_cases.memory_extract]
transport = "carrier-pigeon"
"#,
    )
    .unwrap();
    let err = llm_config::resolve("memory_extract");
    assert!(
        err.is_err(),
        "unknown transport value must be a loud error, got Ok({:?})",
        err.ok()
    );
    let msg = format!("{:#}", err.err().unwrap()).to_lowercase();
    assert!(
        msg.contains("transport") || msg.contains("carrier-pigeon"),
        "error message must name the bad transport field, got: {msg}"
    );

    // 5f. Unknown transport value via env var is also a hard error.
    fs::write(&cfg_path, "").unwrap();
    std::env::set_var("HEX_LLM_TRANSPORT_MEMORY_EXTRACT", "smoke-signal");
    let err = llm_config::resolve("memory_extract");
    assert!(
        err.is_err(),
        "unknown env transport must be a loud error, got Ok({:?})",
        err.ok()
    );
    std::env::remove_var("HEX_LLM_TRANSPORT_MEMORY_EXTRACT");

    // ---- 6. Malformed TOML is a hard error --------------------------------
    fs::write(&cfg_path, "this is = not valid = toml [[[").unwrap();
    let err = llm_config::resolve("memory_extract");
    assert!(
        err.is_err(),
        "malformed llm.toml must be a loud error, got Ok({:?})",
        err.ok()
    );
}
