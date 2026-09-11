//! Red tests for self-throttle wiring (task Tr7pzxkk5).
//!
//! `hex memory consolidate {quick,full}` must accept a `--max` flag that opts
//! out of background-priority self-throttling. The throttle module
//! (src/throttle.rs) must exist and expose the documented surface.

use std::path::PathBuf;
use std::process::Command;

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

#[test]
fn throttle_module_file_exists() {
    let p = src_dir().join("throttle.rs");
    assert!(
        p.exists(),
        "system/harness/src/throttle.rs must exist (the self-throttle module)"
    );
}

#[test]
fn throttle_module_exposes_documented_surface() {
    let p = src_dir().join("throttle.rs");
    let body = std::fs::read_to_string(&p).expect("throttle.rs must be readable");
    for needle in [
        "pub fn should_throttle",
        "pub fn lower_to_background",
        "pub fn apply",
    ] {
        assert!(
            body.contains(needle),
            "throttle.rs must define `{needle}`; current body:\n{body}"
        );
    }
}

#[test]
fn consolidate_full_help_lists_max_flag() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let out = Command::new(bin)
        .args(["memory", "consolidate", "full", "--help"])
        .output()
        .expect("run hex memory consolidate full --help");
    assert!(
        out.status.success(),
        "`hex memory consolidate full --help` must succeed. stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("--max"),
        "`hex memory consolidate full --help` must list `--max` flag; got:\n{help}"
    );
}

#[test]
fn consolidate_quick_help_lists_max_flag() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let out = Command::new(bin)
        .args(["memory", "consolidate", "quick", "--help"])
        .output()
        .expect("run hex memory consolidate quick --help");
    assert!(
        out.status.success(),
        "`hex memory consolidate quick --help` must succeed. stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("--max"),
        "`hex memory consolidate quick --help` must list `--max` flag; got:\n{help}"
    );
}

// Regression guard (task Sw... KTD7, U4): `--max` on `hex memory index` is not
// new behavior — it predates the backlog-escalation work — but it is now the
// CLI surface that forces `Escalation::Normal` in
// `memory::index::escalation_decision` (via the `throttle: fn(&str, bool)`
// injection wired through `memory::index::run` → `run_index_cli`). This just
// confirms the flag stays advertised on that arm.
#[test]
fn index_help_lists_max_flag() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let out = Command::new(bin)
        .args(["memory", "index", "--help"])
        .output()
        .expect("run hex memory index --help");
    assert!(
        out.status.success(),
        "`hex memory index --help` must succeed. stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("--max"),
        "`hex memory index --help` must list `--max` flag; got:\n{help}"
    );
}
