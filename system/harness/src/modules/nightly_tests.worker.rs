//! `hex-nightly-tests`, nightly run of the full test suite including the
//! `#[ignore]`d model tests.
//!
//! Why this exists: twelve `#[ignore]` model tests exist across the
//! workspace, and nothing ever ran them on a schedule. This is the testing
//! standard's nightly layer: a check that costs more than a normal
//! `cargo test` run belongs on cron, not on every developer's machine. This
//! worker closes that gap: every night it runs the container test lane with
//! `--run-ignored all`.
//!
//! Cron `0 0 10 * * * *` is 10:00 UTC, 03:00 PT, clear of the 03:00 UTC full
//! memory consolidation and the 04:00 UTC backup.
//!
//! This worker needs the Docker daemon (OrbStack) reachable at cron time.
//! When Docker is down, `system/scripts/test-lane.sh` exits 2 with a reason
//! on stderr and this worker returns `Err`, loud by design (S6), not a
//! silent skip. The lane's `docker build` step is layer cached, so a healthy
//! run does not rebuild the image; only a real source change triggers a
//! rebuild.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

use hex::worker::{ctx::Ctx, event::Event, Worker};

/// Cron expression: 10:00 UTC daily (03:00 PT), per A5. Clear of the 03:00
/// UTC full consolidation and the 04:00 UTC backup.
pub const CRON_NIGHTLY: &str = "0 0 10 * * * *";

/// Config file naming the repo to test, relative to `HEX_DIR`.
pub const CONFIG_REL: &str = ".hex/config/nightly-tests.toml";

/// Fallback repo location when the config file is absent: the clone `hex
/// upgrade` keeps up to date, relative to `HEX_DIR`.
pub const UPGRADE_CACHE_REL: &str = ".hex/.upgrade-cache";

/// Resolve the repo to run the nightly lane against.
///
/// Reads `<hex_dir>/.hex/config/nightly-tests.toml` for a string key `repo`
/// (a leading `~` is expanded against `dirs::home_dir()`). When the file is
/// present but fails to parse as TOML, or parses without a `repo` key, this
/// is `Err`. A broken config is loud, never silently skipped (S6). When the
/// file is absent, falls back to `<hex_dir>/.hex/.upgrade-cache`. Either way,
/// the resolved repo must contain `system/scripts/test-lane.sh`; if it does
/// not, this is `Err` naming the repo path and both locations that were
/// tried.
pub fn repo_path(hex_dir: &Path) -> Result<PathBuf> {
    let config_path = hex_dir.join(CONFIG_REL);
    let upgrade_cache_path = hex_dir.join(UPGRADE_CACHE_REL);

    let repo = match std::fs::read_to_string(&config_path) {
        Ok(raw) => {
            let value: toml::Value = toml::from_str(&raw).map_err(|e| {
                anyhow!(
                    "hex-nightly-tests: {} does not parse as TOML: {e}",
                    config_path.display()
                )
            })?;
            let repo_str = value.get("repo").and_then(|v| v.as_str()).ok_or_else(|| {
                anyhow!(
                    "hex-nightly-tests: {} has no string `repo` key",
                    config_path.display()
                )
            })?;
            expand_tilde(repo_str)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => upgrade_cache_path.clone(),
        Err(e) => {
            return Err(anyhow!(
                "hex-nightly-tests: read {} failed: {e}",
                config_path.display()
            ))
        }
    };

    let lane_script = repo.join("system/scripts/test-lane.sh");
    if !lane_script.exists() {
        return Err(anyhow!(
            "hex-nightly-tests: no test-lane.sh under resolved repo {} \
             (tried config {} and fallback {})",
            repo.display(),
            config_path.display(),
            upgrade_cache_path.display()
        ));
    }

    Ok(repo)
}

/// Expand a leading `~` (or `~/...`) against `dirs::home_dir()`. A path
/// without a leading `~`, or a home dir that cannot be resolved, passes
/// through unchanged.
fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if raw == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(raw)
}

