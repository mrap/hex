// Guards the `large-unignored-files` doctor check (unit U5, plan
// 2026-09-14-1902-feat-testing-standard-followups).
//
// Incident: projects/system-improvement/incidents/git-cpu-codex-snapshot-2026-09-09.md
// A 3.28 GB untracked disk image sat in the hex tree and every Codex turn
// re-hashed it. Fix item 4 of that incident asks `hex doctor` to flag any
// file over 256 MB that git does not ignore, so the next stray large file
// gets caught before it burns CPU on every turn.

use std::fs::File;
use std::process::{Command, Output};
use tempfile::TempDir;

const ONE_MIB: u64 = 1024 * 1024;

/// Build a temp HEX_DIR with the minimal shape `get_hex_dir()` requires
/// (a `CLAUDE.md`), a git repo, and a `raw/` dir to hold the oversized
/// fixture file.
fn fixture() -> TempDir {
    let dir = tempfile::tempdir().expect("create tempdir");
    let p = dir.path();

    std::fs::write(p.join("CLAUDE.md"), "").unwrap();
    std::fs::create_dir_all(p.join("raw")).unwrap();

    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(p)
        .status()
        .expect("git init must run");
    assert!(status.success(), "git init failed");

    let status = Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(p)
        .status()
        .expect("git config user.email must run");
    assert!(status.success(), "git config user.email failed");

    let status = Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(p)
        .status()
        .expect("git config user.name must run");
    assert!(status.success(), "git config user.name failed");

    dir
}

/// Spawn the real `hex` binary against `hex_dir`, scoped to the
/// `large-unignored` check with `--filter` so this test does not depend on
/// every other doctor check passing in a bare fixture.
fn run_doctor(hex_dir: &std::path::Path) -> Output {
    let bin = env!("CARGO_BIN_EXE_hex");
    Command::new(bin)
        .args(["doctor", "run", "--filter", "large-unignored"])
        .env_clear()
        .env("HEX_DIR", hex_dir)
        .env("HOME", hex_dir)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("hex binary must run")
}

/// Create `path` (relative to `dir`) as a sparse file of exactly `bytes` in
/// length, without writing `bytes` worth of data to disk.
fn make_sparse_file(dir: &std::path::Path, rel_path: &str, bytes: u64) {
    let full = dir.join(rel_path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let file = File::create(&full).expect("create sparse file");
    file.set_len(bytes).expect("set_len must succeed");
}

#[test]
fn doctor_fails_and_names_an_unignored_file_over_256_mb() {
    let hex_dir = fixture();
    make_sparse_file(hex_dir.path(), "raw/archive.dmg", 257 * ONE_MIB);

    let output = run_doctor(hex_dir.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit 1 for an unignored file over 256 MB; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("over 256 MB"),
        "stdout must explain the file is over 256 MB; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("raw/archive.dmg"),
        "stdout must name the offending file raw/archive.dmg; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn doctor_passes_when_the_large_file_is_gitignored() {
    let hex_dir = fixture();
    make_sparse_file(hex_dir.path(), "raw/archive.dmg", 257 * ONE_MIB);
    std::fs::write(hex_dir.path().join(".gitignore"), "*.dmg\n").unwrap();

    let output = run_doctor(hex_dir.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(0),
        "a gitignored large file must not fail doctor; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn doctor_does_not_flag_a_file_of_exactly_256_mb() {
    let hex_dir = fixture();
    make_sparse_file(hex_dir.path(), "raw/archive.dmg", 256 * ONE_MIB);

    let output = run_doctor(hex_dir.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(0),
        "a file at exactly the 256 MB boundary must not be flagged; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn doctor_skips_the_check_outside_a_git_repository() {
    // Same shape as fixture() but with no `git init`, so the check has no
    // repository to enumerate unignored files from.
    let dir = tempfile::tempdir().expect("create tempdir");
    let p = dir.path();
    std::fs::write(p.join("CLAUDE.md"), "").unwrap();
    std::fs::create_dir_all(p.join("raw")).unwrap();

    let output = run_doctor(p);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(0),
        "outside a git repo the check must skip, not fail; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("SKIP"),
        "stdout must show the check was skipped; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
