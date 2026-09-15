//! `hex-build-cache-guard`, hourly prune-and-alert guard over the BOI
//! shared cargo target directory.
//!
//! Why this exists: the BOI shared cargo target grew to 143,664 entries in
//! `debug/deps`. Gatekeeper walks that directory on every test binary
//! launch, so `syspolicyd` ran at 380% CPU. The instance fix (packed
//! split-debuginfo in `~/.cargo/config.toml`) stops the growth, but nothing
//! bounds the directory. This worker is the bound: every hour it deletes
//! stale `.o` files and fails loudly (S6) if the directory is still too big
//! after pruning.
//!
//! - id `hex::build_cache_guard` cron `0 15 * * * * *` (hourly, offset from
//!   the :00 memory tick)
//!
//! Reads the shared cargo target path from `~/.boi/v2/daemon.toml`
//! (`cargo_target_dir` key). A missing file or key is not an error, BOI may
//! not be installed on every box, it logs one warning and returns Ok.

use anyhow::anyhow;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression, hourly at :15, offset from the :00 memory-index tick.
pub const CRON_HOURLY: &str = "0 15 * * * * *";

/// Above this many remaining entries in `debug/deps` after pruning, the
/// guard fails loudly instead of pruning silently forever.
pub const DEPS_ENTRY_THRESHOLD: usize = 25_000;

/// A `.o` file older than this is stale and safe to delete. Cargo's link
/// step finishes in seconds, so 60 minutes is a wide safety margin.
const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

/// Path to BOI's daemon config, or `None` when there is no home directory
/// (should not happen on a real box, but never worth a panic).
pub fn daemon_toml_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".boi/v2/daemon.toml"))
}

/// Read `cargo_target_dir` out of the BOI daemon config at `path`.
///
/// A missing file is `Ok(None)`, BOI may not be installed on this box.
/// A present file missing the key, or with a non-string value, is also
/// `Ok(None)`, nothing to prune against. A file that fails to parse as
/// TOML is `Err`, a corrupt daemon.toml is loud, per S6.
pub fn read_cargo_target_dir(path: &Path) -> Result<Option<PathBuf>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow!(
                "build-cache-guard: read {} failed: {e}",
                path.display()
            ))
        }
    };
    let value: toml::Value = toml::from_str(&raw)
        .map_err(|e| anyhow!("build-cache-guard: parse {} failed: {e}", path.display()))?;
    Ok(value
        .get("cargo_target_dir")
        .and_then(|v| v.as_str())
        .map(PathBuf::from))
}

/// One guard run's result: how many stale `.o` files it deleted, and how
/// many entries remain in `deps_dir` after the pass.
pub struct GuardSummary {
    pub deleted: usize,
    pub remaining: usize,
    pub deps_dir: PathBuf,
}

/// Delete regular files in `deps_dir` whose name ends in `.o` and whose
/// mtime is older than `STALE_AFTER` relative to `now`, then count what
/// remains. Never recurses, never follows symlinks (a symlink's own
/// `file_type()` is never `is_file()`, so it is skipped, not deleted).
///
/// A missing `deps_dir` is `Ok(None)`, the target hasn't been built yet.
pub fn prune_and_count(deps_dir: &Path, now: SystemTime) -> Result<Option<GuardSummary>> {
    if !deps_dir.exists() {
        return Ok(None);
    }

    let mut deleted = 0usize;
    let entries = std::fs::read_dir(deps_dir).map_err(|e| {
        anyhow!(
            "build-cache-guard: read_dir {} failed: {e}",
            deps_dir.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            anyhow!(
                "build-cache-guard: dir entry in {} failed: {e}",
                deps_dir.display()
            )
        })?;
        let file_type = entry.file_type().map_err(|e| {
            anyhow!(
                "build-cache-guard: file_type for {} failed: {e}",
                entry.path().display()
            )
        })?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        if !name.to_string_lossy().ends_with(".o") {
            continue;
        }
        let metadata = entry.metadata().map_err(|e| {
            anyhow!(
                "build-cache-guard: metadata for {} failed: {e}",
                entry.path().display()
            )
        })?;
        let modified = metadata.modified().map_err(|e| {
            anyhow!(
                "build-cache-guard: mtime for {} failed: {e}",
                entry.path().display()
            )
        })?;
        let age = now
            .duration_since(modified)
            .unwrap_or(Duration::from_secs(0));
        if age > STALE_AFTER {
            std::fs::remove_file(entry.path()).map_err(|e| {
                anyhow!(
                    "build-cache-guard: remove {} failed: {e}",
                    entry.path().display()
                )
            })?;
            deleted += 1;
        }
    }

    // Recount after deletions so `remaining` reflects the directory's real
    // state, not a running tally kept during the delete pass.
    let remaining = std::fs::read_dir(deps_dir)
        .map_err(|e| {
            anyhow!(
                "build-cache-guard: recount read_dir {} failed: {e}",
                deps_dir.display()
            )
        })?
        .count();

    Ok(Some(GuardSummary {
        deleted,
        remaining,
        deps_dir: deps_dir.to_path_buf(),
    }))
}

