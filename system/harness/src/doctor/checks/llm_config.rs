//! Doctor check for the per-use-case LLM config file ($HEX_DIR/.hex/config/llm.toml).
//!
//! - Absent file → PASS (built-in defaults will be used at runtime).
//! - Present + valid → PASS with the resolved model reported per known use case.
//! - Present + malformed → FAIL loudly (per no-quiet-failures doctrine).
//!
//! Also provides StaleLlmPreferenceCheck, which warns when the dead
//! `.hex/llm-preference` placeholder file is found, suggesting removal.

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use hex::llm_config::known_use_cases;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

pub struct LlmConfigCheck;

#[derive(Debug, Deserialize, Default)]
struct LlmTomlFile {
    #[serde(default)]
    defaults: Option<SectionFields>,
    #[serde(default)]
    use_cases: HashMap<String, SectionFields>,
}

// KEEP IN SYNC with hex::llm_config::SectionFields — this is a duplicate
// schema used only for validation. A field added there but not here makes
// this check reject configs the runtime accepts (bit us 2026-06-10 when
// `transport` landed in the runtime schema only).
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // most fields are parsed for validation only
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
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    claude_settings_file: Option<String>,
}

fn builtin_model(use_case: &str) -> &'static str {
    match use_case {
        "memory_extract" | "memory_judge" | "consolidate_audit" => "anthropic/claude-sonnet-5",
        "health_check" => "anthropic/claude-haiku-4.5",
        _ => "unknown",
    }
}

fn resolve_model(parsed: &LlmTomlFile, use_case: &str) -> String {
    let uc_model = parsed.use_cases.get(use_case).and_then(|s| s.model.clone());
    let default_model = parsed.defaults.as_ref().and_then(|s| s.model.clone());
    uc_model
        .or(default_model)
        .unwrap_or_else(|| builtin_model(use_case).to_string())
}

fn cfg_path(hex_dir: &Path) -> std::path::PathBuf {
    hex_dir.join(".hex/config/llm.toml")
}

impl DoctorCheck for LlmConfigCheck {
    fn name(&self) -> &str {
        "llm-config"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let path = cfg_path(&ctx.hex_dir);
        if !path.exists() {
            return CheckResult::pass("llm.toml absent — built-in defaults will be used");
        }
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                return CheckResult::fail(format!("could not read {}: {}", path.display(), e));
            }
        };
        let parsed: LlmTomlFile = match toml::from_str(&body) {
            Ok(p) => p,
            Err(e) => {
                return CheckResult::fail(format!(
                    "malformed llm.toml at {}: {}",
                    path.display(),
                    e
                ));
            }
        };

        // Warn (in details) about unknown [use_cases.*] table names — tolerated.
        let mut unknown: Vec<String> = parsed
            .use_cases
            .keys()
            .filter(|k| !known_use_cases().contains(&k.as_str()))
            .cloned()
            .collect();
        unknown.sort();

        let mut lines = Vec::new();
        for uc in known_use_cases() {
            lines.push(format!("  {} → {}", uc, resolve_model(&parsed, uc)));
        }
        if !unknown.is_empty() {
            lines.push(format!(
                "  warning: unknown use_cases tables: {}",
                unknown.join(", ")
            ));
        }
        let details = lines.join("\n");
        CheckResult::pass(format!("llm.toml parsed: {}", path.display())).with_details(details)
    }
}

/// Warn when the stale (dead) `.hex/llm-preference` placeholder file exists.
/// The file is no longer used by anything; suggest removing it.
pub struct StaleLlmPreferenceCheck;

