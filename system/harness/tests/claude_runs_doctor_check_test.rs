//! Red test for task Tn1wrdke4: doctor check + example config + docs.
//!
//! These tests pin down the deliverables of the doctor/docs task:
//!   1. A doctor check named "claude-runs-config" is registered in the runner.
//!   2. The example config at system/templates/claude-runs.toml.example exists
//!      and parses as valid TOML (proves we shipped a working schema sample).
//!   3. Docs under docs/ mention `claude-runs.toml` so operators can discover
//!      the lean-by-default policy.
//!
//! All three will fail until task Tn1wrdke4 is implemented.

use hex::doctor::Runner;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = system/harness
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root resolvable")
}

#[test]
fn doctor_registry_contains_claude_runs_config_check() {
    let runner = Runner::all_checks();
    let names: Vec<String> = runner.checks.iter().map(|c| c.name().to_string()).collect();
    assert!(
        names.iter().any(|n| n == "claude-runs-config"),
        "expected a doctor check named `claude-runs-config` in the registry, \
         got: {names:?}"
    );
}

#[test]
fn example_claude_runs_config_exists_and_parses() {
    let path = repo_root().join("system/templates/claude-runs.toml.example");
    assert!(
        path.is_file(),
        "expected example config at {} — task Tn1wrdke4 must ship it",
        path.display()
    );
    let body = std::fs::read_to_string(&path).expect("read example config");
    // Sanity: schema sample should at least document a profile and a defaults block.
    assert!(
        body.contains("[defaults]") && body.contains("[runs."),
        "example config should demonstrate the [defaults] and [runs.*] schema, got:\n{body}"
    );
    // Parse in-process so this release test does not depend on whichever
    // `python3` happens to be first on the operator's PATH.
    toml::from_str::<toml::Value>(&body).expect("example config must parse as valid TOML");
}

#[test]
fn docs_mention_claude_runs_toml() {
    let docs_dir = repo_root().join("docs");
    let mut hit = false;
    for entry in walkdir::WalkDir::new(&docs_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        if let Ok(body) = std::fs::read_to_string(entry.path()) {
            if body.contains("claude-runs.toml") {
                hit = true;
                break;
            }
        }
    }
    assert!(
        hit,
        "expected at least one file under docs/ to mention `claude-runs.toml`"
    );
}
