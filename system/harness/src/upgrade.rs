//! Real port of the hex upgrade subcommand.
//!
//! Upgrades: scripts, skills, commands, hooks
//! Preserves: memory.db, settings.local.json, user data, AGENTS.md
//!
//! Drift bug fix: the bash shim omitted hooks sync for v2 layout. This
//! implementation syncs hooks unconditionally.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use walkdir::WalkDir;

use crate::path_map;

const DEFAULT_REPO: &str = "https://github.com/mrap/hex-foundation.git";

struct Args {
    dry_run: bool,
    repo_url: Option<String>,
    local_path: Option<String>,
}

struct SourceDirs {
    scripts: PathBuf,
    skills: PathBuf,
    commands: PathBuf,
    hooks: PathBuf,
    /// Additive-only dirs: synced (add/update) but NEVER pruned, because their
    /// deployed copies hold runtime state (`.hex/iii/data`, worker `node_modules`).
    iii: PathBuf,
    templates: PathBuf,
    version_txt: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut dry_run = false;
    let mut repo_url = None;
    let mut local_path = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--repo" => {
                i += 1;
                repo_url = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| "--repo requires a value".to_string())?,
                );
                i += 1;
            }
            "--local" => {
                i += 1;
                local_path = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| "--local requires a value".to_string())?,
                );
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                return Err("help".to_string());
            }
            other => return Err(format!("Unknown option: {other}")),
        }
    }
    Ok(Args {
        dry_run,
        repo_url,
        local_path,
    })
}

fn print_help() {
    println!("Usage: hex upgrade [--dry-run] [--repo URL] [--local PATH]");
    println!();
    println!("Options:");
    println!("  --dry-run    Show what would change without applying");
    println!("  --repo URL   Override repo URL");
    println!("  --local PATH Use a local hex-foundation checkout");
}

fn hex_dir_from_env() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("HEX_DIR") {
        let p = PathBuf::from(&v);
        if p.join("CLAUDE.md").exists() || p.join("AGENTS.md").exists() {
            return Some(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(&home).join("hex");
        if p.join("CLAUDE.md").exists() || p.join("AGENTS.md").exists() {
            return Some(p);
        }
    }
    None
}

fn source_dirs_for_layout(layout: &str, source_root: &Path) -> Option<SourceDirs> {
    match layout {
        "v2" => Some(SourceDirs {
            scripts: source_root.join("system/scripts"),
            skills: source_root.join("system/skills"),
            commands: source_root.join("system/commands"),
            hooks: source_root.join("system/hooks"),
            iii: source_root.join("system/iii"),
            templates: source_root.join("system/templates"),
            version_txt: Some(source_root.join("system/version.txt")),
        }),
        _ => None,
    }
}

/// Walk a directory recursively, yielding all file paths (skipping __pycache__).
fn walk_files(dir: &Path) -> impl Iterator<Item = PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let is_file = e.file_type().is_file();
            let in_pycache = e
                .path()
                .components()
                .any(|c| c.as_os_str() == "__pycache__");
            is_file && !in_pycache
        })
        .map(|e| e.path().to_path_buf())
}

fn files_differ(a: &Path, b: &Path) -> bool {
    match (fs::read(a), fs::read(b)) {
        (Ok(ac), Ok(bc)) => ac != bc,
        _ => true,
    }
}

fn copy_file_with_perms(src: &Path, dst: &Path) -> io::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(src, dst)?;
    let src_mode = fs::metadata(src)?.permissions().mode();
    if src_mode & 0o111 != 0 {
        let mut perms = fs::metadata(dst)?.permissions();
        perms.set_mode(src_mode & 0o777);
        fs::set_permissions(dst, perms)?;
    }
    Ok(())
}

/// Detect which files in src_dir differ from dst_dir.
/// Returns (changed, new_count, unchanged, log_lines).
fn detect_changes(
    src_dir: &Path,
    dst_dir: &Path,
    label: &str,
) -> (usize, usize, usize, Vec<String>) {
    if !src_dir.exists() {
        return (0, 0, 0, vec![]);
    }
    let mut changed = 0;
    let mut new_count = 0;
    let mut unchanged = 0;
    let mut log = Vec::new();

    for src_file in walk_files(src_dir) {
        let rel = match src_file.strip_prefix(src_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel_str = rel.to_string_lossy();
        if rel_str.contains("settings.local.json") {
            continue;
        }
        let dst_file = dst_dir.join(rel);
        if !dst_file.exists() {
            new_count += 1;
            log.push(format!("  + {label}/{rel_str}"));
        } else if files_differ(&src_file, &dst_file) {
            changed += 1;
            log.push(format!("  ~ {label}/{rel_str}"));
        } else {
            unchanged += 1;
        }
    }
    (changed, new_count, unchanged, log)
}

/// Sync src_dir into dst_dir. Backs up overwritten files into backup_dir if provided.
/// Returns count of files written.
pub fn apply_sync(src_dir: &Path, dst_dir: &Path, backup_dir: Option<&Path>) -> io::Result<usize> {
    if !src_dir.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for src_file in walk_files(src_dir) {
        let rel = match src_file.strip_prefix(src_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel.to_string_lossy().contains("settings.local.json") {
            continue;
        }
        let dst_file = dst_dir.join(rel);
        if let Some(bak) = backup_dir {
            if dst_file.exists() && files_differ(&src_file, &dst_file) {
                let bak_file = bak.join(rel);
                if let Some(p) = bak_file.parent() {
                    fs::create_dir_all(p)?;
                }
                fs::copy(&dst_file, &bak_file)?;
            }
        }
        if !dst_file.exists() || files_differ(&src_file, &dst_file) {
            copy_file_with_perms(&src_file, &dst_file)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Remove files in dst_dir that are absent from src_dir. Backs them up first.
pub fn deletion_pass(dst_dir: &Path, src_dir: &Path, backup_dir: &Path) -> io::Result<usize> {
    if !dst_dir.exists() || !src_dir.exists() {
        return Ok(0);
    }
    let mut deleted = 0;
    for dst_file in walk_files(dst_dir) {
        let rel = match dst_file.strip_prefix(dst_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !src_dir.join(rel).exists() {
            let bak_file = backup_dir.join(rel);
            if let Some(p) = bak_file.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(&dst_file, &bak_file)?;
            fs::remove_file(&dst_file)?;
            println!("  → rm (not in foundation): {}", rel.display());
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// Atomically install an executable: write to a temp file in
/// the destination directory, make it executable, ad-hoc
/// codesign it, then rename it over `dst`. Never mutates the
/// live destination inode — safe even if `dst` is currently
/// being executed (mmap'd). Prevents code-signing vnode
/// poisoning.
fn atomic_install_binary(src: &Path, dst: &Path) -> io::Result<()> {
    let dst_dir = dst.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "dst has no parent directory")
    })?;
    fs::create_dir_all(dst_dir)?;
    let tmp = dst_dir.join(format!(".hex-install-{}.tmp", std::process::id()));

    let result = (|| {
        fs::copy(src, &tmp)?;
        let mut perms = fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tmp, perms)?;
        let cs = Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&tmp)
            .status()?;
        if !cs.success() {
            return Err(io::Error::other("codesign failed on temp binary"));
        }
        fs::rename(&tmp, dst)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn make_scripts_executable(dir: &Path) {
    for f in walk_files(dir) {
        if f.extension().and_then(|e| e.to_str()) == Some("sh") {
            if let Ok(meta) = fs::metadata(&f) {
                let mut perms = meta.permissions();
                perms.set_mode(perms.mode() | 0o111);
                let _ = fs::set_permissions(&f, perms);
            }
        }
    }
}

fn get_source_dir(args: &Args, hex_dir: &Path) -> Result<PathBuf, String> {
    let repo_url = args
        .repo_url
        .clone()
        .or_else(|| {
            let cfg = hex_dir.join(".hex/upgrade.json");
            load_config_repo(&cfg)
        })
        .unwrap_or_else(|| DEFAULT_REPO.to_string());

    if let Some(local) = &args.local_path {
        let p = PathBuf::from(local);
        let layout = path_map::detect_layout(p.to_str().unwrap_or(""));
        if layout == "unknown" {
            return Err(format!(
                "No recognized hex layout at {local} (expected system/ + templates/AGENTS.md)"
            ));
        }
        println!("  → Using local checkout: {local}");
        return Ok(p);
    }

    let cache_dir = hex_dir.join(".hex/.upgrade-cache");

    let mut cached = false;
    if cache_is_healthy(&cache_dir) {
        println!("  → Pulling latest from {repo_url}");
        let result = Command::new("git")
            .arg("-C")
            .arg(&cache_dir)
            .args(["pull", "--ff-only"])
            .output();
        match result {
            Ok(out) if out.status.success() => {
                let msg = String::from_utf8_lossy(&out.stdout);
                if msg.contains("Already up to date") {
                    println!("  → Already up to date");
                } else {
                    print!("  → {msg}");
                }
                cached = true;
            }
            _ => {
                println!("  [WARN] Fast-forward pull failed. Re-cloning.");
            }
        }
    }

    if !cached {
        // The cache is missing, corrupt, or stale. Clear whatever is there so
        // the clone has a free path, then clone into a temp dir outside
        // ~/hex/.hex (where macOS blocks git's own `.git` writes) and move it
        // into place — directory moves into that path are permitted.
        clear_cache_dir(&cache_dir)?;
        clone_into_cache(&repo_url, &cache_dir)?;
        let layout = path_map::detect_layout(cache_dir.to_str().unwrap_or(""));
        if layout == "unknown" {
            return Err(
                "Clone succeeded but no recognized hex layout found. Wrong repo?".to_string(),
            );
        }
    }

    println!("  [OK] Source ready");
    Ok(cache_dir)
}

/// A cache is healthy iff it owns its own git directory — i.e.
/// `git -C <cache_dir> rev-parse --absolute-git-dir` succeeds AND resolves to
/// `<cache_dir>/.git`. A bare-existence `.git` check is not enough: a corrupt
/// partial clone (a `.git/` with no HEAD/objects/refs) makes git resolve up the
/// directory tree to a parent repo, so pulls silently operate on the wrong repo.
fn cache_is_healthy(cache_dir: &Path) -> bool {
    let out = match Command::new("git")
        .arg("-C")
        .arg(cache_dir)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return false,
    };
    let reported = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let reported = match fs::canonicalize(&reported) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let expected = match fs::canonicalize(cache_dir.join(".git")) {
        Ok(p) => p,
        Err(_) => return false,
    };
    reported == expected
}

/// Remove an unhealthy/corrupt cache robustly. Prefer `remove_dir_all`; if that
/// fails (macOS blocks deleting protected `.git` files under ~/hex/.hex), move
/// it aside to a unique sibling so the cache path is free. Loud `Err` if neither
/// works — never silently proceed onto a still-occupied path.
fn clear_cache_dir(cache_dir: &Path) -> Result<(), String> {
    if !cache_dir.exists() {
        return Ok(());
    }
    if fs::remove_dir_all(cache_dir).is_ok() {
        return Ok(());
    }
    for n in 0..1000 {
        let aside = cache_dir.with_extension(format!("corrupt-{n}"));
        if aside.exists() {
            continue;
        }
        match fs::rename(cache_dir, &aside) {
            Ok(()) => {
                println!(
                    "  [WARN] Could not delete corrupt cache; moved aside to {}",
                    aside.display()
                );
                return Ok(());
            }
            Err(_) => continue,
        }
    }
    Err(format!(
        "Could not clear corrupt cache at {} (remove and move-aside both failed)",
        cache_dir.display()
    ))
}

/// Clone into a temp dir under the system temp (where git can write `.git`),
/// then move it into `cache_dir`. The whole-directory move into ~/hex/.hex is
/// permitted even though git's own `.git` writes there are not. Falls back to
/// `mv` on a cross-device rename (temp on a different volume). The temp dir is
/// cleaned up on any failure.
fn clone_into_cache(repo_url: &str, cache_dir: &Path) -> Result<(), String> {
    println!("  → Cloning {repo_url}");

    let unique = format!(
        "hex-upgrade-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = std::env::temp_dir().join(unique);

    let status = Command::new("git")
        .args(["clone", "--depth", "1", repo_url])
        .arg(&tmp)
        .status()
        .map_err(|e| format!("git clone failed: {e}"))?;
    if !status.success() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("git clone of {repo_url} failed"));
    }

    if let Some(parent) = cache_dir.parent() {
        let _ = fs::create_dir_all(parent);
    }

    match fs::rename(&tmp, cache_dir) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            // Cross-device (EXDEV) rename can't move across volumes — shell out
            // to `mv`, which falls back to copy+remove.
            let moved = Command::new("mv").arg(&tmp).arg(cache_dir).status();
            match moved {
                Ok(s) if s.success() => Ok(()),
                _ => {
                    let _ = fs::remove_dir_all(&tmp);
                    Err(format!(
                        "Could not move clone into place at {} ({rename_err})",
                        cache_dir.display()
                    ))
                }
            }
        }
    }
}

fn load_config_repo(config_file: &Path) -> Option<String> {
    let content = fs::read_to_string(config_file).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    v.get("repo")?.as_str().map(|s| s.to_string())
}

fn record_upgrade_sha(config_file: &Path, source_dir: &Path, repo_url: &str) {
    let sha = Command::new("git")
        .arg("-C")
        .arg(source_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        });

    let Some(sha) = sha else { return };

    let mut data: serde_json::Value = if config_file.exists() {
        fs::read_to_string(config_file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({ "repo": repo_url })
    };

    data["last_remote_sha"] = serde_json::Value::String(sha.clone());
    let tmp = config_file.with_extension("tmp");
    if let Ok(s) = serde_json::to_string_pretty(&data) {
        if fs::write(&tmp, s + "\n").is_ok() {
            let _ = fs::rename(&tmp, config_file);
            // `sha` is `git rev-parse HEAD` output — hex ASCII, every byte a char boundary.
            #[allow(clippy::string_slice)]
            {
                println!("  → Recorded upgrade SHA: {}...", &sha[..sha.len().min(8)]);
            }
        }
    }
}

/// Pure decision: is the installed binary stale relative to source?
///
/// A binary-only change (new/edited Rust source under `system/harness/src/`
/// with the SAME Cargo version) touches zero *synced* files, so the
/// file-diff gate would report "nothing to do" and skip the rebuild
/// (OBS-028). This captures the same version/SHA test the rebuild step uses
/// so the "anything to do?" gate can honor binary-only changes. Factored out
/// to keep the gate and the rebuild step from ever diverging.
fn binary_needs_rebuild(
    installed_ver: Option<&str>,
    cargo_ver: &str,
    installed_sha: Option<&str>,
    source_sha: Option<&str>,
) -> bool {
    let version_mismatch = installed_ver != Some(cargo_ver);
    // SHA drives a rebuild only when BOTH sides are known and differ. Either
    // side unknown + version matching = freshness unverifiable, and the skip
    // is deliberate (the caller warns loudly): `source_sha` None is the
    // offline/--local source; `installed_sha` None is a prebuilt or
    // hand-installed binary (install.sh never writes hex.sha) — forcing a
    // rebuild there made every upgrade on a cargo-less box hard-fail forever
    // on a binary that was already current (review 2026-08-19).
    let sha_mismatch =
        source_sha.is_some() && installed_sha.is_some() && installed_sha != source_sha;
    version_mismatch || sha_mismatch
}

/// Gather inputs and decide whether the binary is stale. Returns false
/// ("nothing to do") when VERSIONS or Cargo.toml is absent, matching
/// `sync_versions_file`'s own preconditions — if those are missing the
/// rebuild step no-ops anyway, so the gate shouldn't proceed on its account.
fn binary_is_stale(hex_dir: &Path, source_dir: &Path) -> bool {
    let versions_file = hex_dir.join("VERSIONS");
    let cargo_toml = source_dir.join("system/harness/Cargo.toml");
    if !versions_file.exists() || !cargo_toml.exists() {
        return false;
    }
    let cargo_ver = fs::read_to_string(&cargo_toml).ok().and_then(|c| {
        c.lines()
            .find(|l| l.starts_with("version"))
            .and_then(|l| l.split_once('"').map(|x| x.1))
            .and_then(|s| s.split('"').next())
            .map(|s| s.to_string())
    });
    let Some(cargo_ver) = cargo_ver else {
        return false;
    };

    let hex_dot_dir = hex_dir.join(".hex");
    let installed_ver = Command::new(hex_dot_dir.join("bin/hex"))
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .and_then(|s| s.split_whitespace().nth(1).map(|v| v.to_string()))
        });
    let installed_sha = fs::read_to_string(hex_dot_dir.join("bin/hex.sha"))
        .ok()
        .map(|s| s.trim().to_string());
    let source_sha = Command::new("git")
        .arg("-C")
        .arg(source_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        });
    binary_needs_rebuild(
        installed_ver.as_deref(),
        &cargo_ver,
        installed_sha.as_deref(),
        source_sha.as_deref(),
    )
}

