//! Guards: after `hex upgrade` swaps the binary, every sanctioned launchd job
//! must be reloaded, not just `com.hex.harness`.
//!
//! Incident: `$HEX_DIR/projects/system-improvement/incidents/hex-launch-2026-09-09/`.
//! `hex upgrade` swapped the hex binary and restarted `com.hex.harness`, but
//! never refreshed the already loaded `com.hex.failures-probe` launchd job,
//! which then crashed on a stale launch constraint. These tests run the real
//! `hex upgrade --dry-run` subcommand (spawned as its own process, with a
//! fixture `HOME` and `HEX_DIR`, so the launchd job list it prints can be
//! checked without touching real launchd state) and assert the dry run names
//! the sanctioned jobs it would reload.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn write_file(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn init_test_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git must be runnable in tests")
            .success();
        assert!(ok, "git {args:?} failed while preparing test repo");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@example.com"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "commit.gpgsign", "false"]);
}

fn seed_commit(dir: &Path, msg: &str) {
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .status()
        .unwrap();
    let ok = Command::new("git")
        .args(["commit", "-q", "-m", msg])
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "seed commit must succeed");
}

/// Fixture shape mirrors `run_build_failure_leaves_live_managed_files_unchanged`
/// in `system/harness/src/upgrade.rs`: the smallest source and instance tree
/// that gets `hex upgrade` past preflight (source layout detection, managed
/// file inventory, binary staleness check, code-intel inspect) so the dry
/// run reaches its summary instead of failing earlier for an unrelated
/// reason.
struct Fixture {
    _temp: tempfile::TempDir,
    source: PathBuf,
    home: PathBuf,
    hex_dir: PathBuf,
}

impl Fixture {
    /// `launch_agents` lists the plist file names (e.g. "com.hex.harness.plist")
    /// to seed under `$HOME/Library/LaunchAgents/`. An empty slice leaves that
    /// directory empty (but present) to exercise the "none installed" path.
    fn new(launch_agents: &[&str]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let instance = temp.path().join("instance");
        let home = temp.path().join("home");

        // Source: a minimal v2-layout foundation checkout with a higher
        // harness version than what the instance has "installed", so the
        // preflight sees a stale binary and does not short-circuit as
        // "nothing to do" before reaching the dry-run summary.
        write_file(&source.join("templates/AGENTS.md"), "# source\n");
        write_file(&source.join("system/scripts/managed.sh"), "new script\n");
        write_file(
            &source.join("system/managed_cargo_bridge.rs"),
            "new bridge\n",
        );
        write_file(&source.join("system/version.txt"), "2.0.0\n");
        write_file(
            &source.join("system/harness/Cargo.toml"),
            "[package]\nname = \"hex-harness\"\nversion = \"2.0.0\"\nedition = \"2021\"\n",
        );
        init_test_repo(&source);
        seed_commit(&source, "upgrade launchd reload fixture");

        // Instance: an existing "installation" at an older version, with the
        // old managed files so the preflight has something to diff against.
        write_file(&instance.join("CLAUDE.md"), "# instance\n");
        write_file(&instance.join("AGENTS.md"), "# instance\n");
        write_file(
            &instance.join("VERSIONS"),
            "HEX_FOUNDATION_VERSION=v1.0.0\n",
        );
        let bin = instance.join(".hex/bin/hex");
        write_file(&bin, "#!/bin/sh\nprintf 'hex 1.0.0\\n'\n");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        write_file(&instance.join(".hex/scripts/managed.sh"), "old script\n");
        write_file(
            &instance.join(".hex/managed_cargo_bridge.rs"),
            "old bridge\n",
        );

        // Fixture HOME: only the LaunchAgents directory matters here. Content
        // of each plist is irrelevant to `hex upgrade --dry-run` today (it
        // only checks presence), so a short placeholder is enough.
        let launch_agents_dir = home.join("Library/LaunchAgents");
        fs::create_dir_all(&launch_agents_dir).unwrap();
        for name in launch_agents {
            write_file(
                &launch_agents_dir.join(name),
                "<?xml version=\"1.0\"?>\n<!-- fixture plist, content unused -->\n",
            );
        }

        Self {
            _temp: temp,
            source,
            home,
            hex_dir: instance,
        }
    }

    fn run_dry_run(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hex"))
            .args(["upgrade", "--dry-run", "--local"])
            .arg(&self.source)
            .env_clear()
            .env("HOME", &self.home)
            .env("HEX_DIR", &self.hex_dir)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .expect("run hex upgrade --dry-run")
    }
}

/// Pull out the single stdout line that announces which launchd jobs the
/// upgrade would reload. Panics with the full stdout/stderr if no such line
/// exists, so a fixture problem and a "the feature isn't built yet" failure
/// are easy to tell apart from the assertion message.
fn reload_line(stdout: &str, stderr: &str) -> String {
    stdout
        .lines()
        .find(|line| line.contains("launchd jobs to reload"))
        .unwrap_or_else(|| {
            panic!(
                "expected a stdout line mentioning \"launchd jobs to reload\", found none.\n\
                 --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
            )
        })
        .to_string()
}

#[test]
fn dry_run_lists_every_sanctioned_launchd_job_installed_in_home() {
    let fixture = Fixture::new(&[
        "com.hex.failures-probe.plist",
        "com.hex.scipd.plist",
        "com.hex.harness.plist",
        "com.other.plist",
    ]);
    let output = fixture.run_dry_run();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "hex upgrade --dry-run must exit 0.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );

    let line = reload_line(&stdout, &stderr);
    assert!(
        line.contains("com.hex.failures-probe"),
        "reload line must name com.hex.failures-probe (the incident job): {line}"
    );
    assert!(
        line.contains("com.hex.scipd"),
        "reload line must name com.hex.scipd (sanctioned, non-harness job): {line}"
    );
    assert!(
        !line.contains("com.hex.harness"),
        "reload line must not name com.hex.harness (it is restarted separately, \
         via restart_and_verify, not this reload path): {line}"
    );
    assert!(
        !line.contains("com.other"),
        "reload line must not name an unsanctioned job: {line}"
    );
}

#[test]
fn dry_run_reports_none_installed_when_launch_agents_dir_is_empty() {
    let fixture = Fixture::new(&[]);
    let output = fixture.run_dry_run();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "hex upgrade --dry-run must exit 0.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );

    let line = reload_line(&stdout, &stderr);
    assert!(
        line.contains("none installed"),
        "with no sanctioned plists present, the reload line must say \
         \"none installed\": {line}"
    );
}