/// Fail loud (S6) when `summary.remaining` is still above the threshold
/// after pruning. Never a bespoke notifier, the harness failure path turns
/// this `Err` into telemetry and an alert.
pub fn check_threshold(summary: &GuardSummary) -> Result<()> {
    if summary.remaining > DEPS_ENTRY_THRESHOLD {
        anyhow::bail!(
            "build-cache-guard: {} has {} entries, above threshold {}",
            summary.deps_dir.display(),
            summary.remaining,
            DEPS_ENTRY_THRESHOLD
        );
    }
    Ok(())
}

/// One guard run against an explicit `daemon_toml` path and clock. Separated
/// from the handler so tests control both the config path and time.
///
/// Missing `cargo_target_dir` (file or key absent) logs one warning and
/// returns `Ok(None)`. A missing `debug/deps` dir also returns `Ok(None)`,
/// with its own log line. Otherwise logs one summary line and checks the
/// threshold.
pub fn run_guard_at(daemon_toml: &Path, now: SystemTime) -> Result<Option<GuardSummary>> {
    let target_dir = match read_cargo_target_dir(daemon_toml)? {
        Some(dir) => dir,
        None => {
            eprintln!(
                "[build-cache-guard] no cargo_target_dir in {} (file or key missing); skipping",
                daemon_toml.display()
            );
            return Ok(None);
        }
    };

    let deps_dir = target_dir.join("debug").join("deps");
    let summary = match prune_and_count(&deps_dir, now)? {
        Some(summary) => summary,
        None => {
            eprintln!(
                "[build-cache-guard] {} does not exist; nothing to prune",
                deps_dir.display()
            );
            return Ok(None);
        }
    };

    eprintln!(
        "[build-cache-guard] deleted={} remaining={} threshold={} dir={}",
        summary.deleted,
        summary.remaining,
        DEPS_ENTRY_THRESHOLD,
        summary.deps_dir.display()
    );

    check_threshold(&summary)?;
    Ok(Some(summary))
}

/// Handler glue: resolve the real daemon.toml path and clock, delegate to
/// `run_guard_at`. No home dir found is not an error, logs one warning.
fn run_guard(_e: Event, _ctx: Ctx) -> Result<()> {
    match daemon_toml_path() {
        Some(path) => run_guard_at(&path, SystemTime::now()).map(|_| ()),
        None => {
            eprintln!("[build-cache-guard] could not resolve home dir; skipping");
            Ok(())
        }
    }
}