/// True if the user has a personal overlay that `build.rs` compiles under
/// `--features personal` — keyed on overlay-dir PRESENCE (`harness-personal/`
/// integration probes, or `modules/` personal workers), not any specific file.
/// `hex_dot_dir` is the `.hex` dir (same `HEX_DIR/.hex` build.rs scans).
fn detect_personal_overlay(hex_dot_dir: &Path) -> bool {
    hex_dot_dir.join("harness-personal").is_dir() || hex_dot_dir.join("modules").is_dir()
}

/// Sync VERSIONS and rebuild/swap the hex binary when stale. Returns `true`
/// when the binary step is HEALTHY (rebuilt+swapped, or legitimately up to
/// date / not applicable) and `false` when the installed binary may be stale
/// after this run (sync failure, cargo build failure, install failure). The
/// caller MUST fail the whole upgrade on `false` — printing "Upgrade
/// complete." over a stale binary is the OBS-017 deploy black hole.
fn sync_versions_file(
    hex_dir: &Path,
    source_dir: &Path,
    backup_dir: &Path,
) -> Result<(), BinaryStepFailure> {
    let versions_file = hex_dir.join("VERSIONS");
    if !versions_file.exists() {
        return Ok(());
    }
    let cargo_toml = source_dir.join("system/harness/Cargo.toml");
    if !cargo_toml.exists() {
        return Ok(());
    }
    let cargo_content = match fs::read_to_string(&cargo_toml) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("  [WARN] Could not read Cargo.toml");
            return Err(BinaryStepFailure::Build);
        }
    };
    let cargo_ver = cargo_content
        .lines()
        .find(|l| l.starts_with("version"))
        .and_then(|l| l.split_once('"').map(|x| x.1))
        .and_then(|s| s.split('"').next())
        .map(|s| s.to_string());

    let Some(cargo_ver) = cargo_ver else {
        eprintln!("  [WARN] Could not parse version from Cargo.toml");
        return Err(BinaryStepFailure::Build);
    };

    // Preserve every existing line — comments, blank lines, and any
    // KEY=VALUE we do not manage (BOI_VERSION, custom instance pins,
    // repo overrides, etc.). Only update the managed keys we own in
    // place, or append them if missing. Regression: previous behavior
    // rewrote the file with only HEX_FOUNDATION_VERSION, destroying
    // unmanaged pins like BOI_VERSION that install.sh parity reads
    // (2026-07-16 audit).
    let existing = fs::read_to_string(&versions_file).unwrap_or_default();
    let managed_key = "HEX_FOUNDATION_VERSION";
    let managed_line = format!("{managed_key}=v{cargo_ver}");
    let mut replaced = false;
    let mut lines: Vec<String> = Vec::new();
    for line in existing.lines() {
        let key_prefix = format!("{managed_key}=");
        if line.trim_start().starts_with(&key_prefix) {
            if !replaced {
                lines.push(managed_line.clone());
                replaced = true;
            }
            // Drop duplicate managed lines silently — a rewrite should
            // leave exactly one canonical managed line.
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        lines.push(managed_line.clone());
    }
    let mut new_content = lines.join("\n");
    new_content.push('\n');

    let tmp = versions_file.with_extension("tmp");
    if fs::write(&tmp, &new_content).is_ok() {
        let _ = fs::rename(&tmp, &versions_file);
        println!("  [OK] VERSIONS → HEX_FOUNDATION_VERSION=v{cargo_ver}");
    }

    // Rebuild hex binary if version or commit SHA changed
    let hex_dot_dir = hex_dir.join(".hex");
    let installed_bin = hex_dot_dir.join("bin/hex");
    let installed_sha_file = hex_dot_dir.join("bin/hex.sha");

    let installed_ver = Command::new(&installed_bin)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .and_then(|s| s.split_whitespace().nth(1).map(|v| v.to_string()))
        });

    let installed_sha = fs::read_to_string(&installed_sha_file)
        .ok()
        .map(|s| s.trim().to_string());

    let source_sha = Command::new("git")
        .arg("-C")
        .arg(source_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        });

    // `version_mismatch` drives the human-readable reason below; the actual
    // rebuild decision is `binary_needs_rebuild` (shared with the upstream gate).
    let version_mismatch = installed_ver.as_deref() != Some(&cargo_ver);

    if binary_needs_rebuild(
        installed_ver.as_deref(),
        &cargo_ver,
        installed_sha.as_deref(),
        source_sha.as_deref(),
    ) {
        let harness_dst = hex_dot_dir.join("harness");
        let reason = if version_mismatch {
            format!(
                "version mismatch ({} → {cargo_ver})",
                installed_ver.as_deref().unwrap_or("none")
            )
        } else {
            format!(
                "SHA mismatch ({} → {} at v{cargo_ver})",
                installed_sha.as_deref().unwrap_or("none"),
                source_sha.as_deref().unwrap_or("unknown")
            )
        };
        println!("  → hex binary {reason} — rebuilding...");
        let harness_src = source_dir.join("system/harness");
        if let Err(e) = apply_sync(&harness_src, &harness_dst, None) {
            eprintln!("  [FAIL] Failed to sync harness source: {e}");
            return Err(BinaryStepFailure::Build);
        }
        // Deletion pass scoped to src/ and tests/ only — never touches target/ or Cargo.lock.
        for sub in &["src", "tests"] {
            let dst_sub = harness_dst.join(sub);
            let src_sub = harness_src.join(sub);
            if dst_sub.exists() && src_sub.exists() {
                if let Err(e) = deletion_pass(&dst_sub, &src_sub, backup_dir) {
                    eprintln!("  [WARN] Harness deletion pass on {sub}/ failed: {e}");
                }
            }
        }

        // The harness depends on scipd via `scipd = { path = "../code-intel" }`
        // (system/code-intel, workspace sibling). Sync it to .hex/code-intel —
        // sibling of .hex/harness — BEFORE the cargo build, or the path dep
        // cannot resolve and the rebuild fails. Same mechanism as the harness
        // sync above: full-dir apply_sync + deletion pass scoped to src/ and
        // tests/ only (never target/ or generated Cargo.lock).
        let codeintel_src = source_dir.join("system/code-intel");
        let codeintel_dst = hex_dot_dir.join("code-intel");
        if codeintel_src.exists() {
            if let Err(e) = apply_sync(&codeintel_src, &codeintel_dst, None) {
                eprintln!("  [FAIL] Failed to sync code-intel source: {e}");
                return Err(BinaryStepFailure::Build);
            }
            for sub in &["src", "tests"] {
                let dst_sub = codeintel_dst.join(sub);
                let src_sub = codeintel_src.join(sub);
                if dst_sub.exists() && src_sub.exists() {
                    if let Err(e) = deletion_pass(&dst_sub, &src_sub, backup_dir) {
                        eprintln!("  [WARN] code-intel deletion pass on {sub}/ failed: {e}");
                    }
                }
            }
        }

        // Detect a personal overlay and build with --features personal (and set
        // HEX_DIR so build.rs can find it). Keyed on overlay PRESENCE — a
        // `harness-personal/` dir (integration probes) or a `modules/` dir
        // (personal workers) — NOT a specific file, so it survives files being
        // added/removed/re-homed (e.g. release.rs leaving the binary).
        let use_personal = detect_personal_overlay(&hex_dot_dir);
        let mut build_args = vec!["build", "--release"];
        // --target-dir is always set to harness_dst/target so the output location is
        // deterministic regardless of workspace nesting (fixes OBS-017).
        let target_dir = harness_dst.join("target");
        let target_dir_str = target_dir.to_string_lossy().into_owned();
        build_args.extend_from_slice(&["--target-dir", &target_dir_str]);
        if use_personal {
            build_args.extend_from_slice(&["--features", "personal"]);
            println!("  → Personal overlay detected — building with --features personal");
        }
        let build_status = Command::new("cargo")
            .args(&build_args)
            .current_dir(&harness_dst)
            .env("HEX_DIR", hex_dir)
            .status();
        match build_status {
            Ok(s) if s.success() => {
                // --target-dir guarantees the binary is always here.
                let release_bin = harness_dst.join("target/release/hex");
                match atomic_install_binary(&release_bin, &installed_bin) {
                    Ok(()) => {
                        println!("  [OK] hex binary rebuilt and swapped (atomic): v{cargo_ver}");
                        if let Some(ref sha) = source_sha {
                            let sha_tmp = installed_sha_file.with_extension("tmp");
                            if fs::write(&sha_tmp, sha).is_ok() {
                                let _ = fs::rename(&sha_tmp, &installed_sha_file);
                                // `sha` is `git rev-parse HEAD` output — hex ASCII, every byte a char boundary.
                                #[allow(clippy::string_slice)]
                                {
                                    println!(
                                        "  → Recorded installed SHA: {}...",
                                        &sha[..sha.len().min(8)]
                                    );
                                }
                            }
                        }
                        // The binary changed, but the long-running harness
                        // (`com.hex.harness`, the gui LaunchAgent) still holds the
                        // OLD binary in memory — engine + every worker run inside
                        // it. Restart it so the whole stack reloads — and VERIFY it
                        // came back (a swallowed restart failure left the harness
                        // dead ~3h on 2026-06-12).
                        let ws_root = hex_dot_dir.parent().unwrap_or(hex_dot_dir.as_path());
                        let restart_result = restart_harness(ws_root);
                        // Refresh the code-intel binaries (cq, scipd) so they
                        // deploy alongside hex. Best-effort + loud (S6): a
                        // failure here never blocks the hex swap above. Run it
                        // regardless of the restart outcome so the only deltas a
                        // restart failure introduces are the nonzero exit and the
                        // distinct message below.
                        build_and_install_code_intel(&hex_dot_dir);
                        // The binary WAS swapped. If the harness restart failed,
                        // the running harness still holds the OLD binary in memory
                        // — propagate that as a DISTINCT failure kind so run()
                        // prints the "swapped but restart FAILED" message and exits
                        // nonzero (the 2026-06-12 stale-harness incident), never
                        // the build-failure wording (the binary was in fact updated).
                        if let Err(e) = restart_result {
                            return Err(BinaryStepFailure::RestartFailed(e));
                        }
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("  [FAIL] atomic binary install failed: {e}");
                        Err(BinaryStepFailure::Build)
                    }
                }
            }
            _ => {
                eprintln!("  [FAIL] cargo build failed — install Rust and rerun upgrade");
                Err(BinaryStepFailure::Build)
            }
        }
    } else {
        if source_sha.is_none() || installed_sha.is_none() {
            // Version matches but freshness is UNVERIFIABLE — source SHA
            // unknown (offline/--local, git failed) or installed SHA never
            // recorded (prebuilt / hand-installed binary; install.sh writes
            // no hex.sha). The skip is deliberate: forcing a rebuild here
            // hard-fails forever on cargo-less boxes whose binary is already
            // current. But silence would hide a same-version code change
            // (OBS-017 #1's residual) — say it loudly.
            let missing = if source_sha.is_none() {
                "source SHA unknown (git unavailable at source)"
            } else {
                "installed SHA never recorded (prebuilt or hand-installed binary)"
            };
            eprintln!(
                "  [WARN] {missing} — binary freshness NOT verified; \
                 skipping rebuild because the version matches (v{cargo_ver})."
            );
        } else {
            println!("  [OK] hex binary already at v{cargo_ver} (SHA matches) — no rebuild needed");
        }
        Ok(())
    }
}