/// Build the argv that runs the lane with every `#[ignore]` test included.
pub fn lane_argv(repo: &Path) -> Vec<String> {
    vec![
        "bash".to_string(),
        "-c".to_string(),
        "cd \"$1\" && bash system/scripts/test-lane.sh -- --run-ignored all".to_string(),
        "_".to_string(),
        repo.display().to_string(),
    ]
}

/// Resolve the repo and run the nightly lane against it. Separated from the
/// handler so tests control `hex_dir` and the `Ctx` directly.
pub fn run_nightly_at(hex_dir: &Path, ctx: &Ctx) -> Result<()> {
    let repo = repo_path(hex_dir)?;
    eprintln!(
        "[hex-nightly-tests] running lane against {} (--run-ignored all)",
        repo.display()
    );
    ctx.run(&lane_argv(&repo)).map(|_| ())
}

/// Handler glue: resolve `HEX_DIR` from the environment, delegate to
/// `run_nightly_at`. A missing `HEX_DIR` is `Err`, loud (S6).
fn run_nightly(_e: Event, ctx: Ctx) -> Result<()> {
    let hex_dir =
        std::env::var("HEX_DIR").map_err(|_| anyhow!("hex-nightly-tests: HEX_DIR is not set"))?;
    run_nightly_at(&PathBuf::from(hex_dir), &ctx)
}