impl DoctorCheck for StaleLlmPreferenceCheck {
    fn name(&self) -> &str {
        "stale-llm-preference"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let path = ctx.hex_dir.join(".hex/llm-preference");
        if !path.exists() {
            return CheckResult::pass("no stale .hex/llm-preference");
        }
        if ctx.fix && std::fs::remove_file(&path).is_ok() {
            return CheckResult::fixed(format!(
                "removed stale {} (no longer used)",
                path.display()
            ));
        }
        CheckResult::warn(format!(
            "stale {} present — file is unused; remove it",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::check::{Category, Context, DoctorCheck, Status};
    use std::fs;
    use std::path::PathBuf;

    fn tmp_ctx(suffix: &str) -> (Context, PathBuf) {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("hex-llm-cfg-{pid}-{nanos}-{suffix}"));
        let _ = fs::create_dir_all(&dir);
        let ctx = Context {
            hex_dir: dir.clone(),
            home: PathBuf::from("/tmp"),
            fix: false,
        };
        (ctx, dir)
    }

    #[test]
    fn llm_config_check_name_and_category() {
        let c = LlmConfigCheck;
        assert_eq!(c.name(), "llm-config");
        assert_eq!(c.category(), Category::Config);
    }

    #[test]
    fn passes_when_llm_toml_absent() {
        let (ctx, dir) = tmp_ctx("absent");
        let r = LlmConfigCheck.run(&ctx);
        assert_eq!(
            r.status,
            Status::Pass,
            "missing llm.toml must pass: {:?}",
            r
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fails_loudly_on_malformed_llm_toml() {
        let (ctx, dir) = tmp_ctx("bad");
        let cfg = dir.join(".hex/config");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(cfg.join("llm.toml"), "this is = = not valid toml [[[").unwrap();
        let r = LlmConfigCheck.run(&ctx);
        assert_eq!(
            r.status,
            Status::Fail,
            "malformed llm.toml must fail loudly: {:?}",
            r
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn passes_and_reports_resolved_model_when_valid() {
        let (ctx, dir) = tmp_ctx("ok");
        let cfg = dir.join(".hex/config");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(
            cfg.join("llm.toml"),
            "[defaults]\nmodel = \"anthropic/claude-sonnet-5\"\n",
        )
        .unwrap();
        let r = LlmConfigCheck.run(&ctx);
        assert_eq!(r.status, Status::Pass, "valid llm.toml must pass: {:?}", r);
        // Resolved model per use case is reported (in message or details).
        let blob = format!("{} {}", r.message, r.details.clone().unwrap_or_default());
        assert!(
            blob.contains("memory_extract") || blob.contains("health_check"),
            "expected resolved-model report to mention known use cases, got: {}",
            blob
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn passes_with_transport_and_claude_settings_fields() {
        let (ctx, dir) = tmp_ctx("transport");
        let cfg = dir.join(".hex/config");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(
            cfg.join("llm.toml"),
            "[use_cases.memory_extract]\ntransport = \"claude-cli\"\n\n[use_cases.consolidate_audit]\ntransport = \"claude-cli\"\nclaude_settings_file = \"/tmp/x.json\"\nmax_input_tokens = 100000\n",
        )
        .unwrap();
        let r = LlmConfigCheck.run(&ctx);
        assert_eq!(
            r.status,
            Status::Pass,
            "llm.toml with transport/claude_settings_file/max_input_tokens must pass: {:?}",
            r
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // The doctor must no longer auto-create .hex/llm-preference, even when
    // fix=true. The new behavior: nothing is written.
    #[test]
    fn doctor_no_longer_creates_llm_preference_placeholder() {
        let (mut ctx, dir) = tmp_ctx("nocreate");
        ctx.fix = true;
        // Run the full set of doctor checks via the public runner if available;
        // otherwise scan the checks registry. Simplest portable assertion:
        // the LlmPreferenceExists check must NOT exist OR must not create the
        // file when fix is true.
        let placeholder = dir.join(".hex/llm-preference");
        // Run the historical check struct (if it still exists) — it must not
        // create the file. We import lazily so removal of the symbol is also
        // an acceptable green outcome (covered by the next test).
        #[allow(unused_imports)]
        use crate::doctor::checks::llm_preference::LlmPreferenceExists;
        let _ = LlmPreferenceExists.run(&ctx);
        assert!(
            !placeholder.exists(),
            ".hex/llm-preference must not be auto-created any more"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // A check must warn when the stale .hex/llm-preference file exists.
    #[test]
    fn stale_llm_preference_warns() {
        let (ctx, dir) = tmp_ctx("stale");
        let stale = dir.join(".hex/llm-preference");
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(&stale, "claude\n").unwrap();
        let r = StaleLlmPreferenceCheck.run(&ctx);
        assert_eq!(
            r.status,
            Status::Warn,
            "stale .hex/llm-preference must warn: {:?}",
            r
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