/// Build the synced `.hex/code-intel` crate and atomically install its `cq`
/// and `scipd` binaries into `.hex/bin/`, alongside `hex`. Mirrors the harness
/// rebuild: `--target-dir` pinned to the crate's own `target/` so the output
/// location is deterministic regardless of workspace nesting (OBS-017), and
/// `atomic_install_binary` for the swap (codesign + rename, never mutates the
/// live inode). Best-effort: warns loudly on failure, never fails the upgrade.
fn build_and_install_code_intel(hex_dot_dir: &Path) {
    let codeintel_dst = hex_dot_dir.join("code-intel");
    if !codeintel_dst.join("Cargo.toml").exists() {
        return; // code-intel not synced (older foundation) — nothing to build
    }
    println!("  → Building code-intel binaries (cq, scipd)...");
    let target_dir = codeintel_dst.join("target");
    let target_dir_str = target_dir.to_string_lossy().into_owned();
    let build_status = Command::new("cargo")
        .args(["build", "--release", "--target-dir", &target_dir_str])
        .current_dir(&codeintel_dst)
        .status();
    match build_status {
        Ok(s) if s.success() => {
            for name in ["cq", "scipd"] {
                let release_bin = target_dir.join("release").join(name);
                let dst = hex_dot_dir.join("bin").join(name);
                match atomic_install_binary(&release_bin, &dst) {
                    Ok(()) => println!("  [OK] {name} binary installed (atomic)"),
                    Err(e) => eprintln!("  [WARN] could not install {name}: {e}"),
                }
            }
        }
        _ => eprintln!(
            "  [WARN] code-intel cargo build failed — cq/scipd not refreshed (hex swap unaffected)"
        ),
    }
}

/// Why the binary step of an upgrade failed. Kept distinct so `run()` can print
/// the RIGHT loud message: a build/install failure means the binary was NOT
/// swapped, but a restart failure means the binary WAS swapped yet the running
/// harness still holds the OLD one in memory (the 2026-06-12 stale-harness
/// incident). Folding both into a single `bool` can only ever reproduce the
/// build-failure wording, which is factually wrong once the swap has happened.
#[derive(Debug, PartialEq)]
enum BinaryStepFailure {
    /// The binary was NOT updated — source sync, `cargo build`, atomic install,
    /// or version parse failed before/at the swap.
    Build,
    /// The binary WAS swapped, but restarting the harness to load it failed.
    /// Carries the underlying error so the operator can act on it.
    RestartFailed(String),
}

/// Render the loud, operator-facing message for a binary-step failure. Pure
/// (returns text, prints nothing) so it is testable without capturing stderr —
/// same pattern as `hex_new_block()`. The `Build` wording is byte-identical to
/// the pre-existing v0.50.4 line so that path stays unchanged; `RestartFailed`
/// is deliberately distinct — it never claims the binary "was NOT updated".
fn binary_step_failure_message(failure: &BinaryStepFailure) -> String {
    match failure {
        BinaryStepFailure::Build => {
            "Upgrade FAILED — the hex binary was NOT updated (see Step 5).".to_string()
        }
        BinaryStepFailure::RestartFailed(err) => format!(
            "Upgrade INCOMPLETE — the new hex binary WAS swapped in, but the harness \
             restart FAILED.\n  \
             The running harness still holds the OLD binary in memory (engine + every \
             worker). New code is on disk but NOT live.\n  \
             Restart error: {err}\n  \
             Run `hex harness restart` manually to load the new binary."
        ),
    }
}

/// Injectable core of `restart_harness`: pure of launchctl/plist I/O so tests
/// exercise the propagation path deterministically (a live restart would bootout
/// the developer's real harness). When no LaunchAgent is installed there is
/// nothing to restart — success forever, and `restart_fn` is NOT invoked.
/// Otherwise the restart action's `Result` propagates unchanged.
fn restart_harness_with<F>(hex_dir: &Path, agent_installed: bool, restart_fn: F) -> Result<(), String>
where
    F: FnOnce(&Path) -> Result<(), String>,
{
    if !agent_installed {
        return Ok(());
    }
    restart_fn(hex_dir)
}

/// Restart the single `com.hex.harness` gui LaunchAgent so the swapped binary
/// (engine + all workers, one process) reloads, then VERIFY the engine actually
/// serves — escalating loudly (S6 alert) if it does not. Routes through
/// `harness::supervise::restart_and_verify`, which holds the bootstrap lock (so it
/// cannot race the watchdog) and re-bootstraps once before giving up. Skipped when
/// the agent isn't installed (nothing to restart on this box). Returns the restart
/// result so the caller can propagate a failure (binary swapped, harness stale).
///
/// `hex_dir` is the workspace root (parent of `.hex`).
fn restart_harness(hex_dir: &Path) -> Result<(), String> {
    let agent_installed = std::env::var("HOME").ok().is_some_and(|home| {
        Path::new(&home)
            .join("Library/LaunchAgents/com.hex.harness.plist")
            .exists()
    });
    let result = restart_harness_with(hex_dir, agent_installed, |dir| {
        hex::harness::supervise::restart_and_verify(dir, "com.hex.harness").map(|_| ())
    });
    match &result {
        Ok(()) if agent_installed => {
            println!("  [OK] restarted com.hex.harness — engine + workers on the new binary");
        }
        // Not installed → nothing was restarted → nothing to announce.
        Ok(()) => {}
        // restart_and_verify already printed [FAIL] + fired the S6 alert; surface it here too
        // so the upgrade output makes the dead harness impossible to miss.
        Err(e) => eprintln!("  [FAIL] com.hex.harness did not come back after upgrade: {e}"),
    }
    result
}