/// Build the `hex-nightly-tests` worker.
pub fn worker() -> Worker {
    Worker::new("hex-nightly-tests").on_cron_named("nightly", CRON_NIGHTLY, run_nightly)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Write a lane script fixture at `<repo>/system/scripts/test-lane.sh`
    /// with the given body, made executable.
    fn write_lane_script(repo: &Path, body: &str) {
        let script_dir = repo.join("system/scripts");
        std::fs::create_dir_all(&script_dir).unwrap();
        let script_path = script_dir.join("test-lane.sh");
        std::fs::write(&script_path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }
    }

    #[test]
    fn lane_argv_runs_the_lane_with_run_ignored_all() {
        let repo = PathBuf::from("/some/repo");
        let argv = lane_argv(&repo);
        let joined = argv.join(" ");
        assert!(joined.contains("test-lane.sh"), "argv: {joined}");
        assert!(
            joined.contains("-- --profile nightly --no-fail-fast --run-ignored all"),
            "argv must pass --profile nightly, --no-fail-fast, and --run-ignored all \
             (in that order) after --: {joined}"
        );
        assert_eq!(
            argv.last().map(String::as_str),
            Some("/some/repo"),
            "the repo path must be the last argv entry (bash's $1): {argv:?}"
        );
    }

    #[test]
    fn nextest_profile_nightly_excludes_live_tests() {
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let nextest_toml_path = workspace_root.join(".config/nextest.toml");
        let raw = std::fs::read_to_string(&nextest_toml_path).unwrap_or_else(|e| {
            panic!(
                "read {} failed: {e} (expected the nightly profile config at the workspace root)",
                nextest_toml_path.display()
            )
        });
        let value: toml::Value = toml::from_str(&raw).unwrap_or_else(|e| {
            panic!(
                "{} does not parse as TOML: {e}",
                nextest_toml_path.display()
            )
        });
        let default_filter = value
            .get("profile")
            .and_then(|p| p.get("nightly"))
            .and_then(|n| n.get("default-filter"))
            .and_then(|v| v.as_str());
        assert_eq!(
            default_filter,
            Some("not test(/_live$/)"),
            "profile.nightly.default-filter must exclude _live tests, got: {raw}"
        );
    }

    #[test]
    fn repo_path_reads_the_config_file() {
        let hex_dir = tempdir().unwrap();
        let repo = tempdir().unwrap();
        write_lane_script(repo.path(), "#!/bin/sh\nexit 0\n");

        std::fs::create_dir_all(hex_dir.path().join(".hex/config")).unwrap();
        std::fs::write(
            hex_dir.path().join(CONFIG_REL),
            format!("repo = \"{}\"\n", repo.path().display()),
        )
        .unwrap();

        let resolved = repo_path(hex_dir.path()).expect("repo_path must resolve");
        assert_eq!(resolved, repo.path());
    }

    #[test]
    fn repo_path_falls_back_to_the_upgrade_cache_when_no_config() {
        let hex_dir = tempdir().unwrap();
        let cache_dir = hex_dir.path().join(UPGRADE_CACHE_REL);
        write_lane_script(&cache_dir, "#!/bin/sh\nexit 0\n");

        let resolved = repo_path(hex_dir.path()).expect("repo_path must fall back");
        assert_eq!(resolved, cache_dir);
    }

    #[test]
    fn repo_path_is_loud_when_neither_location_has_the_lane_script() {
        let hex_dir = tempdir().unwrap();
        // Neither the config file nor the upgrade-cache fallback exists.
        let err = repo_path(hex_dir.path()).expect_err("must be Err when nothing has the lane");
        let msg = err.to_string();
        assert!(
            msg.contains(&hex_dir.path().join(UPGRADE_CACHE_REL).display().to_string()),
            "error must name the upgrade-cache fallback path: {msg}"
        );
        assert!(
            msg.contains(&hex_dir.path().join(CONFIG_REL).display().to_string()),
            "error must name the config path that was checked: {msg}"
        );
    }

    #[test]
    fn repo_path_is_loud_when_the_config_lacks_repo() {
        let hex_dir = tempdir().unwrap();
        std::fs::create_dir_all(hex_dir.path().join(".hex/config")).unwrap();
        std::fs::write(hex_dir.path().join(CONFIG_REL), "other_key = \"x\"\n").unwrap();

        let err = repo_path(hex_dir.path()).expect_err("must be Err when repo key is missing");
        let msg = err.to_string();
        assert!(
            msg.contains(&hex_dir.path().join(CONFIG_REL).display().to_string()),
            "error must name the config file: {msg}"
        );
        assert!(
            msg.contains("repo"),
            "error must mention the missing repo key: {msg}"
        );
    }

    #[test]
    fn nightly_run_returns_err_when_the_lane_exits_nonzero() {
        let hex_dir = tempdir().unwrap();
        let repo = tempdir().unwrap();
        write_lane_script(
            repo.path(),
            "#!/bin/sh\necho \"test-lane: docker daemon not reachable\" >&2\nexit 2\n",
        );
        std::fs::create_dir_all(hex_dir.path().join(".hex/config")).unwrap();
        std::fs::write(
            hex_dir.path().join(CONFIG_REL),
            format!("repo = \"{}\"\n", repo.path().display()),
        )
        .unwrap();

        let err = run_nightly_at(hex_dir.path(), &Ctx::new())
            .expect_err("a non-zero lane exit must be Err");
        let msg = err.to_string();
        assert!(
            msg.contains("exited 2"),
            "error must carry the exit code: {msg}"
        );
        assert!(
            msg.contains("docker daemon not reachable"),
            "error must carry the lane's stderr reason: {msg}"
        );
    }

    #[test]
    fn nightly_run_returns_ok_when_the_lane_exits_zero() {
        let hex_dir = tempdir().unwrap();
        let repo = tempdir().unwrap();
        write_lane_script(
            repo.path(),
            "#!/bin/sh\necho '{\"schema\":\"hex.test-lane.receipt.v1\",\"exit_code\":0}'\nexit 0\n",
        );
        std::fs::create_dir_all(hex_dir.path().join(".hex/config")).unwrap();
        std::fs::write(
            hex_dir.path().join(CONFIG_REL),
            format!("repo = \"{}\"\n", repo.path().display()),
        )
        .unwrap();

        run_nightly_at(hex_dir.path(), &Ctx::new()).expect("exit 0 lane run must be Ok");
    }
}