/// Build the `hex-build-cache-guard` worker.
pub fn worker() -> Worker {
    Worker::new("hex-build-cache-guard").on_cron_named("hourly", CRON_HOURLY, run_guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    fn age_file(path: &Path, age: Duration) {
        let f = File::open(path).expect("open file to age it");
        let modified = SystemTime::now()
            .checked_sub(age)
            .expect("age fits in SystemTime");
        f.set_modified(modified).expect("set_modified");
    }

    #[test]
    fn stale_o_file_is_deleted() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("foo.o");
        File::create(&f).unwrap();
        age_file(&f, Duration::from_secs(2 * 60 * 60));

        let summary = prune_and_count(dir.path(), SystemTime::now())
            .unwrap()
            .expect("deps dir exists");
        assert_eq!(summary.deleted, 1);
        assert_eq!(summary.remaining, 0);
        assert!(!f.exists());
    }

    #[test]
    fn fresh_o_file_is_kept() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("bar.o");
        File::create(&f).unwrap();
        age_file(&f, Duration::from_secs(10 * 60));

        let summary = prune_and_count(dir.path(), SystemTime::now())
            .unwrap()
            .expect("deps dir exists");
        assert_eq!(summary.deleted, 0);
        assert_eq!(summary.remaining, 1);
        assert!(f.exists());
    }

    #[test]
    fn non_o_files_kept_regardless_of_age() {
        let dir = tempdir().unwrap();
        let rlib = dir.path().join("stale.rlib");
        let dfile = dir.path().join("stale.d");
        File::create(&rlib).unwrap();
        File::create(&dfile).unwrap();
        age_file(&rlib, Duration::from_secs(3 * 60 * 60));
        age_file(&dfile, Duration::from_secs(3 * 60 * 60));

        let summary = prune_and_count(dir.path(), SystemTime::now())
            .unwrap()
            .expect("deps dir exists");
        assert_eq!(summary.deleted, 0);
        assert_eq!(summary.remaining, 2);
        assert!(rlib.exists());
        assert!(dfile.exists());
    }

    #[test]
    fn subdirectory_named_dot_o_is_kept() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("weird.o");
        std::fs::create_dir(&sub).unwrap();

        let summary = prune_and_count(dir.path(), SystemTime::now())
            .unwrap()
            .expect("deps dir exists");
        assert_eq!(summary.deleted, 0);
        assert_eq!(summary.remaining, 1);
        assert!(sub.exists());
    }

    #[test]
    fn missing_deps_dir_returns_none() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let result = prune_and_count(&missing, SystemTime::now()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn check_threshold_ok_at_exact_threshold() {
        let summary = GuardSummary {
            deleted: 0,
            remaining: DEPS_ENTRY_THRESHOLD,
            deps_dir: PathBuf::from("/tmp/deps"),
        };
        assert!(check_threshold(&summary).is_ok());
    }

    #[test]
    fn check_threshold_err_above_threshold() {
        let count = DEPS_ENTRY_THRESHOLD + 1;
        let summary = GuardSummary {
            deleted: 0,
            remaining: count,
            deps_dir: PathBuf::from("/tmp/deps"),
        };
        let err = check_threshold(&summary).expect_err("must fail above threshold");
        let msg = err.to_string();
        assert!(msg.contains(&count.to_string()), "{msg}");
        assert!(msg.contains("/tmp/deps"), "{msg}");
        assert!(msg.contains(&DEPS_ENTRY_THRESHOLD.to_string()), "{msg}");
    }

    #[test]
    fn read_cargo_target_dir_missing_file_is_ok_none() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("daemon.toml");
        let result = read_cargo_target_dir(&missing).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_cargo_target_dir_with_key_returns_path() {
        let dir = tempdir().unwrap();
        let toml_path = dir.path().join("daemon.toml");
        std::fs::write(&toml_path, "cargo_target_dir = \"/some/path\"\n").unwrap();
        let result = read_cargo_target_dir(&toml_path).unwrap();
        assert_eq!(result, Some(PathBuf::from("/some/path")));
    }

    #[test]
    fn read_cargo_target_dir_without_key_is_ok_none() {
        let dir = tempdir().unwrap();
        let toml_path = dir.path().join("daemon.toml");
        std::fs::write(&toml_path, "other_key = \"x\"\n").unwrap();
        let result = read_cargo_target_dir(&toml_path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_cargo_target_dir_invalid_toml_is_err() {
        let dir = tempdir().unwrap();
        let toml_path = dir.path().join("daemon.toml");
        std::fs::write(&toml_path, "this is not [[[ valid toml").unwrap();
        let result = read_cargo_target_dir(&toml_path);
        assert!(result.is_err());
    }

    #[test]
    fn run_guard_at_missing_daemon_toml_returns_ok_none() {
        let dir = tempdir().unwrap();
        let toml_path = dir.path().join("daemon.toml");
        let result = run_guard_at(&toml_path, SystemTime::now()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn run_guard_at_target_without_debug_deps_returns_ok_none() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        let toml_path = dir.path().join("daemon.toml");
        std::fs::write(
            &toml_path,
            format!("cargo_target_dir = \"{}\"\n", target.display()),
        )
        .unwrap();

        let result = run_guard_at(&toml_path, SystemTime::now()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn run_guard_at_populated_deps_dir_returns_summary() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        let deps = target.join("debug").join("deps");
        std::fs::create_dir_all(&deps).unwrap();

        let stale = deps.join("stale.o");
        File::create(&stale).unwrap();
        age_file(&stale, Duration::from_secs(2 * 60 * 60));

        let fresh = deps.join("fresh.o");
        File::create(&fresh).unwrap();
        age_file(&fresh, Duration::from_secs(5 * 60));

        let toml_path = dir.path().join("daemon.toml");
        std::fs::write(
            &toml_path,
            format!("cargo_target_dir = \"{}\"\n", target.display()),
        )
        .unwrap();

        let summary = run_guard_at(&toml_path, SystemTime::now())
            .unwrap()
            .expect("deps dir populated");
        assert_eq!(summary.deleted, 1);
        assert_eq!(summary.remaining, 1);
        assert!(!stale.exists());
        assert!(fresh.exists());
    }
}