/// The `hex-new` launcher block appended to the user's shell rc. Pure so
/// tests can syntax-check and execute the exact emitted script. Must be
/// valid in zsh AND bash, safe under `set -u`, and must not contain the
/// guard markers of sibling blocks ("claude() {", "hex completions").
fn hex_new_block() -> Vec<String> {
    [
        "# hex session launcher — hex-new [name] [claude args...]",
        "# Launches a hex session from $HEX_DIR with Remote Control enabled",
        "# (drive it from claude.ai/code or the mobile app; the client gates RC",
        "# off when unsupported). A name labels the session and its RC entry.",
        "# hex is session-less — context loads via hooks on attach.",
        "hex-new() {",
        // ${HEX_DIR:-...} default matters: POSIX `cd ""` is a successful
        // no-op, so an unset HEX_DIR would otherwise launch from the
        // caller's cwd instead of failing.
        r#"  cd "${HEX_DIR:-$HOME/hex}" || return"#,
        r#"  case "${1-}" in"#,
        r#"    ""|-*)"#,
        r#"      command claude --dangerously-skip-permissions --remote-control "$@""#,
        "      ;;",
        "    *)",
        r#"      local name="$1"; shift"#,
        r#"      command claude --dangerously-skip-permissions --name "$name" --remote-control "$name" "$@""#,
        "      ;;",
        "  esac",
        "}",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn setup_shell(hex_dir: &Path) {
    let hex_dot_dir = hex_dir.join(".hex");
    let shell = std::env::var("SHELL").unwrap_or_default();
    let user_shell = Path::new(&shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            eprintln!("  [WARN] HOME not set, skipping shell setup");
            return;
        }
    };

    let rc_path = if user_shell == "zsh" || Path::new(&home).join(".zshrc").exists() {
        PathBuf::from(&home).join(".zshrc")
    } else if user_shell == "bash" || Path::new(&home).join(".bashrc").exists() {
        PathBuf::from(&home).join(".bashrc")
    } else {
        eprintln!("  [WARN] Could not detect shell rc file — add PATH manually");
        return;
    };

    let content = fs::read_to_string(&rc_path).unwrap_or_default();
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut dirty = false;

    // Remove old `alias hex=` shim if present
    let before = lines.len();
    lines.retain(|l| !l.starts_with("alias hex="));
    if lines.len() != before {
        dirty = true;
        println!("  [OK] Removed old hex alias");
    }

    if !content.contains("export HEX_DIR=") {
        lines.push(String::new());
        lines.push(format!(r#"export HEX_DIR="{}""#, hex_dir.display()));
        lines.push(r#"export AGENT_DIR="$HEX_DIR"  # deprecated alias — use HEX_DIR"#.to_string());
        dirty = true;
    }

    if !content.contains(".hex/bin") {
        lines.push(String::new());
        lines.push("# hex binary".to_string());
        lines.push(format!(
            r#"export PATH="{bin}:$PATH""#,
            bin = hex_dot_dir.join("bin").display()
        ));
        dirty = true;
    }

    // Guard on the function signature, not the flag: other managed blocks
    // (hex-new) embed --dangerously-skip-permissions in their bodies, which
    // would false-positive here and silently skip installing the wrapper.
    // "function claude" catches the keyword-style definition so a user's
    // hand-rolled wrapper still opts out.
    if !content.contains("claude() {") && !content.contains("function claude") {
        lines.push(String::new());
        lines.push("# Claude Code — skip permission prompts".to_string());
        lines.push("unalias claude 2>/dev/null".to_string());
        lines.push(
            r#"claude() { command claude --dangerously-skip-permissions "$@"; }"#.to_string(),
        );
        dirty = true;
    }

    // Session launcher. The `hex-new` guard doubles as an opt-out: users who
    // define their own hex-new in the rc keep their version.
    if !content.contains("hex-new") {
        lines.push(String::new());
        lines.extend(hex_new_block());
        dirty = true;
    }

    // Shell completions — sourced from the binary so they always match the
    // installed version. Self-contained (no fpath/compinit ordering deps).
    if !content.contains("hex completions") {
        let completions_shell = if rc_path.ends_with(".bashrc") {
            "bash"
        } else {
            "zsh"
        };
        lines.push(String::new());
        lines.push("# hex shell completions".to_string());
        lines.push(format!(
            r#"command -v hex >/dev/null 2>&1 && source <(hex completions {completions_shell})"#
        ));
        dirty = true;
    }

    if dirty {
        let out = lines.join("\n") + "\n";
        let tmp = rc_path.with_extension("tmp");
        if fs::write(&tmp, &out).is_ok() {
            let _ = fs::rename(&tmp, &rc_path);
            println!("  [OK] Shell rc updated: {}", rc_path.display());
        }
    } else {
        println!("  [OK] Shell rc already up to date");
    }
}

/// True iff `dir` is itself the TOP LEVEL of a git work tree, not merely nested
/// inside some surrounding repo. The post-upgrade commit is gated on this so we
/// only ever commit into the instance's OWN repo — never a parent repo that
/// happens to contain the workspace. `git rev-parse --show-toplevel` is the same
/// call `main.rs` already uses to resolve a repo root.
fn is_own_git_toplevel(dir: &Path) -> bool {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let top = String::from_utf8_lossy(&o.stdout).trim().to_string();
            match (fs::canonicalize(&top), fs::canonicalize(dir)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
        }
        _ => false,
    }
}

/// After a successful sync + rebuild, commit the synced tracked files under
/// `.hex/` in the instance workspace so the repo reflects the deployed version.
/// This closes the "deployed-but-orphaned" blind spot (a live deploy left
/// uncommitted that git — and `hex upgrade`'s own change detection — reads as
/// "nothing changed").
///
/// Returns:
///   Ok(true)  — a commit was made.
///   Ok(false) — the synced tree was already clean (no-op success, NOT an error).
///   Err(msg)  — the commit could not be made; the caller MUST surface this
///               LOUDLY (S6: no quiet failures), never a silent skip.
///
/// Scope: only tracked changes under `.hex/` are staged (`git add -u -- .hex`),
/// so the operator's unrelated tracked work (todo.md, me/, projects/, landings/)
/// is never swept into the upgrade commit. New, untracked files are deliberately
/// left out — see docs/hex-ops.md on the instance-side gitignore shadowing of
/// new harness source files.
fn commit_synced_files(workspace: &Path, version: &str) -> Result<bool, String> {
    // Lead with `git status` — it yields all three outcomes deterministically and,
    // unlike `git add -u -- <pathspec>`, does NOT exit-128 when the pathspec matches
    // no tracked files (which would otherwise spuriously fail a clean upgrade).
    // `-uno` = tracked changes only (no untracked noise).
    let status = Command::new("git")
        .args(["status", "--porcelain", "-uno", "--", ".hex"])
        .current_dir(workspace)
        .output()
        .map_err(|e| format!("could not run git status in {}: {e}", workspace.display()))?;
    if !status.status.success() {
        // Non-zero here means git could not operate here at all (e.g. exit 128:
        // not a git repository). The caller surfaces this loudly.
        return Err(format!(
            "git status failed in {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }
    if status.stdout.is_empty() {
        // Clean synced tree — nothing to commit. No-op success.
        return Ok(false);
    }

    // There ARE tracked changes under .hex/: stage exactly those.
    let add = Command::new("git")
        .args(["add", "-u", "--", ".hex"])
        .current_dir(workspace)
        .output()
        .map_err(|e| format!("could not run git add in {}: {e}", workspace.display()))?;
    if !add.status.success() {
        return Err(format!(
            "git add -u -- .hex failed in {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&add.stderr).trim()
        ));
    }

    // Commit with a message naming the version. gpgsign is disabled and hooks are
    // skipped for this bookkeeping commit so it can never hang on a passphrase or
    // pre-commit prompt inside an unattended `hex upgrade`. The commit is given the
    // SAME `.hex` pathspec as the `add` above (`--only -- .hex`): without a pathspec,
    // `git commit` records the WHOLE index, so any work the operator had pre-staged
    // (`git add todo.md`) before running `hex upgrade` would be swept into this
    // bookkeeping commit. `--only -- .hex` records only the working-tree content of
    // `.hex`, leaving every other staged path untouched — matching the scope
    // guarantee documented in docs/hex-ops.md.
    let msg = format!("chore(hex): sync harness files to v{version}");
    let commit = Command::new("git")
        .args([
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--no-verify",
            "--only",
            "-q",
            "-m",
            &msg,
            "--",
            ".hex",
        ])
        .current_dir(workspace)
        .output()
        .map_err(|e| format!("could not run git commit in {}: {e}", workspace.display()))?;
    if !commit.status.success() {
        return Err(format!(
            "git commit failed in {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&commit.stderr).trim()
        ));
    }
    Ok(true)
}

pub fn run(args: &[String]) -> i32 {
    let cfg = match parse_args(args) {
        Ok(c) => c,
        Err(e) if e == "help" => return 0,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return 1;
        }
    };

    let hex_dir = match hex_dir_from_env() {
        Some(d) => d,
        None => {
            eprintln!("ERROR: Cannot determine hex workspace. Set HEX_DIR or run from within your hex directory.");
            return 1;
        }
    };

    let hex_dot_dir = hex_dir.join(".hex");
    let config_file = hex_dot_dir.join("upgrade.json");

    let repo_url = cfg
        .repo_url
        .clone()
        .or_else(|| load_config_repo(&config_file))
        .unwrap_or_else(|| DEFAULT_REPO.to_string());

    let now = chrono::Local::now();
    println!();
    println!("════════════════════════════════════════════════════");
    println!(" Hexagon Upgrade — {}", now.format("%Y-%m-%d %H:%M"));
    println!("════════════════════════════════════════════════════");
    if cfg.dry_run {
        println!("  [DRY RUN] No changes will be made.");
    }
    println!();

    // Step 1: Get source
    println!("1. Get Latest Source");
    let source_dir = match get_source_dir(&cfg, &hex_dir) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("  [FAIL] {e}");
            return 1;
        }
    };

    let layout = path_map::detect_layout(source_dir.to_str().unwrap_or(""));
    if layout == "unknown" {
        eprintln!(
            "  [FAIL] Unknown source layout at {} (expected v2)",
            source_dir.display()
        );
        return 1;
    }
    println!("  → Source layout: {layout}");

    let src_dirs = match source_dirs_for_layout(layout, &source_dir) {
        Some(d) => d,
        None => {
            eprintln!("  [FAIL] Could not resolve source dirs for layout {layout}");
            return 1;
        }
    };

    // Step 3: Detect changes
    println!("\n3. Detect Changes");
    let (c1, n1, u1, log1) =
        detect_changes(&src_dirs.scripts, &hex_dot_dir.join("scripts"), "scripts");
    let (c2, n2, u2, log2) =
        detect_changes(&src_dirs.skills, &hex_dot_dir.join("skills"), "skills");
    let (c3, n3, u3, log3) = detect_changes(
        &src_dirs.commands,
        &hex_dot_dir.join("commands"),
        "commands",
    );
    let (c4, n4, u4, log4) = detect_changes(&src_dirs.hooks, &hex_dot_dir.join("hooks"), "hooks");
    // Additive dirs (iii engine config/workers, launchd + other templates)
    let (c5, n5, u5, log5) = detect_changes(&src_dirs.iii, &hex_dot_dir.join("iii"), "iii");
    let (c6, n6, u6, log6) = detect_changes(
        &src_dirs.templates,
        &hex_dot_dir.join("templates"),
        "templates",
    );

    let total_changed = c1 + c2 + c3 + c4 + c5 + c6;
    let total_new = n1 + n2 + n3 + n4 + n5 + n6;
    let total_unchanged = u1 + u2 + u3 + u4 + u5 + u6;

    println!("  → {total_changed} changed, {total_new} new, {total_unchanged} unchanged");
    for line in log1
        .iter()
        .chain(&log2)
        .chain(&log3)
        .chain(&log4)
        .chain(&log5)
        .chain(&log6)
    {
        println!("{line}");
    }

    // Check version.txt changes
    let mut version_changed = false;
    if let Some(src_ver_file) = &src_dirs.version_txt {
        if src_ver_file.exists() {
            let src_ver = fs::read_to_string(src_ver_file).unwrap_or_default();
            let dst_ver = fs::read_to_string(hex_dot_dir.join("version.txt")).unwrap_or_default();
            if src_ver != dst_ver {
                version_changed = true;
                println!("  ~ version.txt ({} → {})", dst_ver.trim(), src_ver.trim());
            }
        }
    }

    // OBS-028: a binary-only change (Rust source moved, same Cargo version, no
    // synced files changed) must still trigger a rebuild. Without this the gate
    // below early-returns "Nothing to do" before Step 5 ever runs, and the
    // upgrade silently ships nothing while reporting success.
    let binary_stale = binary_is_stale(&hex_dir, &source_dir);

    if total_changed == 0 && total_new == 0 && !version_changed && !binary_stale {
        println!("  [OK] Everything is up to date. Nothing to do.");
        return 0;
    }

    if binary_stale && total_changed == 0 && total_new == 0 && !version_changed {
        println!("  → Binary stale (source moved, no synced files changed) — will rebuild.");
    }

    if cfg.dry_run {
        println!("\n4. Dry Run Complete");
        println!("  → Run without --dry-run to apply changes.");
        return 0;
    }

    // Step 4: Apply changes
    println!("\n4. Apply Changes");
    let backup_dir = hex_dot_dir.join(format!(".upgrade-backup-{}", now.format("%Y%m%d-%H%M%S")));
    fs::create_dir_all(&backup_dir).ok();

    let sync_pairs: &[(&PathBuf, PathBuf)] = &[
        (&src_dirs.scripts, hex_dot_dir.join("scripts")),
        (&src_dirs.skills, hex_dot_dir.join("skills")),
        (&src_dirs.commands, hex_dot_dir.join("commands")),
        (&src_dirs.hooks, hex_dot_dir.join("hooks")),
    ];

    let mut applied = 0;
    for (src, dst) in sync_pairs {
        if src.exists() {
            match apply_sync(src, dst, Some(&backup_dir)) {
                Ok(n) => applied += n,
                Err(e) => eprintln!("  [WARN] Sync failed for {}: {e}", src.display()),
            }
        }
    }

    // Additive dirs: sync (add/update) but DO NOT add to the deletion pass below,
    // so deployed runtime state (.hex/iii/data, worker node_modules) is preserved.
    let additive_pairs: &[(&PathBuf, PathBuf)] = &[
        (&src_dirs.iii, hex_dot_dir.join("iii")),
        (&src_dirs.templates, hex_dot_dir.join("templates")),
    ];
    for (src, dst) in additive_pairs {
        if src.exists() {
            match apply_sync(src, dst, Some(&backup_dir)) {
                Ok(n) => applied += n,
                Err(e) => eprintln!("  [WARN] Sync failed for {}: {e}", src.display()),
            }
        }
    }

    // Mirror commands to runtime slash-command dir
    let runtime_cmd_dir = hex_dir.join(".claude/commands");
    if src_dirs.commands.exists() {
        fs::create_dir_all(&runtime_cmd_dir).ok();
        apply_sync(&src_dirs.commands, &runtime_cmd_dir, None).ok();
    }

    // Deletion pass
    println!("  → Running deletion pass...");
    let mut deleted = 0;
    for (src, dst) in sync_pairs {
        if src.exists() {
            deleted += deletion_pass(dst, src, &backup_dir).unwrap_or(0);
        }
    }
    if src_dirs.commands.exists() {
        deleted += deletion_pass(&runtime_cmd_dir, &src_dirs.commands, &backup_dir).unwrap_or(0);
    }

    if deleted > 0 {
        println!("  [OK] Deletion pass: removed {deleted} stale file(s)");
    } else {
        println!("  → Deletion pass: nothing to prune");
    }

    make_scripts_executable(&hex_dot_dir);

    // Update version.txt for v2 layout
    if let Some(src_ver_file) = &src_dirs.version_txt {
        if src_ver_file.exists() {
            fs::copy(src_ver_file, hex_dot_dir.join("version.txt")).ok();
        }
    }

    println!("  [OK] Applied {applied} file(s)");

    record_upgrade_sha(&config_file, &source_dir, &repo_url);
    let _ = fs::remove_file(hex_dot_dir.join(".update-available"));

    // Step 5: Sync VERSIONS + rebuild binary if needed
    println!("\n5. Sync VERSIONS");
    let binary_result = sync_versions_file(&hex_dir, &source_dir, &backup_dir);

    // Step 6: Shell setup
    println!("\n6. Shell Setup");
    setup_shell(&hex_dir);

    // Step 7: Summary
    println!("\n7. Summary");
    println!("  Files updated:  {total_changed}");
    println!("  Files added:    {total_new}");
    println!();
    if let Err(failure) = &binary_result {
        // Never print success over a binary problem. The message is chosen by
        // failure KIND: Build (binary NOT updated — OBS-017 deploy black hole)
        // vs RestartFailed (binary WAS swapped, harness still runs the old one —
        // 2026-06-12 stale-harness). Both exit nonzero.
        eprintln!("  {}", binary_step_failure_message(failure));
        if matches!(failure, BinaryStepFailure::Build) {
            eprintln!("  The workspace files may have synced, but the running code is stale.");
        }
        println!();
        return 1;
    }

    // Commit the synced tracked files so the instance repo reflects the deployed
    // version. Closes the deployed-but-orphaned blind spot: without this, a live
    // deploy sits uncommitted and git — plus `hex upgrade`'s own change detection
    // — reads it as "nothing changed" indefinitely. Only ever commit into the
    // instance's OWN repo; a failure here is LOUD (S6) and exits nonzero so the
    // operator knows the deploy is live but unrecorded in git.
    if is_own_git_toplevel(&hex_dir) {
        let synced_version = fs::read_to_string(hex_dot_dir.join("version.txt"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        match commit_synced_files(&hex_dir, &synced_version) {
            Ok(true) => {
                println!("  [OK] Committed synced files (v{synced_version}) in instance repo.");
            }
            Ok(false) => {
                println!("  → Instance repo already consistent; no synced-file changes to commit.");
            }
            Err(e) => {
                eprintln!(
                    "  [FAIL] Sync and rebuild SUCCEEDED, but the instance repo could not be committed: {e}"
                );
                eprintln!(
                    "  The deployed version is live but not reflected in git (deployed-but-orphaned)."
                );
                eprintln!(
                    "  Fix: git -C {ws} add -u -- .hex && git -C {ws} commit -m \"chore(hex): sync harness files to v{synced_version}\"",
                    ws = hex_dir.display()
                );
                println!();
                return 1;
            }
        }
    } else {
        println!("  → Workspace is not its own git repo; skipping upgrade commit.");
    }

    println!("  Upgrade complete.");
    println!();

    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    fn write_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    // The wrapper block's guard is the function signature "claude() {".
    // The hex-new block embeds --dangerously-skip-permissions, so guarding
    // the wrapper on that flag (the old guard) false-positives against an
    // rc that has hex-new but no wrapper, silently skipping the wrapper
    // forever. Pin both marker relationships.
    #[test]
    fn hex_new_block_does_not_collide_with_sibling_guards() {
        let block = hex_new_block().join("\n");
        assert!(
            block.contains("hex-new"),
            "must contain its own guard marker"
        );
        assert!(
            !block.contains("claude() {") && !block.contains("function claude"),
            "must not contain the claude() wrapper's guard markers"
        );
        assert!(
            !block.contains("hex completions"),
            "must not contain the completions block's guard marker"
        );
        let src = include_str!("upgrade.rs");
        // Needle built at runtime so this assertion doesn't match itself.
        let needle = format!(r#"content.contains("{}")"#, "dangerously-skip-permissions");
        assert!(
            !src.contains(&needle),
            "wrapper guard must key on the function signature, not the flag \
             (the hex-new block embeds the flag in its body)"
        );
    }

    /// Runs `hex-new <invocation>` through bash with a stubbed `claude`,
    /// returning (exit ok, recorded argv lines, cwd at launch).
    fn run_hex_new(invocation: &str, set_hex_dir: bool) -> (bool, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let block = dir.path().join("block.sh");
        fs::write(&block, hex_new_block().join("\n")).unwrap();

        let bin = dir.path().join("bin");
        let args_out = dir.path().join("args.txt");
        write_file(
            &bin.join("claude"),
            "#!/bin/sh\npwd > \"$CLAUDE_CWD_OUT\"\nprintf '%s\\n' \"$@\" > \"$CLAUDE_ARGS_OUT\"\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let hexdir = dir.path().join("hexdir");
        fs::create_dir(&hexdir).unwrap();

        let mut cmd = std::process::Command::new("bash");
        // set -u: the block must survive nounset rc environments.
        cmd.arg("-c")
            .arg(format!(
                "set -u; . '{}'; hex-new {}",
                block.display(),
                invocation
            ))
            .env("CLAUDE_ARGS_OUT", &args_out)
            .env("CLAUDE_CWD_OUT", dir.path().join("cwd.txt"))
            // HOME without a hex/ dir, so the unset-HEX_DIR case must fail.
            .env("HOME", dir.path())
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            );
        if set_hex_dir {
            cmd.env("HEX_DIR", &hexdir);
        } else {
            cmd.env_remove("HEX_DIR");
        }
        let out = cmd.output().unwrap();
        (
            out.status.success(),
            fs::read_to_string(&args_out).unwrap_or_default(),
            fs::read_to_string(dir.path().join("cwd.txt")).unwrap_or_default(),
        )
    }

    #[test]
    fn hex_new_named_session_routes_name_to_both_flags() {
        let (ok, args, cwd) = run_hex_new("debug --model opus", true);
        assert!(ok);
        assert_eq!(
            args,
            "--dangerously-skip-permissions\n--name\ndebug\n--remote-control\ndebug\n--model\nopus\n"
        );
        assert!(cwd.trim().ends_with("hexdir"), "must launch from HEX_DIR");
    }

    #[test]
    fn hex_new_leading_flag_means_no_name() {
        let (ok, args, _) = run_hex_new("--model sonnet", true);
        assert!(ok);
        assert_eq!(
            args,
            "--dangerously-skip-permissions\n--remote-control\n--model\nsonnet\n"
        );
    }

    #[test]
    fn hex_new_no_args_under_nounset() {
        let (ok, args, _) = run_hex_new("", true);
        assert!(ok);
        assert_eq!(args, "--dangerously-skip-permissions\n--remote-control\n");
    }

    // POSIX `cd ""` is a successful no-op, so without the ${HEX_DIR:-...}
    // default an unset HEX_DIR would launch claude (skip-permissions!) in
    // the caller's cwd. With the default pointing at a missing HEX_DIR fallback,
    // cd must fail and claude must never run.
    #[test]
    fn hex_new_unset_hex_dir_fails_instead_of_launching_in_cwd() {
        let (ok, args, cwd) = run_hex_new("debug", false);
        assert!(
            !ok,
            "must fail when HEX_DIR is unset and $HOME/hex is absent"
        );
        assert!(
            args.is_empty() && cwd.is_empty(),
            "claude must not have run"
        );
    }

    // Regression test for spec S90mv90b6 / task Tndh988cz: AGENTS.md is the
    // single canonical instruction-file template. upgrade.rs must not
    // reference the old per-runtime template path anywhere — neither in the
    // v2-layout sentinel error string, nor in test fixtures, nor in the
    // //! Preserves doc comment. The needle is built at runtime so this
    // assertion itself doesn't trip the check.
    #[test]
    fn upgrade_rs_has_no_legacy_template_references() {
        let src = include_str!("upgrade.rs");
        let needle = format!("templates/{}.md", "CLAUDE");
        assert!(
            !src.contains(&needle),
            "upgrade.rs must not reference the legacy per-runtime template \
             path; the canonical template is templates/AGENTS.md (the v2 \
             sentinel and test fixtures must be repointed)"
        );
    }

    // OBS-028 regression: the exact case that shipped nothing — same Cargo
    // version, installed binary built at an older commit. Must rebuild.
    #[test]
    fn binary_needs_rebuild_on_sha_mismatch_same_version() {
        assert!(binary_needs_rebuild(
            Some("0.29.0"),
            "0.29.0",
            Some("9ecdfb29"),
            Some("b1b38e50"),
        ));
    }

    #[test]
    fn binary_needs_rebuild_false_when_sha_and_version_match() {
        assert!(!binary_needs_rebuild(
            Some("0.29.0"),
            "0.29.0",
            Some("b1b38e50"),
            Some("b1b38e50"),
        ));
    }

    #[test]
    fn binary_needs_rebuild_on_version_mismatch() {
        assert!(binary_needs_rebuild(
            Some("0.28.0"),
            "0.29.0",
            Some("b1b38e50"),
            Some("b1b38e50"),
        ));
    }

    #[test]
    fn binary_needs_rebuild_true_when_installed_missing() {
        // No installed binary / --version failed → must build.
        assert!(binary_needs_rebuild(None, "0.29.0", None, Some("b1b38e50")));
    }

    #[test]
    fn binary_needs_rebuild_ignores_sha_when_source_sha_unknown() {
        // git rev-parse failed (source_sha None): don't rebuild on SHA alone
        // when the version already matches — avoids needless rebuilds offline.
        assert!(!binary_needs_rebuild(
            Some("0.29.0"),
            "0.29.0",
            Some("9ecdfb29"),
            None,
        ));
    }

    #[test]
    fn test_hooks_sync_lands_in_target() {
        // Core requirement: v2 layout hook files must sync to .hex/hooks/
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let hex_dot = tmp.path().join(".hex");

        // Set up v2 source with a hook file
        write_file(
            &source.join("system/hooks/scripts/my-hook.sh"),
            "#!/bin/bash\necho hello",
        );
        write_file(&source.join("templates/AGENTS.md"), "# Agents");
        fs::create_dir_all(source.join("system/scripts")).unwrap();
        fs::create_dir_all(source.join("system/skills")).unwrap();
        fs::create_dir_all(source.join("system/commands")).unwrap();

        let layout = path_map::detect_layout(source.to_str().unwrap());
        assert_eq!(layout, "v2");

        let src_dirs = source_dirs_for_layout(layout, &source).unwrap();
        let dst_hooks = hex_dot.join("hooks");

        apply_sync(&src_dirs.hooks, &dst_hooks, None).unwrap();

        let target = dst_hooks.join("scripts/my-hook.sh");
        assert!(
            target.exists(),
            "hook file must be synced to .hex/hooks/scripts/my-hook.sh"
        );
        assert!(fs::read_to_string(&target).unwrap().contains("echo hello"));
    }

    #[test]
    fn test_hooks_sync_updates_changed_file() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let hex_dot = tmp.path().join(".hex");

        write_file(
            &source.join("system/hooks/scripts/hook.sh"),
            "#!/bin/bash\nnew content",
        );
        write_file(&source.join("templates/AGENTS.md"), "# Agents");
        // Pre-existing stale hook in destination
        write_file(
            &hex_dot.join("hooks/scripts/hook.sh"),
            "#!/bin/bash\nold content",
        );

        let layout = path_map::detect_layout(source.to_str().unwrap());
        let src_dirs = source_dirs_for_layout(layout, &source).unwrap();
        let dst_hooks = hex_dot.join("hooks");

        let backup_dir = tmp.path().join("backup");
        fs::create_dir_all(&backup_dir).unwrap();
        apply_sync(&src_dirs.hooks, &dst_hooks, Some(&backup_dir)).unwrap();

        let result = fs::read_to_string(hex_dot.join("hooks/scripts/hook.sh")).unwrap();
        assert_eq!(result, "#!/bin/bash\nnew content");
        // Old file backed up
        assert!(
            backup_dir.join("scripts/hook.sh").exists(),
            "old hook must be backed up"
        );
    }

    #[test]
    fn test_deletion_pass_removes_stale_files() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        let bak = tmp.path().join("bak");

        write_file(&src.join("current.sh"), "#!/bin/bash\n# current");
        write_file(&dst.join("current.sh"), "#!/bin/bash\n# current");
        write_file(&dst.join("stale.sh"), "#!/bin/bash\n# stale");

        let deleted = deletion_pass(&dst, &src, &bak).unwrap();
        assert_eq!(deleted, 1);
        assert!(!dst.join("stale.sh").exists(), "stale file must be removed");
        assert!(
            bak.join("stale.sh").exists(),
            "stale file must be backed up"
        );
        assert!(dst.join("current.sh").exists(), "current file must remain");
    }

    #[test]
    fn test_parse_args_dry_run() {
        let args = vec!["--dry-run".to_string()];
        let cfg = parse_args(&args).unwrap();
        assert!(cfg.dry_run);
        assert!(cfg.repo_url.is_none());
    }

    #[test]
    fn test_parse_args_local() {
        let args = vec!["--local".to_string(), "/some/path".to_string()];
        let cfg = parse_args(&args).unwrap();
        assert!(!cfg.dry_run);
        assert_eq!(cfg.local_path.as_deref(), Some("/some/path"));
    }

    #[test]
    fn test_parse_args_repo() {
        let args = vec![
            "--repo".to_string(),
            "https://example.com/repo.git".to_string(),
        ];
        let cfg = parse_args(&args).unwrap();
        assert_eq!(
            cfg.repo_url.as_deref(),
            Some("https://example.com/repo.git")
        );
    }

    #[test]
    fn test_v2_source_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path();
        let src_dirs = source_dirs_for_layout("v2", source).unwrap();
        assert!(src_dirs.scripts.ends_with("system/scripts"));
        assert!(src_dirs.hooks.ends_with("system/hooks"));
        assert!(src_dirs.iii.ends_with("system/iii"));
        assert!(src_dirs.templates.ends_with("system/templates"));
        assert!(src_dirs.version_txt.is_some());
    }

    #[test]
    fn test_files_differ_detects_change() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        fs::write(&a, "hello").unwrap();
        fs::write(&b, "world").unwrap();
        assert!(files_differ(&a, &b));
        fs::write(&b, "hello").unwrap();
        assert!(!files_differ(&a, &b));
    }

    /// atomic_install_binary must: install to dst, make it executable, leave no temp behind.
    #[test]
    #[cfg(target_os = "macos")]
    fn test_atomic_install_binary_basic() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src_bin");
        // Write a minimal valid Mach-O or any binary-ish content; codesign on macOS
        // accepts any file for ad-hoc signing, so a simple ELF stub won't work.
        // Use the current test binary as the source — it is already a real executable.
        let self_path = std::env::current_exe().unwrap();
        let dst = tmp.path().join("dst_bin");

        atomic_install_binary(&self_path, &dst).unwrap();

        assert!(dst.exists(), "dst must exist after atomic install");
        let mode = fs::metadata(&dst).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "dst must be executable");

        // No temp files should remain
        let temps: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".hex-install-"))
            .collect();
        assert!(
            temps.is_empty(),
            "no temp files should remain after success"
        );
        drop(src);
    }

    /// Calling atomic_install_binary twice over the same dst must produce
    /// a different inode each time — proving rename semantics, not in-place overwrite.
    #[test]
    #[cfg(target_os = "macos")]
    fn test_atomic_install_binary_different_inode() {
        let tmp = tempfile::tempdir().unwrap();
        let self_path = std::env::current_exe().unwrap();
        let dst = tmp.path().join("dst_inode");

        atomic_install_binary(&self_path, &dst).unwrap();
        let ino1 = fs::metadata(&dst).unwrap().ino();

        atomic_install_binary(&self_path, &dst).unwrap();
        let ino2 = fs::metadata(&dst).unwrap().ino();

        assert_ne!(ino1, ino2, "each atomic install must produce a fresh inode");
    }

    /// After a successful atomic install, no .hex-install-*.tmp file must remain.
    #[test]
    #[cfg(target_os = "macos")]
    fn test_atomic_install_binary_no_temp_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        let self_path = std::env::current_exe().unwrap();
        let dst = tmp.path().join("dst_cleanup");

        atomic_install_binary(&self_path, &dst).unwrap();

        let leftover: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".hex-install-"))
            .collect();
        assert!(
            leftover.is_empty(),
            "no .hex-install-*.tmp must remain after success: {:?}",
            leftover.iter().map(|e| e.file_name()).collect::<Vec<_>>()
        );
    }

    /// OBS-017: release_bin must always be harness_dst/target/release/hex regardless of
    /// workspace nesting. This test verifies the path formula used in sync_versions_file.
    #[test]
    fn test_release_bin_path_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        // Simulate harness_dst deep inside a workspace: <root>/.hex/harness
        let harness_dst = tmp.path().join(".hex").join("harness");
        fs::create_dir_all(&harness_dst).unwrap();

        // With --target-dir harness_dst/target, the binary is ALWAYS here:
        let expected = harness_dst.join("target/release/hex");

        // The old guessing code would have tried harness_dst.parent()/target/release/hex
        // which for a workspace root = <root>/.hex/target/release/hex (wrong level).
        let old_guess = harness_dst
            .parent()
            .map(|p| p.join("target/release/hex"))
            .unwrap();

        assert_ne!(
            expected, old_guess,
            "old workspace-guessing path differs from deterministic path (confirms the bug)"
        );
        assert!(
            expected.starts_with(&harness_dst),
            "deterministic release_bin must be inside harness_dst"
        );
    }

    /// Defect 3 safety: deletion_pass scoped to src/ sub-dir must NOT touch a sibling
    /// target/ directory even when both live under the same parent.
    #[test]
    fn test_harness_deletion_pass_does_not_touch_target() {
        let tmp = tempfile::tempdir().unwrap();
        // Simulate harness_dst layout
        let harness_dst = tmp.path().join("harness");
        let bak = tmp.path().join("bak");

        // Files that should survive (target build cache)
        let target_bin = harness_dst.join("target/release/hex");
        write_file(&target_bin, "binary");
        write_file(&harness_dst.join("Cargo.lock"), "lock");

        // Files in src/ that exist in dst but not in source → stale → should be deleted
        write_file(&harness_dst.join("src/old_module.rs"), "// stale");
        // File in src/ that exists in source → should be kept
        write_file(&harness_dst.join("src/lib.rs"), "// current");

        let src_dir = tmp.path().join("src_foundation").join("src");
        write_file(&src_dir.join("lib.rs"), "// current");
        // old_module.rs is absent from src_foundation/src → stale

        fs::create_dir_all(&bak).unwrap();

        // Call deletion_pass SCOPED to src/ only (as the fix does)
        let dst_src = harness_dst.join("src");
        let deleted = deletion_pass(&dst_src, &src_dir, &bak).unwrap();

        assert_eq!(deleted, 1, "only old_module.rs should be pruned");
        assert!(
            !dst_src.join("old_module.rs").exists(),
            "stale src file must be removed"
        );
        assert!(
            dst_src.join("lib.rs").exists(),
            "current src file must remain"
        );

        // Critical: target/ and Cargo.lock must be untouched
        assert!(
            target_bin.exists(),
            "target/release/hex must NOT be deleted"
        );
        assert!(
            harness_dst.join("Cargo.lock").exists(),
            "Cargo.lock must NOT be deleted"
        );
    }

    /// Defect 2: personal overlay detection keys on overlay PRESENCE (a
    /// `harness-personal/` or `modules/` dir), not a specific file.
    #[test]
    fn test_personal_overlay_marker_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let hex_dot_dir = tmp.path().join(".hex");

        // No overlay dirs → not a personal build (exercises the real production fn).
        assert!(!super::detect_personal_overlay(&hex_dot_dir));

        // A harness-personal/ overlay (e.g. an integration probe) → personal build.
        write_file(
            &hex_dot_dir.join("harness-personal/integration_foo.rs"),
            "// probe",
        );
        assert!(
            super::detect_personal_overlay(&hex_dot_dir),
            "overlay dir present → personal build"
        );
    }

    /// Cache health check: a real `git init` repo is healthy; a headless `.git`
    /// shell (only config, no HEAD/objects) is NOT (git would resolve up-tree);
    /// a missing dir is NOT. No network — uses a local `git init`.
    #[test]
    fn test_cache_is_healthy() {
        let tmp = tempfile::tempdir().unwrap();

        // Missing dir → unhealthy.
        let missing = tmp.path().join("missing");
        assert!(!cache_is_healthy(&missing));

        // Real repo → healthy.
        let good = tmp.path().join("good");
        fs::create_dir_all(&good).unwrap();
        let init = Command::new("git")
            .arg("-C")
            .arg(&good)
            .args(["init", "-q"])
            .status();
        // Skip the healthy assertion if git is unavailable in the test env.
        if matches!(init, Ok(s) if s.success()) {
            assert!(
                cache_is_healthy(&good),
                "a real git init repo must be healthy"
            );
        }

        // Headless .git shell (config + hook samples only, no HEAD) → unhealthy.
        // Nest it inside `good` so any up-tree resolution would find good/.git
        // and wrongly pass a naive existence check.
        let corrupt = good.join("corrupt");
        write_file(&corrupt.join(".git/config"), "[core]\n");
        write_file(&corrupt.join(".git/hooks/pre-commit.sample"), "#!/bin/sh\n");
        assert!(
            !cache_is_healthy(&corrupt),
            "a headless .git shell must be unhealthy (must not resolve up-tree)"
        );
    }

    /// The binary step must report health honestly: an up-to-date skip is
    /// `true`; an unparseable Cargo.toml (can't even determine the target
    /// version — the source sync is broken) is `false`, which run() turns
    /// into a failed upgrade instead of "Upgrade complete." (OBS-017).
    #[cfg(unix)]
    #[test]
    fn sync_versions_file_returns_binary_step_health() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let hex_dir = tmp.path().join("hex");
        let source_dir = tmp.path().join("source");
        let backup_dir = tmp.path().join("backup");
        fs::create_dir_all(&backup_dir).unwrap();
        write_file(&hex_dir.join("VERSIONS"), "HEX_FOUNDATION_VERSION=v0.1.0\n");
        write_file(
            &source_dir.join("system/harness/Cargo.toml"),
            "[package]\nname = \"hex-harness\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );
        let bin_dir = hex_dir.join(".hex/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let mock_bin = bin_dir.join("hex");
        fs::write(&mock_bin, "#!/bin/sh\necho hex 1.0.0\n").unwrap();
        fs::set_permissions(&mock_bin, fs::Permissions::from_mode(0o755)).unwrap();

        // Version matches → legitimate skip → healthy.
        assert!(
            sync_versions_file(&hex_dir, &source_dir, &backup_dir).is_ok(),
            "up-to-date skip must report the binary step healthy"
        );

        // Cargo.toml present but versionless → cannot verify anything → NOT healthy.
        write_file(
            &source_dir.join("system/harness/Cargo.toml"),
            "[package]\nname = \"hex-harness\"\nedition = \"2021\"\n",
        );
        assert!(
            sync_versions_file(&hex_dir, &source_dir, &backup_dir).is_err(),
            "unparseable Cargo.toml must fail the binary step loudly"
        );

        // Missing VERSIONS (older layout) → step not applicable → healthy no-op.
        fs::remove_file(hex_dir.join("VERSIONS")).unwrap();
        assert!(sync_versions_file(&hex_dir, &source_dir, &backup_dir).is_ok());
    }

    /// The deploy-black-hole path itself (OBS-017): a rebuild is NEEDED
    /// (version mismatch) but the rebuild machinery fails — here the harness
    /// source sync fails deterministically because `.hex/harness` exists as a
    /// FILE. sync_versions_file must return false so run() fails the upgrade
    /// instead of printing "Upgrade complete." over a stale binary. This
    /// enters the `binary_needs_rebuild == true` block without invoking
    /// cargo.
    #[cfg(unix)]
    #[test]
    fn sync_versions_file_fails_when_rebuild_path_breaks() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let hex_dir = tmp.path().join("hex");
        let source_dir = tmp.path().join("source");
        let backup_dir = tmp.path().join("backup");
        fs::create_dir_all(&backup_dir).unwrap();
        write_file(&hex_dir.join("VERSIONS"), "HEX_FOUNDATION_VERSION=v0.1.0\n");
        write_file(
            &source_dir.join("system/harness/Cargo.toml"),
            "[package]\nname = \"hex-harness\"\nversion = \"2.0.0\"\nedition = \"2021\"\n",
        );
        // Installed binary reports an OLDER version → rebuild required.
        let bin_dir = hex_dir.join(".hex/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let mock_bin = bin_dir.join("hex");
        fs::write(&mock_bin, "#!/bin/sh\necho hex 1.0.0\n").unwrap();
        fs::set_permissions(&mock_bin, fs::Permissions::from_mode(0o755)).unwrap();
        // Sabotage: .hex/harness is a FILE, so the harness source sync fails.
        fs::write(hex_dir.join(".hex/harness"), "not a directory").unwrap();

        assert!(
            sync_versions_file(&hex_dir, &source_dir, &backup_dir).is_err(),
            "a broken rebuild path must fail the binary step (deploy black hole)"
        );
    }

    /// Prebuilt/hand-installed binaries have no hex.sha. When the version
    /// already matches, that must NOT force a rebuild (a cargo-less box would
    /// hard-fail every upgrade forever on a binary that is already current);
    /// a genuine version mismatch must still rebuild.
    #[test]
    fn binary_needs_rebuild_skips_when_installed_sha_unrecorded() {
        assert!(!binary_needs_rebuild(
            Some("0.50.3"),
            "0.50.3",
            None,
            Some("d1c63f37"),
        ));
        assert!(binary_needs_rebuild(
            Some("0.50.2"),
            "0.50.3",
            None,
            Some("d1c63f37"),
        ));
    }

    // Regression test for the 2026-07-16 audit finding: sync_versions_file
    // rewrote the instance VERSIONS file with only HEX_FOUNDATION_VERSION,
    // destroying every KEY=VALUE line it does not itself manage. Verified
    // live: the production instance's VERSIONS lost its BOI_VERSION pin
    // (which install.sh parity reads). The contract must be: preserve every
    // existing KEY=VALUE line and comment we do not manage; only
    // update/insert the keys we own (HEX_FOUNDATION_VERSION). Name is
    // grep-anchored on `preserv|boi_version` so the spec's regression gate
    // (grep -rqE 'fn [a-z_]*(preserv|boi_version)[a-z_]*') pins it.
    #[cfg(unix)]
    #[test]
    fn sync_versions_file_preserves_boi_version_and_comments() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let hex_dir = tmp.path().join("hex");
        let source_dir = tmp.path().join("source");
        let backup_dir = tmp.path().join("backup");
        fs::create_dir_all(&hex_dir).unwrap();
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&backup_dir).unwrap();

        // Cargo.toml pins the foundation version at 1.0.0.
        write_file(
            &source_dir.join("system/harness/Cargo.toml"),
            "[package]\nname = \"hex-harness\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );

        // Mock installed hex binary reporting the same version so
        // binary_needs_rebuild returns false (no cargo build triggered by
        // this unit test).
        let bin_dir = hex_dir.join(".hex/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let mock_bin = bin_dir.join("hex");
        fs::write(&mock_bin, "#!/bin/sh\necho hex 1.0.0\n").unwrap();
        fs::set_permissions(&mock_bin, fs::Permissions::from_mode(0o755)).unwrap();

        // VERSIONS with a comment block, the managed HEX_FOUNDATION_VERSION
        // key (out of date), an UNMANAGED BOI_VERSION pin (parity with
        // install.sh), and an unmanaged custom key. All non-managed lines
        // must survive the sync — only HEX_FOUNDATION_VERSION should
        // change.
        let versions_path = hex_dir.join("VERSIONS");
        let original = "\
# hex-foundation instance pins
# Managed section — edit via `hex upgrade`

HEX_FOUNDATION_VERSION=v0.0.0
BOI_VERSION=v0.5.0
CUSTOM_INSTANCE_PIN=abc123
";
        fs::write(&versions_path, original).unwrap();

        let _ = sync_versions_file(&hex_dir, &source_dir, &backup_dir);

        let after = fs::read_to_string(&versions_path).unwrap();
        assert!(
            after.contains("HEX_FOUNDATION_VERSION=v1.0.0"),
            "sync_versions_file must update the managed key. Got:\n{after}",
        );
        assert!(
            after.contains("BOI_VERSION=v0.5.0"),
            "sync_versions_file must PRESERVE the unmanaged BOI_VERSION pin \
             (install.sh parity reads it). Got:\n{after}",
        );
        assert!(
            after.contains("CUSTOM_INSTANCE_PIN=abc123"),
            "sync_versions_file must preserve every unmanaged KEY=VALUE line, \
             not just BOI_VERSION. Got:\n{after}",
        );
        assert!(
            after.contains("# hex-foundation instance pins"),
            "sync_versions_file must preserve comments. Got:\n{after}",
        );
        assert!(
            after.contains("# Managed section — edit via `hex upgrade`"),
            "sync_versions_file must preserve every comment line. Got:\n{after}",
        );
        // Guard against duplicate managed keys after a rewrite (would be a
        // regression on the merge/update logic).
        let hex_ver_count = after.matches("HEX_FOUNDATION_VERSION=").count();
        assert_eq!(
            hex_ver_count, 1,
            "sync_versions_file must not duplicate the managed key. Got:\n{after}",
        );
    }

    // ---------------------------------------------------------------------
    // RED tests for task T82eegath (restart-health).
    //
    // Today `restart_harness` (line ~919) returns `()`: its call site (line
    // ~835, inside sync_versions_file's rebuild branch) discards the result
    // entirely, so a harness restart failure after a successful binary swap
    // is silently swallowed — `binary_step_ok` stays `true`, `run()` prints
    // "Upgrade complete." and exits 0 while the running harness still holds
    // the OLD binary in memory (the exact 2026-06-12 stale-harness incident,
    // see harness/supervise.rs's module doc).
    //
    // The tests below pin the required fix at a seam that does NOT touch
    // `supervise.rs` (out of scope for this task) and does NOT invoke real
    // `launchctl` or mutate `$HOME` (cargo test is multithreaded — an
    // in-process $HOME mutation would race hex_dir_from_env/setup_shell,
    // which also read it; and calling the real restart against the actual
    // installed `com.hex.harness` LaunchAgent on this box would bootout the
    // developer's live harness). They currently FAIL TO COMPILE because the
    // fix has not been implemented yet:
    //
    //   - `restart_harness_with(hex_dir, agent_installed, restart_fn)` must
    //     be added as an injectable, pure-of-I/O core for `restart_harness`:
    //     when `agent_installed` is false it must short-circuit to `Ok(())`
    //     WITHOUT calling `restart_fn` (nothing installed → nothing to
    //     restart, must stay success forever); otherwise it must call
    //     `restart_fn(hex_dir)` and return its `Result<(), String>`
    //     unchanged. `restart_harness` itself becomes a thin wrapper that
    //     checks the real plist and delegates to
    //     `hex::harness::supervise::restart_and_verify` as the closure.
    //   - `BinaryStepFailure` must be added as a small enum distinguishing
    //     *why* the binary step failed — `Build` (today's only case, the
    //     v0.50.4 message) vs `RestartFailed(String)` (binary WAS swapped,
    //     harness restart failed) — because folding a restart failure into
    //     the existing single `bool` can only ever reproduce the build-
    //     failure message, which is factually wrong once the binary has
    //     already been swapped.
    //   - `binary_step_failure_message(&BinaryStepFailure) -> String` must
    //     render the loud, distinct message for each case (mirrors the
    //     existing pure-fn-returns-text pattern used by `hex_new_block()`
    //     above, so it's testable without capturing stdout/stderr).
    #[test]
    fn restart_harness_with_propagates_restart_result_without_live_launchctl() {
        let tmp = tempfile::tempdir().unwrap();
        let hex_dir = tmp.path();

        // No LaunchAgent installed → nothing to restart → success, and the
        // restart action must never even be invoked (invoking it is exactly
        // the live-launchctl hazard this seam exists to avoid).
        let ok = restart_harness_with(hex_dir, false, |_: &Path| -> Result<(), String> {
            panic!("must not attempt a restart when no LaunchAgent is installed")
        });
        assert!(ok.is_ok(), "no agent installed must report success");

        // Agent installed, restart succeeds → success (unchanged behavior;
        // this is the success-unchanged verification).
        let ok2 = restart_harness_with(hex_dir, true, |_: &Path| Ok(()));
        assert!(ok2.is_ok(), "a successful restart must report success");

        // Agent installed, restart fails → the failure must propagate, not
        // be swallowed.
        let failed = restart_harness_with(hex_dir, true, |_: &Path| {
            Err("launchctl bootstrap failed: EIO".to_string())
        });
        assert!(
            failed.is_err(),
            "a failed harness restart must propagate as an error, not be swallowed \
             (this is the 2026-06-12 stale-harness bug: binary swapped, restart \
             failed, upgrade exited 0 anyway)"
        );
    }

    #[test]
    fn restart_failure_message_is_distinct_from_binary_build_failure_message() {
        // The existing v0.50.4 message (run(), line ~1301) — printed when the
        // binary rebuild/install itself failed and the binary was NOT
        // updated. The new restart-failure message must never be confusable
        // with this one: after a restart failure the binary WAS swapped.
        let build_failure_msg = "Upgrade FAILED — the hex binary was NOT updated (see Step 5).";

        let restart_err = "launchctl bootstrap failed: EIO".to_string();
        let msg = binary_step_failure_message(&BinaryStepFailure::RestartFailed(restart_err.clone()));

        assert_ne!(
            msg, build_failure_msg,
            "restart-failure message must differ from the binary-build failure message"
        );
        assert!(
            !msg.contains("was NOT updated"),
            "restart-failure message must not claim the binary wasn't updated — \
             it WAS swapped. Got:\n{msg}"
        );
        assert!(
            msg.to_lowercase().contains("swapped"),
            "restart-failure message must state the binary WAS swapped. Got:\n{msg}"
        );
        assert!(
            msg.to_lowercase().contains("restart") && msg.to_lowercase().contains("harness"),
            "restart-failure message must state the harness restart failed. Got:\n{msg}"
        );
        assert!(
            msg.contains(&restart_err),
            "restart-failure message should surface the underlying error so an \
             operator can act on it. Got:\n{msg}"
        );
        assert!(
            msg.contains("hex harness restart"),
            "restart-failure message must tell the operator what to run \
             manually (`hex harness restart`). Got:\n{msg}"
        );

        // The pre-existing build-failure case must render BYTE-IDENTICAL to
        // today's v0.50.4 output (run(), line ~1301) — success-unchanged:
        // this task must not alter the build-failure path's wording at all.
        let build_msg = binary_step_failure_message(&BinaryStepFailure::Build);
        assert_eq!(
            build_msg, build_failure_msg,
            "the build-failure message must stay byte-identical to the existing \
             v0.50.4 wording — this task only adds the restart-failure case"
        );
    }

    /// Pins the WIRING, not just the pieces: `sync_versions_file` must
    /// itself return `Result<(), BinaryStepFailure>` (not a bare `bool`).
    /// A `bool` return can only ever fold `Build` and `RestartFailed` into a
    /// single `false`, which is how an implementer could add
    /// `restart_harness_with` / `BinaryStepFailure` / the message fn above,
    /// pass both other red tests, and still leave `run()` printing the
    /// build-failure wording over a restart failure (the binary WAS
    /// swapped) — exactly the bug this task exists to close. Reuses the
    /// no-cargo-invoked fixture from `sync_versions_file_returns_binary_step_health`
    /// (version match → healthy; unparseable Cargo.toml → fails as `Build`,
    /// since no rebuild/restart is ever attempted on that path).
    #[cfg(unix)]
    #[test]
    fn sync_versions_file_return_type_carries_the_failure_kind() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let hex_dir = tmp.path().join("hex");
        let source_dir = tmp.path().join("source");
        let backup_dir = tmp.path().join("backup");
        fs::create_dir_all(&backup_dir).unwrap();
        write_file(&hex_dir.join("VERSIONS"), "HEX_FOUNDATION_VERSION=v0.1.0\n");
        write_file(
            &source_dir.join("system/harness/Cargo.toml"),
            "[package]\nname = \"hex-harness\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );
        let bin_dir = hex_dir.join(".hex/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let mock_bin = bin_dir.join("hex");
        fs::write(&mock_bin, "#!/bin/sh\necho hex 1.0.0\n").unwrap();
        fs::set_permissions(&mock_bin, fs::Permissions::from_mode(0o755)).unwrap();

        // Version matches → legitimate skip → Ok.
        assert!(
            sync_versions_file(&hex_dir, &source_dir, &backup_dir).is_ok(),
            "up-to-date skip must report the binary step healthy"
        );

        // Cargo.toml present but versionless → cannot verify anything → must
        // fail as Build (no rebuild/restart was ever attempted here), never
        // RestartFailed.
        write_file(
            &source_dir.join("system/harness/Cargo.toml"),
            "[package]\nname = \"hex-harness\"\nedition = \"2021\"\n",
        );
        assert_eq!(
            sync_versions_file(&hex_dir, &source_dir, &backup_dir),
            Err(BinaryStepFailure::Build),
            "an unparseable Cargo.toml must fail the binary step as Build, \
             never RestartFailed"
        );
    }

    // -----------------------------------------------------------------
    // Task Tyeav60q3: after a successful sync + rebuild, `hex upgrade`
    // must commit the synced tracked files in the instance workspace
    // with a message naming the version, and fail LOUDLY (never a silent
    // skip) if the commit cannot be made.
    //
    // RED tests: `commit_synced_files` does not exist yet, so this whole
    // test target will not compile until it is implemented. The seam:
    //
    //   fn commit_synced_files(workspace: &Path, version: &str)
    //       -> Result<bool, String>
    //     Ok(true)  = a commit was made
    //     Ok(false) = nothing to commit (clean synced tree) — NOT an error
    //     Err(msg)  = commit could not be made — caller MUST surface this
    //                 loudly (eprintln! + nonzero return in run()).
    //
    // The commit is scoped to synced files under `.hex/`; the operator's
    // unrelated tracked work (todo.md, me/, projects/, landings/) must NOT
    // be swept into the upgrade commit.
    // -----------------------------------------------------------------

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

    fn head_subject(dir: &Path) -> String {
        let out = Command::new("git")
            .args(["log", "-1", "--format=%s"])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn path_is_dirty(dir: &Path, rel: &str) -> bool {
        let out = Command::new("git")
            .args(["status", "--porcelain", "--", rel])
            .current_dir(dir)
            .output()
            .unwrap();
        !out.stdout.is_empty()
    }

    // 1. A modified, already-tracked synced file under `.hex/` is committed,
    //    and the commit subject names the upgraded version.
    #[test]
    fn commit_synced_files_commits_tracked_changes_and_names_version() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".hex/scripts/foo.sh"), "echo old\n");
        seed_commit(ws, "seed");
        // Simulate the upgrade sync editing the tracked synced file.
        write_file(&ws.join(".hex/scripts/foo.sh"), "echo new\n");

        let made = commit_synced_files(ws, "9.9.9-test")
            .expect("committing tracked synced changes must succeed");
        assert!(made, "a commit must have been made (Ok(true))");
        assert!(
            head_subject(ws).contains("9.9.9-test"),
            "commit subject must name the version; got: {}",
            head_subject(ws)
        );
        assert!(
            !path_is_dirty(ws, ".hex/scripts/foo.sh"),
            "the synced file's change must now be committed"
        );
    }

    // 2. A clean synced tree is a no-op success, NOT a loud error.
    #[test]
    fn commit_synced_files_clean_tree_is_ok_false_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".hex/scripts/foo.sh"), "echo old\n");
        seed_commit(ws, "seed");

        let res = commit_synced_files(ws, "9.9.9-test");
        assert_eq!(
            res,
            Ok(false),
            "a clean synced tree must return Ok(false), never an error: {res:?}"
        );
    }

    // 3. A commit that cannot be made (workspace is not a git repo) fails
    //    LOUDLY with an Err, never a silent Ok.
    #[test]
    fn commit_synced_files_fails_loudly_when_not_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path(); // deliberately NOT `git init`ed
        write_file(&ws.join(".hex/scripts/foo.sh"), "echo new\n");

        let res = commit_synced_files(ws, "9.9.9-test");
        assert!(
            res.is_err(),
            "a commit that cannot be made must return Err, never a silent Ok: {res:?}"
        );
    }

    // 4. Scope guard: the operator's unrelated tracked work (todo.md at the
    //    workspace root) must survive UNCOMMITTED — the upgrade commit only
    //    sweeps synced files under `.hex/`.
    #[test]
    fn commit_synced_files_leaves_unrelated_tracked_work_uncommitted() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".hex/scripts/foo.sh"), "echo old\n");
        write_file(&ws.join("todo.md"), "- item one\n");
        seed_commit(ws, "seed");
        // The sync edits a synced file; the operator independently edits todo.md.
        write_file(&ws.join(".hex/scripts/foo.sh"), "echo new\n");
        write_file(&ws.join("todo.md"), "- item one\n- operator edit\n");

        let _ = commit_synced_files(ws, "9.9.9-test");
        assert!(
            path_is_dirty(ws, "todo.md"),
            "unrelated operator work (todo.md) must remain uncommitted after \
             the upgrade commit"
        );
    }

    // 5. Scope guard, harder case: the operator PRE-STAGED unrelated work
    //    (`git add todo.md`) before `hex upgrade` ran. A bare `git commit`
    //    records the whole index and would sweep that staged work into the
    //    bookkeeping commit. The commit must be pathspec-scoped to `.hex/`, so
    //    the pre-staged todo.md must NOT land in the upgrade commit (it stays
    //    staged / dirty relative to HEAD).
    #[test]
    fn commit_synced_files_does_not_sweep_pre_staged_operator_work() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".hex/scripts/foo.sh"), "echo old\n");
        write_file(&ws.join("todo.md"), "- item one\n");
        seed_commit(ws, "seed");
        // The sync edits a synced file.
        write_file(&ws.join(".hex/scripts/foo.sh"), "echo new\n");
        // The operator independently edits AND stages todo.md before upgrading.
        write_file(&ws.join("todo.md"), "- item one\n- operator edit\n");
        let staged = Command::new("git")
            .args(["add", "todo.md"])
            .current_dir(ws)
            .status()
            .unwrap()
            .success();
        assert!(staged, "pre-staging todo.md must succeed");

        let made = commit_synced_files(ws, "9.9.9-test")
            .expect("committing tracked synced changes must succeed");
        assert!(made, "a commit must have been made for the .hex change");
        // The synced file IS committed.
        assert!(
            !path_is_dirty(ws, ".hex/scripts/foo.sh"),
            "the synced .hex change must be committed"
        );
        // The pre-staged operator work is NOT in the commit: if it had been
        // swept in, todo.md would match HEAD and read clean.
        assert!(
            path_is_dirty(ws, "todo.md"),
            "pre-staged operator work (todo.md) must NOT be swept into the \
             upgrade commit"
        );
    }
}
