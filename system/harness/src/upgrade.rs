//! Real port of the hex upgrade subcommand.
//!
//! Upgrades: scripts, skills, commands, hooks
//! Preserves: memory.db, settings.local.json, user data, AGENTS.md
//!
//! Drift bug fix: the bash shim omitted hooks sync for v2 layout. This
//! implementation syncs hooks unconditionally.

use std::collections::{HashMap, HashSet};
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
    /// A source file with a fixed managed destination. It participates in the
    /// same inventory as directory trees; it is not a build-only side copy.
    managed_cargo_bridge: PathBuf,
    version_txt: Option<PathBuf>,
}

struct ManagedFile<'a> {
    source: &'a Path,
    destination: PathBuf,
    label: &'a str,
}

fn managed_files<'a>(sources: &'a SourceDirs, hex_dir: &Path) -> Vec<ManagedFile<'a>> {
    vec![ManagedFile {
        source: &sources.managed_cargo_bridge,
        destination: hex_dir.join(".hex/managed_cargo_bridge.rs"),
        label: "managed_cargo_bridge.rs",
    }]
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
            managed_cargo_bridge: source_root.join("system/managed_cargo_bridge.rs"),
            version_txt: Some(source_root.join("system/version.txt")),
        }),
        _ => None,
    }
}

fn walk_files_checked(dir: &Path) -> io::Result<Vec<PathBuf>> {
    WalkDir::new(dir)
        .into_iter()
        .filter_entry(|entry| {
            !matches!(
                entry.file_name().to_str(),
                Some(".git" | "target" | "node_modules" | "__pycache__")
            )
        })
        .map(|entry| {
            let entry = entry.map_err(io::Error::other)?;
            let is_file = entry.file_type().is_file();
            let in_pycache = entry
                .path()
                .components()
                .any(|c| c.as_os_str() == "__pycache__");
            Ok((is_file && !in_pycache).then(|| entry.path().to_path_buf()))
        })
        .filter_map(|result| result.transpose())
        .collect()
}

fn files_differ(a: &Path, b: &Path) -> bool {
    match (fs::read(a), fs::read(b)) {
        (Ok(ac), Ok(bc)) => ac != bc,
        _ => true,
    }
}

fn copy_file_with_perms(src: &Path, dst: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dst, bytes)?;
    let src_mode = fs::metadata(src)?.permissions().mode();
    if src_mode & 0o111 != 0 {
        let mut perms = fs::metadata(dst)?.permissions();
        perms.set_mode(src_mode & 0o777);
        fs::set_permissions(dst, perms)?;
    }
    Ok(())
}

fn read_file_state(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn backup_path(
    backup_dir: &Path,
    dst_dir: &Path,
    relative: &Path,
    scope_root: Option<&Path>,
) -> PathBuf {
    let scope = scope_root
        .and_then(|root| dst_dir.strip_prefix(root).ok())
        .filter(|path| !path.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| {
            backup_dir
                .parent()
                .and_then(|root| dst_dir.strip_prefix(root).ok())
                .filter(|path| !path.as_os_str().is_empty())
                .map(Path::to_path_buf)
        })
        .or_else(|| dst_dir.file_name().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("scope"));
    backup_dir.join(scope).join(relative)
}

/// Detect which files in src_dir differ from dst_dir.
/// Returns (changed, new_count, unchanged, log_lines).
fn detect_changes(
    src_dir: &Path,
    dst_dir: &Path,
    label: &str,
) -> io::Result<(usize, usize, usize, Vec<String>)> {
    if !src_dir.exists() {
        return Ok((0, 0, 0, vec![]));
    }
    let mut changed = 0;
    let mut new_count = 0;
    let mut unchanged = 0;
    let mut log = Vec::new();

    for src_file in walk_files_checked(src_dir)? {
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
    Ok((changed, new_count, unchanged, log))
}

/// Count stale files for a managed destination during preflight. Additive
/// runtime trees intentionally skip this check because they are not pruned.
fn detect_stale_changes(
    src_dir: &Path,
    dst_dir: &Path,
    label: &str,
) -> io::Result<(usize, Vec<String>)> {
    // A missing source means this scope is not present in the selected
    // layout. The apply deletion pass has the same policy and must not let
    // preflight claim work that apply will skip.
    if !src_dir.exists() || !dst_dir.exists() {
        return Ok((0, vec![]));
    }
    let mut stale = 0;
    let mut log = Vec::new();
    for dst_file in walk_files_checked(dst_dir)? {
        let rel = match dst_file.strip_prefix(dst_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel.to_string_lossy().contains("settings.local.json") {
            continue;
        }
        if !src_dir.join(rel).exists() {
            stale += 1;
            log.push(format!("  - {label}/{}", rel.to_string_lossy()));
        }
    }
    Ok((stale, log))
}

fn detect_managed_changes(
    src_dir: &Path,
    dst_dir: &Path,
    label: &str,
    prune: bool,
) -> io::Result<(usize, usize, usize, Vec<String>)> {
    let (mut changed, new_count, unchanged, mut log) = detect_changes(src_dir, dst_dir, label)?;
    if prune {
        let (stale, stale_log) = detect_stale_changes(src_dir, dst_dir, label)?;
        changed += stale;
        log.extend(stale_log);
    }
    Ok((changed, new_count, unchanged, log))
}

fn detect_managed_file(file: &ManagedFile<'_>) -> io::Result<(usize, usize, usize, Vec<String>)> {
    if !file.source.exists() {
        return Ok((0, 0, 0, vec![]));
    }
    if !file.destination.exists() {
        return Ok((0, 1, 0, vec![format!("  + {}", file.label)]));
    }
    if files_differ(file.source, &file.destination) {
        return Ok((1, 0, 0, vec![format!("  ~ {}", file.label)]));
    }
    Ok((0, 0, 1, vec![]))
}

fn apply_managed_file_protected(
    file: &ManagedFile<'_>,
    backup_dir: Option<&Path>,
    protection: Option<(&Path, &UpgradeGitSnapshot)>,
    mut owned: Option<&mut HashMap<PathBuf, Option<Vec<u8>>>>,
) -> io::Result<usize> {
    if !file.source.exists() {
        return Ok(0);
    }
    if let Some((workspace, snapshot)) = protection {
        protect_sync_path(
            workspace,
            &file.destination,
            Some(file.source),
            snapshot,
            owned.as_deref(),
        )?;
    }
    if file.destination.exists() && files_differ(file.source, &file.destination) {
        if let Some(backup_dir) = backup_dir {
            let relative = file.destination.file_name().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "managed destination has no filename",
                )
            })?;
            let backup = backup_path(
                backup_dir,
                file.destination.parent().unwrap(),
                Path::new(relative),
                protection.map(|(root, _)| root),
            );
            if let Some(parent) = backup.parent() {
                fs::create_dir_all(parent)?;
            }
            if !owned
                .as_deref()
                .is_some_and(|paths| paths.contains_key(&file.destination))
            {
                fs::copy(&file.destination, backup)?;
            }
        }
    }
    if !file.destination.exists() || files_differ(file.source, &file.destination) {
        let bytes = fs::read(file.source)?;
        copy_file_with_perms(file.source, &file.destination, &bytes)?;
        if let Some(paths) = owned.as_deref_mut() {
            paths.insert(file.destination.clone(), Some(bytes));
        }
        return Ok(1);
    }
    Ok(0)
}

/// Sync src_dir into dst_dir. Backs up overwritten files into backup_dir if provided.
/// Returns count of files written.
#[cfg(test)]
pub fn apply_sync(src_dir: &Path, dst_dir: &Path, backup_dir: Option<&Path>) -> io::Result<usize> {
    apply_sync_protected(src_dir, dst_dir, backup_dir, None, None)
}

fn apply_sync_protected(
    src_dir: &Path,
    dst_dir: &Path,
    backup_dir: Option<&Path>,
    protection: Option<(&Path, &UpgradeGitSnapshot)>,
    mut owned: Option<&mut HashMap<PathBuf, Option<Vec<u8>>>>,
) -> io::Result<usize> {
    if !src_dir.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for src_file in walk_files_checked(src_dir)? {
        let rel = match src_file.strip_prefix(src_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel.to_string_lossy().contains("settings.local.json") {
            continue;
        }
        let dst_file = dst_dir.join(rel);
        if let Some((workspace, snapshot)) = protection {
            protect_sync_path(
                workspace,
                &dst_file,
                Some(&src_file),
                snapshot,
                owned.as_deref(),
            )?;
        }
        if let Some(bak) = backup_dir {
            if dst_file.exists() && files_differ(&src_file, &dst_file) {
                let bak_file = backup_path(bak, dst_dir, rel, protection.map(|(root, _)| root));
                if let Some(p) = bak_file.parent() {
                    fs::create_dir_all(p)?;
                }
                if !owned
                    .as_deref()
                    .is_some_and(|paths| paths.contains_key(&dst_file))
                {
                    let mut backup = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&bak_file)?;
                    io::copy(&mut fs::File::open(&dst_file)?, &mut backup)?;
                }
            }
        }
        if !dst_file.exists() || files_differ(&src_file, &dst_file) {
            let source_bytes = fs::read(&src_file)?;
            copy_file_with_perms(&src_file, &dst_file, &source_bytes)?;
            if let Some(paths) = owned.as_deref_mut() {
                paths.insert(dst_file.clone(), Some(source_bytes));
            }
            count += 1;
        }
    }
    Ok(count)
}

/// Remove files in dst_dir that are absent from src_dir. Backs them up first.
#[cfg(test)]
pub fn deletion_pass(dst_dir: &Path, src_dir: &Path, backup_dir: &Path) -> io::Result<usize> {
    deletion_pass_protected(dst_dir, src_dir, backup_dir, None, None)
}

fn deletion_pass_protected(
    dst_dir: &Path,
    src_dir: &Path,
    backup_dir: &Path,
    protection: Option<(&Path, &UpgradeGitSnapshot)>,
    mut owned: Option<&mut HashMap<PathBuf, Option<Vec<u8>>>>,
) -> io::Result<usize> {
    if !dst_dir.exists() || !src_dir.exists() {
        return Ok(0);
    }
    let mut deleted = 0;
    for dst_file in walk_files_checked(dst_dir)? {
        let rel = match dst_file.strip_prefix(dst_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !src_dir.join(rel).exists() {
            if let Some((workspace, snapshot)) = protection {
                protect_sync_path(workspace, &dst_file, None, snapshot, owned.as_deref())?;
            }
            let bak_file = backup_path(backup_dir, dst_dir, rel, protection.map(|(root, _)| root));
            if let Some(p) = bak_file.parent() {
                fs::create_dir_all(p)?;
            }
            if !owned
                .as_deref()
                .is_some_and(|paths| paths.contains_key(&dst_file))
            {
                let mut backup = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&bak_file)?;
                io::copy(&mut fs::File::open(&dst_file)?, &mut backup)?;
            }
            fs::remove_file(&dst_file)?;
            if let Some(paths) = owned.as_deref_mut() {
                paths.insert(dst_file.clone(), None);
            }
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

fn make_scripts_executable(owned: &HashMap<PathBuf, Option<Vec<u8>>>) -> io::Result<()> {
    for (path, expected) in owned {
        if expected.is_some() && path.extension().and_then(|e| e.to_str()) == Some("sh") {
            if read_file_state(path)? != *expected {
                return Err(io::Error::other(format!(
                    "operator edit before chmod: {}",
                    path.display()
                )));
            }
            let meta = fs::metadata(path)?;
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o111);
            fs::set_permissions(path, perms)?;
        }
    }
    Ok(())
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

    // The cache refresh above succeeded (pulled or freshly cloned). Best-effort
    // sweep of any `.upgrade-cache.corrupt-*` residue a prior clear_cache_dir()
    // left behind — never lets residue block or fail this upgrade.
    let _ = sweep_corrupt_cache_residue(hex_dir);

    println!("  [OK] Source ready");
    Ok(cache_dir)
}

/// Remove any `.upgrade-cache.corrupt-*` directories under `<hex_dir>/.hex`
/// left behind by a prior `clear_cache_dir()` move-aside. Best-effort: a
/// directory that can't be removed (e.g. OS-protected) is logged with a WARN
/// and returned in `failed`, never turned into an upgrade failure. The live
/// `.upgrade-cache` dir itself is never touched — only names starting with
/// `.upgrade-cache.corrupt-` match.
fn sweep_corrupt_cache_residue(hex_dir: &Path) -> (usize, Vec<PathBuf>) {
    let dot_hex = hex_dir.join(".hex");
    let mut removed = 0usize;
    let mut failed = Vec::new();

    let entries = match fs::read_dir(&dot_hex) {
        Ok(e) => e,
        Err(_) => return (removed, failed),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let is_residue = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with(".upgrade-cache.corrupt-"))
            .unwrap_or(false);
        if !is_residue {
            continue;
        }
        if fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        } else {
            println!(
                "  [WARN] Could not remove upgrade-cache residue at {}",
                path.display()
            );
            failed.push(path);
        }
    }

    (removed, failed)
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

fn record_upgrade_sha(
    config_file: &Path,
    source_dir: &Path,
    repo_url: &str,
    protection: Option<(&Path, &UpgradeGitSnapshot)>,
    owned: Option<&mut HashMap<PathBuf, Option<Vec<u8>>>>,
) -> Result<(), String> {
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

    let Some(sha) = sha else {
        return Err(format!(
            "could not read source SHA from {}",
            source_dir.display()
        ));
    };

    let mut data: serde_json::Value = if config_file.exists() {
        let content = fs::read_to_string(config_file)
            .map_err(|e| format!("could not read {}: {e}", config_file.display()))?;
        serde_json::from_str(&content)
            .map_err(|e| format!("could not parse {}: {e}", config_file.display()))?
    } else {
        serde_json::json!({ "repo": repo_url })
    };

    data["last_remote_sha"] = serde_json::Value::String(sha.clone());
    let tmp = config_file.with_extension("tmp");
    let s = serde_json::to_string_pretty(&data)
        .map_err(|e| format!("could not encode {}: {e}", config_file.display()))?;
    let serialized = format!("{s}\n");
    if let Some((workspace, snapshot)) = protection {
        protect_generated_path(
            workspace,
            config_file,
            serialized.as_bytes(),
            snapshot,
            owned.as_deref(),
        )
        .map_err(|e| format!("upgrade.json operator edit conflict: {e}"))?;
    }
    fs::write(&tmp, &serialized).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, config_file)
        .map_err(|e| format!("could not install {}: {e}", config_file.display()))?;
    if let Some(paths) = owned {
        paths.insert(config_file.to_path_buf(), Some(serialized.into_bytes()));
    }
    // `sha` is `git rev-parse HEAD` output — hex ASCII, every byte a char boundary.
    #[allow(clippy::string_slice)]
    {
        println!("  → Recorded upgrade SHA: {}...", &sha[..sha.len().min(8)]);
    }
    Ok(())
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

/// Parse the package field, not an arbitrary line beginning with `version`.
fn read_cargo_version(path: &Path) -> io::Result<String> {
    let content = fs::read_to_string(path)?;
    let manifest: toml::Value =
        toml::from_str(&content).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    manifest
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(toml::Value::as_str)
        .filter(|version| !version.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("missing package.version in {}", path.display()),
            )
        })
}

/// An absent binary needs installation. A failed probe cannot prove freshness.
fn read_installed_version(path: &Path) -> io::Result<Option<String>> {
    let output = match Command::new(path).arg("--version").output() {
        Ok(output) => output,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{} --version failed with {}",
            path.display(),
            output.status
        )));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut words = text.split_whitespace();
    if words.next() != Some("hex") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid hex version output",
        ));
    }
    words
        .next()
        .map(|version| Some(version.to_owned()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing installed hex version"))
}

/// Read optional installed metadata without confusing an I/O failure with
/// an intentionally absent file. An installed SHA is optional for prebuilt
/// binaries, but a present unreadable file is an upgrade failure.
fn read_optional_utf8(path: &Path) -> io::Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(|text| Some(text.trim().to_owned()))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Read the source commit used to prove the rebuilt binary's provenance.
fn read_source_sha(source_dir: &Path) -> io::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source_dir)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git rev-parse HEAD failed with {}",
            output.status
        )));
    }
    let sha = String::from_utf8(output.stdout)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .trim()
        .to_owned();
    if sha.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "git rev-parse HEAD returned no commit",
        ));
    }
    Ok(sha)
}

/// Gather inputs and decide whether the binary is stale. Returns false
/// ("nothing to do") when VERSIONS or Cargo.toml is absent, matching
/// `sync_versions_file`'s own preconditions — if those are missing the
/// rebuild step no-ops anyway, so the gate shouldn't proceed on its account.
fn binary_is_stale(hex_dir: &Path, source_dir: &Path) -> io::Result<bool> {
    let versions_file = hex_dir.join("VERSIONS");
    let cargo_toml = source_dir.join("system/harness/Cargo.toml");
    if !versions_file.exists() || !cargo_toml.exists() {
        return Ok(false);
    }
    let signing_mode = hex::app_identity::prepare_upgrade(hex_dir, source_dir)?;
    if signing_mode == hex::app_identity::UpgradeMode::Migrate {
        return Ok(true);
    }
    // VERSIONS is required by the apply step even when every selected file
    // is unchanged. Validate it before allowing the no-op fast path.
    fs::read_to_string(&versions_file)?;
    let cargo_ver = read_cargo_version(&cargo_toml)?;
    let hex_dot_dir = hex_dir.join(".hex");
    let installed_ver = read_installed_version(&hex_dot_dir.join("bin/hex"))?;
    let installed_sha = match signing_mode {
        hex::app_identity::UpgradeMode::Signed(revision) => Some(revision),
        _ => read_optional_utf8(&hex_dot_dir.join("bin/hex.sha"))?,
    };
    let source_sha = read_source_sha(source_dir)?;
    Ok(binary_needs_rebuild(
        installed_ver.as_deref(),
        &cargo_ver,
        installed_sha.as_deref(),
        Some(&source_sha),
    ))
}

/// Report whether the managed foundation pin needs reconciliation. This is a
/// metadata-only change and must not force a binary rebuild, but it must keep
/// the no-op preflight from skipping the VERSIONS sync.
fn versions_pin_is_stale(hex_dir: &Path, source_dir: &Path) -> io::Result<bool> {
    let versions_file = hex_dir.join("VERSIONS");
    let cargo_toml = source_dir.join("system/harness/Cargo.toml");
    if !versions_file.exists() || !cargo_toml.exists() {
        return Ok(false);
    }
    let existing = fs::read_to_string(&versions_file)?;
    let cargo_ver = read_cargo_version(&cargo_toml)?;
    let expected = format!("HEX_FOUNDATION_VERSION=v{cargo_ver}");
    let mut found = 0;
    for line in existing.lines() {
        if line.trim_start().starts_with("HEX_FOUNDATION_VERSION=") {
            found += 1;
            if line.trim() != expected {
                return Ok(true);
            }
        }
    }
    Ok(found != 1)
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
#[cfg(test)]
fn sync_versions_file(
    hex_dir: &Path,
    source_dir: &Path,
    backup_dir: &Path,
) -> Result<(), BinaryStepFailure> {
    sync_versions_file_protected(hex_dir, source_dir, backup_dir, None, None)
}

fn sync_versions_file_protected(
    hex_dir: &Path,
    source_dir: &Path,
    backup_dir: &Path,
    protection: Option<(&Path, &UpgradeGitSnapshot)>,
    mut owned: Option<&mut HashMap<PathBuf, Option<Vec<u8>>>>,
) -> Result<(), BinaryStepFailure> {
    let versions_file = hex_dir.join("VERSIONS");
    if !versions_file.exists() {
        return Ok(());
    }
    let cargo_toml = source_dir.join("system/harness/Cargo.toml");
    if !cargo_toml.exists() {
        return Ok(());
    }
    let cargo_ver = read_cargo_version(&cargo_toml).map_err(|e| {
        eprintln!("  [FAIL] Could not read package version from Cargo.toml: {e}");
        BinaryStepFailure::Build
    })?;
    let signing_mode = hex::app_identity::prepare_upgrade(hex_dir, source_dir).map_err(|e| {
        eprintln!("  [FAIL] App identity preflight failed: {e}");
        BinaryStepFailure::Build
    })?;

    // Preserve every existing line — comments, blank lines, and any
    // KEY=VALUE we do not manage (BOI_VERSION, custom instance pins,
    // repo overrides, etc.). Only update the managed keys we own in
    // place, or append them if missing. Regression: previous behavior
    // rewrote the file with only HEX_FOUNDATION_VERSION, destroying
    // unmanaged pins like BOI_VERSION that install.sh parity reads
    // (2026-07-16 audit).
    let existing = fs::read_to_string(&versions_file).map_err(|e| {
        eprintln!("  [FAIL] Could not read VERSIONS: {e}");
        BinaryStepFailure::Build
    })?;
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

    // Do not mutate a managed destination until the candidate binary builds.
    // The source tree below includes every build input, including the bridge
    // module, so a failed build leaves the instance source and VERSIONS intact.
    let install_versions = |owned: &mut Option<&mut HashMap<PathBuf, Option<Vec<u8>>>>| -> Result<(), BinaryStepFailure> {
        let tmp = versions_file.with_extension("tmp");
        if let Some((workspace, snapshot)) = protection {
            protect_generated_path(workspace, &versions_file, new_content.as_bytes(), snapshot, owned.as_deref())
                .map_err(|e| {
                    eprintln!("  [FAIL] VERSIONS operator edit conflict: {e}");
                    BinaryStepFailure::Build
                })?;
        }
        fs::write(&tmp, &new_content).map_err(|e| {
            eprintln!("  [FAIL] Could not write {}: {e}", tmp.display());
            BinaryStepFailure::Build
        })?;
        fs::rename(&tmp, &versions_file).map_err(|e| {
            eprintln!("  [FAIL] Could not install {}: {e}", versions_file.display());
            BinaryStepFailure::Build
        })?;
        if let Some(paths) = owned.as_deref_mut() {
            paths.insert(versions_file.clone(), Some(new_content.clone().into_bytes()));
        }
        println!("  [OK] VERSIONS → HEX_FOUNDATION_VERSION=v{cargo_ver}");
        Ok(())
    };

    // Rebuild hex binary if version or commit SHA changed
    let hex_dot_dir = hex_dir.join(".hex");
    let installed_bin = hex_dot_dir.join("bin/hex");
    let installed_sha_file = hex_dot_dir.join("bin/hex.sha");

    let installed_ver = if signing_mode == hex::app_identity::UpgradeMode::Migrate {
        None
    } else {
        read_installed_version(&installed_bin).map_err(|e| {
            eprintln!("  [FAIL] Could not read installed hex version: {e}");
            BinaryStepFailure::Build
        })?
    };

    let installed_sha = if let hex::app_identity::UpgradeMode::Signed(ref revision) = signing_mode {
        Some(revision.clone())
    } else {
        read_optional_utf8(&installed_sha_file).map_err(|e| {
            eprintln!("  [FAIL] Could not read installed SHA: {e}");
            BinaryStepFailure::Build
        })?
    };
    let source_sha = Some(read_source_sha(source_dir).map_err(|e| {
        eprintln!("  [FAIL] Could not read source SHA: {e}");
        BinaryStepFailure::Build
    })?);

    // `version_mismatch` drives the human-readable reason below; the actual
    // rebuild decision is `binary_needs_rebuild` (shared with the upstream gate).
    let version_mismatch = installed_ver.as_deref() != Some(&cargo_ver);

    if binary_needs_rebuild(
        installed_ver.as_deref(),
        &cargo_ver,
        installed_sha.as_deref(),
        source_sha.as_deref(),
    ) {
        let harness_dst = source_dir.join("system/harness");
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
        // Build directly from the selected source checkout. Its sibling
        // code-intel crate and bridge module are therefore staged together
        // without writing into the live instance before cargo succeeds.

        // Detect a personal overlay and build with --features personal (and set
        // HEX_DIR so build.rs can find it). Keyed on overlay PRESENCE — a
        // `harness-personal/` dir (integration probes) or a `modules/` dir
        // (personal workers) — NOT a specific file, so it survives files being
        // added/removed/re-homed (e.g. release.rs leaving the binary).
        let use_personal = detect_personal_overlay(&hex_dot_dir);
        let mut build_args = vec!["build", "--release"];
        // Build output is private staging, so a failed build cannot mutate the
        // installed harness source tree.
        let target_dir = backup_dir.join("harness-build-target");
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
                let release_bin = target_dir.join("release/hex");
                // Re-check after compilation. A newly configured or previously
                // signed app must not fall through to legacy raw installation.
                let final_mode =
                    hex::app_identity::prepare_upgrade(hex_dir, source_dir).map_err(|e| {
                        eprintln!("  [FAIL] App install preflight failed: {e}");
                        BinaryStepFailure::Build
                    })?;
                if signing_mode != hex::app_identity::UpgradeMode::Legacy
                    && final_mode == hex::app_identity::UpgradeMode::Legacy
                {
                    eprintln!("  [FAIL] Signed installation evidence disappeared during build");
                    return Err(BinaryStepFailure::Build);
                }
                let managed = final_mode != hex::app_identity::UpgradeMode::Legacy;
                let installed = if managed {
                    hex::app_identity::install_build(
                        hex_dir,
                        source_dir,
                        &release_bin,
                        &cargo_ver,
                        source_sha.as_deref().ok_or(BinaryStepFailure::Build)?,
                    )
                } else {
                    atomic_install_binary(&release_bin, &installed_bin)
                };
                match installed {
                    Ok(()) => {
                        println!("  [OK] hex binary rebuilt and swapped (atomic): v{cargo_ver}");
                        if let Some(sha) = source_sha.as_ref().filter(|_| !managed) {
                            if let Some((workspace, snapshot)) = protection {
                                protect_generated_path(
                                    workspace,
                                    &installed_sha_file,
                                    sha.as_bytes(),
                                    snapshot,
                                    owned.as_deref(),
                                )
                                .map_err(|e| {
                                    eprintln!("  [FAIL] Installed SHA conflict: {e}");
                                    BinaryStepFailure::Build
                                })?;
                            }
                            let sha_tmp = installed_sha_file.with_extension("tmp");
                            fs::write(&sha_tmp, sha).map_err(|e| {
                                eprintln!("  [FAIL] Could not write installed SHA: {e}");
                                BinaryStepFailure::Build
                            })?;
                            fs::rename(&sha_tmp, &installed_sha_file).map_err(|e| {
                                eprintln!("  [FAIL] Could not install installed SHA: {e}");
                                BinaryStepFailure::Build
                            })?;
                            if let Some(paths) = owned.as_deref_mut() {
                                paths.insert(
                                    installed_sha_file.clone(),
                                    Some(sha.as_bytes().to_vec()),
                                );
                            }
                            // `sha` is `git rev-parse HEAD` output — hex ASCII, every byte a char boundary.
                            #[allow(clippy::string_slice)]
                            {
                                println!(
                                    "  → Recorded installed SHA: {}...",
                                    &sha[..sha.len().min(8)]
                                );
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
                        build_and_install_code_intel(&hex_dot_dir, managed);
                        // The binary WAS swapped. If the harness restart failed,
                        // the running harness still holds the OLD binary in memory
                        // — propagate that as a DISTINCT failure kind so run()
                        // prints the "swapped but restart FAILED" message and exits
                        // nonzero (the 2026-06-12 stale-harness incident), never
                        // the build-failure wording (the binary was in fact updated).
                        if let Err(e) = restart_result {
                            return Err(BinaryStepFailure::RestartFailed(e));
                        }
                        install_versions(&mut owned)
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
        install_versions(&mut owned)
    }
}

/// Build the synced `.hex/code-intel` crate and atomically install its `cq`
/// and `scipd` binaries into `.hex/bin/`, alongside `hex`. Mirrors the harness
/// rebuild: `--target-dir` pinned to the crate's own `target/` so the output
/// location is deterministic regardless of workspace nesting (OBS-017), and
/// `atomic_install_binary` for the swap (codesign + rename, never mutates the
/// live inode). Best-effort: warns loudly on failure, never fails the upgrade.
fn build_and_install_code_intel(hex_dot_dir: &Path, preserve_identity: bool) {
    if preserve_identity {
        // Managed companions have a separate required, resumable step in run().
        return;
    }
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
fn restart_harness_with<F>(
    hex_dir: &Path,
    agent_installed: bool,
    restart_fn: F,
) -> Result<(), String>
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

/// Point `core.hooksPath` at the committed `.githooks/` dir so the leak-guard
/// pre-commit hook fires for clones and upgraded instances without a manual
/// `git config` step. Idempotent: a no-op when already set to `.githooks`, and
/// a no-op when the repo carries no `.githooks/` dir. Failures are surfaced
/// loudly (S6: no quiet failures) but never abort the upgrade — a missing
/// hooks wiring must not block a version sync. `hex doctor`'s `git-hookspath`
/// check is the standing backstop that flags an unwired repo.
fn configure_hooks_path(workspace: &Path) {
    let githooks = workspace.join(".githooks");
    if !githooks.is_dir() {
        return;
    }
    let current = Command::new("git")
        .args(["config", "--get", "core.hooksPath"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if current == ".githooks" {
        return; // already wired
    }
    match Command::new("git")
        .args(["config", "core.hooksPath", ".githooks"])
        .current_dir(workspace)
        .output()
    {
        Ok(o) if o.status.success() => {
            println!("  [OK] Wired core.hooksPath → .githooks (leak-guard pre-commit hook).");
        }
        Ok(o) => {
            eprintln!(
                "  [WARN] Could not set core.hooksPath in {}: {}",
                workspace.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Err(e) => {
            eprintln!(
                "  [WARN] Could not run `git config core.hooksPath` in {}: {e}",
                workspace.display()
            );
        }
    }
}

/// Paths already dirty when the upgrade started are never eligible for the
/// upgrade bookkeeping commit. This snapshot is intentionally small: it
/// records only porcelain paths under `.hex`, including untracked operator
/// files, and leaves their index/worktree state untouched.
#[derive(Debug, Default)]
struct UpgradeGitSnapshot {
    preexisting_paths: HashSet<PathBuf>,
    baseline_content: HashMap<PathBuf, Option<Vec<u8>>>,
}

#[cfg(test)]
fn upgrade_git_snapshot(workspace: &Path) -> Result<UpgradeGitSnapshot, String> {
    let mut planned = Vec::new();
    for root in [workspace.join(".hex"), workspace.join(".claude/commands")] {
        if root.exists() {
            planned.extend(walk_files_checked(&root).map_err(|e| e.to_string())?);
        }
    }
    planned.push(workspace.join("VERSIONS"));
    upgrade_git_snapshot_for(workspace, &planned)
}

fn upgrade_dirty_paths(workspace: &Path) -> Result<HashSet<PathBuf>, String> {
    let output = Command::new("git")
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--no-renames",
            "--untracked-files=all",
            "--",
            ".hex",
            "VERSIONS",
            ".claude/commands",
        ])
        .current_dir(workspace)
        .output()
        .map_err(|e| format!("could not run git status in {}: {e}", workspace.display()))?;
    if !output.status.success() {
        return Err(format!(
            "git status failed in {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut paths = HashSet::new();
    for record in output.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if record.len() < 4 {
            return Err("git status returned a malformed porcelain record".to_string());
        }
        let path = std::str::from_utf8(&record[3..])
            .map_err(|_| "git status returned a non-UTF-8 path".to_string())?;
        let relative = PathBuf::from(path);
        paths.insert(relative);
    }
    Ok(paths)
}

/// Read only the exact destinations admitted by the operation inventory.
/// Explicit absence is a baseline; an unknown path is never assumed absent.
fn upgrade_git_snapshot_for(
    workspace: &Path,
    planned_paths: &[PathBuf],
) -> Result<UpgradeGitSnapshot, String> {
    let preexisting_paths = upgrade_dirty_paths(workspace)?;
    let mut baseline_content = HashMap::new();
    for path in planned_paths {
        let relative = path
            .strip_prefix(workspace)
            .map_err(|_| format!("snapshot path escaped workspace: {}", path.display()))?;
        baseline_content.insert(
            relative.to_path_buf(),
            read_file_state(path)
                .map_err(|e| format!("could not snapshot {}: {e}", path.display()))?,
        );
    }
    Ok(UpgradeGitSnapshot {
        preexisting_paths,
        baseline_content,
    })
}

/// Enumerate source-selected writes and only the established deletion scopes.
/// Additive runtime directories are never traversed on the destination side.
fn planned_upgrade_paths(
    workspace: &Path,
    source_dir: &Path,
    sources: &SourceDirs,
) -> io::Result<Vec<PathBuf>> {
    let hex = workspace.join(".hex");
    let pairs = [
        (sources.scripts.clone(), hex.join("scripts"), true),
        (sources.skills.clone(), hex.join("skills"), true),
        (sources.commands.clone(), hex.join("commands"), true),
        (sources.hooks.clone(), hex.join("hooks"), true),
        (sources.iii.clone(), hex.join("iii"), false),
        (sources.templates.clone(), hex.join("templates"), false),
        (
            sources.commands.clone(),
            workspace.join(".claude/commands"),
            true,
        ),
        (
            source_dir.join("system/harness"),
            hex.join("harness"),
            false,
        ),
        (
            source_dir.join("system/code-intel"),
            hex.join("code-intel"),
            false,
        ),
    ];
    let mut paths = HashSet::new();
    for (source, destination, prune) in &pairs {
        if !source.exists() {
            continue;
        }
        for file in walk_files_checked(source)? {
            let relative = file.strip_prefix(source).map_err(io::Error::other)?;
            if !relative.to_string_lossy().contains("settings.local.json") {
                paths.insert(destination.join(relative));
            }
        }
        let deletion_roots = if *prune {
            vec![(source.clone(), destination.clone())]
        } else if *destination == hex.join("harness") || *destination == hex.join("code-intel") {
            ["src", "tests"]
                .iter()
                .map(|sub| (source.join(sub), destination.join(sub)))
                .collect()
        } else {
            Vec::new()
        };
        for (source_root, destination_root) in deletion_roots {
            if !source_root.exists() || !destination_root.exists() {
                continue;
            }
            for file in walk_files_checked(&destination_root)? {
                let relative = file
                    .strip_prefix(&destination_root)
                    .map_err(io::Error::other)?;
                if !source_root.join(relative).exists() {
                    paths.insert(file);
                }
            }
        }
    }
    for file in managed_files(sources, workspace) {
        if file.source.exists() {
            paths.insert(file.destination);
        }
    }
    paths.extend([
        hex.join("version.txt"),
        hex.join("bin/hex.sha"),
        hex.join("upgrade.json"),
        workspace.join("VERSIONS"),
    ]);
    let mut paths: Vec<_> = paths.into_iter().collect();
    paths.sort();
    Ok(paths)
}

fn protect_sync_path(
    workspace: &Path,
    path: &Path,
    desired: Option<&Path>,
    snapshot: &UpgradeGitSnapshot,
    owned: Option<&HashMap<PathBuf, Option<Vec<u8>>>>,
) -> io::Result<()> {
    let desired = desired.map(fs::read).transpose()?;
    protect_write(workspace, path, desired.as_deref(), snapshot, owned)
}

fn protect_write(
    workspace: &Path,
    path: &Path,
    desired: Option<&[u8]>,
    snapshot: &UpgradeGitSnapshot,
    owned: Option<&HashMap<PathBuf, Option<Vec<u8>>>>,
) -> io::Result<()> {
    let relative = path.strip_prefix(workspace).map_err(io::Error::other)?;
    let original = snapshot.baseline_content.get(relative).ok_or_else(|| {
        io::Error::other(format!("unplanned upgrade destination: {}", path.display()))
    })?;
    let prior_owned = owned.and_then(|paths| paths.get(path));
    let before = prior_owned.unwrap_or(original);
    if read_file_state(path)? != *before {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("operator edit changed during upgrade: {}", path.display()),
        ));
    }
    if snapshot.preexisting_paths.contains(relative) {
        protect_sync_bytes(path, desired, original)?;
    }
    Ok(())
}

fn protect_sync_bytes(
    path: &Path,
    desired: Option<&[u8]>,
    before: &Option<Vec<u8>>,
) -> io::Result<()> {
    match desired {
        Some(bytes) if Some(bytes.to_vec()) == *before => Ok(()),
        None if before.is_none() => Ok(()),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("upgrade would overwrite operator edit: {}", path.display()),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("upgrade would delete operator edit: {}", path.display()),
        )),
    }
}

fn protect_generated_path(
    workspace: &Path,
    path: &Path,
    generated: &[u8],
    snapshot: &UpgradeGitSnapshot,
    owned: Option<&HashMap<PathBuf, Option<Vec<u8>>>>,
) -> io::Result<()> {
    protect_write(workspace, path, Some(generated), snapshot, owned)
}

/// After a successful sync + rebuild, commit only files changed by this
/// upgrade under `.hex` in the instance workspace. Pre-existing dirty paths
/// are excluded, so the upgrade never consumes operator work.
///
/// The snapshot must be taken before any sync writes occur. A path that was
/// already dirty is left in the index/worktree exactly as the operator left
/// it. The scoped `git add` and `git commit --only` preserve unrelated staged
/// paths as well.
///
/// Returns:
///   Ok(true)  — a commit was made.
///   Ok(false) — the synced tree was already clean (no-op success, NOT an error).
///   Err(msg)  — the commit could not be made; the caller MUST surface this
///               LOUDLY (S6: no quiet failures), never a silent skip.
fn commit_synced_files_since(
    workspace: &Path,
    version: &str,
    snapshot: &UpgradeGitSnapshot,
    owned_paths: Option<&HashMap<PathBuf, Option<Vec<u8>>>>,
) -> Result<bool, String> {
    let after = upgrade_dirty_paths(workspace)?;
    let candidates: Vec<PathBuf> = after
        .difference(&snapshot.preexisting_paths)
        .cloned()
        .collect();
    let owned: Vec<PathBuf> = match owned_paths {
        Some(paths) => {
            for relative in &candidates {
                if let Some(expected) = paths.get(&workspace.join(relative)) {
                    let index = Command::new("git")
                        .args([
                            "diff",
                            "--cached",
                            "--quiet",
                            "--",
                            relative.to_string_lossy().as_ref(),
                        ])
                        .current_dir(workspace)
                        .status()
                        .map_err(|e| {
                            format!(
                                "could not inspect git index for {}: {e}",
                                relative.display()
                            )
                        })?;
                    if !index.success() {
                        return Err(format!(
                            "operator staged edit changed upgrade-owned path before commit: {}",
                            workspace.join(relative).display()
                        ));
                    }
                    let current = read_file_state(&workspace.join(relative))
                        .map_err(|e| format!("could not inspect {}: {e}", relative.display()))?;
                    if &current != expected {
                        return Err(format!(
                            "operator edit changed upgrade-owned path before commit: {}",
                            workspace.join(relative).display()
                        ));
                    }
                }
            }
            candidates
                .into_iter()
                .filter(|relative| paths.contains_key(&workspace.join(relative)))
                .collect()
        }
        None => candidates,
    };
    if owned.is_empty() {
        return Ok(false);
    }

    let mut add = Command::new("git");
    add.args(["add", "-A", "--"]);
    for path in &owned {
        add.arg(path);
    }
    let add = add
        .current_dir(workspace)
        .output()
        .map_err(|e| format!("could not run git add in {}: {e}", workspace.display()))?;
    if !add.status.success() {
        return Err(format!(
            "git add upgrade-owned .hex paths failed in {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&add.stderr).trim()
        ));
    }

    let msg = format!("chore(hex): sync harness files to v{version}");
    let mut commit = Command::new("git");
    commit.args([
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--no-verify",
        "--only",
        "-q",
        "-m",
        &msg,
        "--",
    ]);
    for path in &owned {
        commit.arg(path);
    }
    let commit = commit
        .current_dir(workspace)
        .output()
        .map_err(|e| format!("could not run git commit in {}: {e}", workspace.display()))?;
    if !commit.status.success() {
        return Err(format!(
            "git commit upgrade-owned .hex paths failed in {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&commit.stderr).trim()
        ));
    }
    Ok(true)
}

/// Compatibility wrapper for callers that do not have a pre-sync snapshot.
/// The full upgrade flow always uses `commit_synced_files_since`.
#[cfg(test)]
fn commit_synced_files(workspace: &Path, version: &str) -> Result<bool, String> {
    // Test-only compatibility seam. The production flow always supplies the
    // pre-sync snapshot; an empty snapshot preserves the historical helper's
    // direct-call behavior for older unit tests.
    let snapshot = UpgradeGitSnapshot::default();
    commit_synced_files_since(workspace, version, &snapshot, None)
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
    macro_rules! detect {
        ($src:expr, $dst:expr, $label:expr, $prune:expr) => {
            match detect_managed_changes($src, $dst, $label, $prune) {
                Ok(result) => result,
                Err(e) => {
                    eprintln!(
                        "  [FAIL] Could not inspect {} during preflight: {e}",
                        $label
                    );
                    return 1;
                }
            }
        };
    }
    let (c1, n1, u1, log1) = detect!(
        &src_dirs.scripts,
        &hex_dot_dir.join("scripts"),
        "scripts",
        true
    );
    let (c2, n2, u2, log2) = detect!(
        &src_dirs.skills,
        &hex_dot_dir.join("skills"),
        "skills",
        true
    );
    let (c3, n3, u3, log3) = detect!(
        &src_dirs.commands,
        &hex_dot_dir.join("commands"),
        "commands",
        true
    );
    let (c4, n4, u4, log4) = detect!(&src_dirs.hooks, &hex_dot_dir.join("hooks"), "hooks", true);
    // Additive dirs (iii engine config/workers, launchd + other templates)
    let (c5, n5, u5, log5) = detect!(&src_dirs.iii, &hex_dot_dir.join("iii"), "iii", false);
    let (c6, n6, u6, log6) = detect!(
        &src_dirs.templates,
        &hex_dot_dir.join("templates"),
        "templates",
        false
    );
    let (c7, n7, u7, log7) = detect!(
        &src_dirs.commands,
        &hex_dir.join(".claude/commands"),
        "command mirror",
        true
    );
    let bridge_files = managed_files(&src_dirs, &hex_dir);
    let (c8, n8, u8, log8) = match bridge_files.iter().map(detect_managed_file).try_fold(
        (0, 0, 0, Vec::new()),
        |(c, n, u, mut log), result| {
            result.map(|(next_c, next_n, next_u, next_log)| {
                log.extend(next_log);
                (c + next_c, n + next_n, u + next_u, log)
            })
        },
    ) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("  [FAIL] Could not inspect managed cargo bridge during preflight: {e}");
            return 1;
        }
    };

    let total_changed = c1 + c2 + c3 + c4 + c5 + c6 + c7 + c8;
    let total_new = n1 + n2 + n3 + n4 + n5 + n6 + n7 + n8;
    let total_unchanged = u1 + u2 + u3 + u4 + u5 + u6 + u7 + u8;

    println!("  → {total_changed} changed, {total_new} new, {total_unchanged} unchanged");
    for line in log1
        .iter()
        .chain(&log2)
        .chain(&log3)
        .chain(&log4)
        .chain(&log5)
        .chain(&log6)
        .chain(&log7)
        .chain(&log8)
    {
        println!("{line}");
    }

    // Check version.txt changes
    let mut version_changed = false;
    if let Some(src_ver_file) = &src_dirs.version_txt {
        if src_ver_file.exists() {
            let src_ver = match read_file_state(src_ver_file) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => Vec::new(),
                Err(e) => {
                    eprintln!(
                        "  [FAIL] Could not read source version metadata {}: {e}",
                        src_ver_file.display()
                    );
                    return 1;
                }
            };
            let dst_ver = match read_file_state(&hex_dot_dir.join("version.txt")) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => Vec::new(),
                Err(e) => {
                    eprintln!("  [FAIL] Could not read installed version metadata: {e}");
                    return 1;
                }
            };
            if src_ver != dst_ver {
                version_changed = true;
                println!(
                    "  ~ version.txt ({} → {})",
                    String::from_utf8_lossy(&dst_ver).trim(),
                    String::from_utf8_lossy(&src_ver).trim()
                );
            }
        }
    }

    // OBS-028: a binary-only change (Rust source moved, same Cargo version, no
    // synced files changed) must still trigger a rebuild. Without this the gate
    // below early-returns "Nothing to do" before Step 5 ever runs, and the
    // upgrade silently ships nothing while reporting success.
    let binary_stale = match binary_is_stale(&hex_dir, &source_dir) {
        Ok(stale) => stale,
        Err(e) => {
            eprintln!("  [FAIL] Could not inspect binary metadata during preflight: {e}");
            return 1;
        }
    };
    let versions_pin_stale = match versions_pin_is_stale(&hex_dir, &source_dir) {
        Ok(stale) => stale,
        Err(e) => {
            eprintln!("  [FAIL] Could not inspect foundation version pin during preflight: {e}");
            return 1;
        }
    };
    if versions_pin_stale {
        println!("  → VERSIONS foundation pin needs reconciliation.");
    }

    let companion_plan = match crate::codeintel_upgrade::inspect(&hex_dir, &source_dir) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("  [FAIL] Code-intel preflight failed: {e}");
            return 1;
        }
    };
    if companion_plan.needs_work() {
        println!("  → Code-intel needs a separate app, command path or service update.");
    }

    if total_changed == 0
        && total_new == 0
        && !version_changed
        && !binary_stale
        && !versions_pin_stale
        && !companion_plan.needs_work()
    {
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

    let git_snapshot = if is_own_git_toplevel(&hex_dir) {
        let planned = match planned_upgrade_paths(&hex_dir, &source_dir, &src_dirs) {
            Ok(paths) => paths,
            Err(e) => {
                eprintln!("  [FAIL] Could not inventory required upgrade paths: {e}");
                return 1;
            }
        };
        match upgrade_git_snapshot_for(&hex_dir, &planned) {
            Ok(snapshot) => Some(snapshot),
            Err(e) => {
                eprintln!("  [FAIL] Could not snapshot operator edits before upgrade: {e}");
                return 1;
            }
        }
    } else {
        None
    };

    // Step 4: Apply changes
    println!("\n4. Apply Changes");
    let backup_dir = hex_dot_dir.join(format!(".upgrade-backup-{}", now.format("%Y%m%d-%H%M%S")));
    if let Err(e) = fs::create_dir_all(&backup_dir) {
        eprintln!("  [FAIL] Could not create upgrade backup directory: {e}");
        return 1;
    }
    let mut failures = Vec::new();
    let protection = git_snapshot
        .as_ref()
        .map(|snapshot| (hex_dir.as_path(), snapshot));
    let mut owned_paths = HashMap::new();

    // Build the candidate from the selected source before mutating any live
    // managed file. This includes the path-imported bridge module. A failed
    // build exits here with the instance source tree still untouched.
    println!("\n4. Build Candidate");
    let binary_result = sync_versions_file_protected(
        &hex_dir,
        &source_dir,
        &backup_dir,
        protection,
        Some(&mut owned_paths),
    );
    if let Err(failure) = &binary_result {
        eprintln!("  {}", binary_step_failure_message(failure));
        println!();
        return 1;
    }

    // Step 5: Apply changes
    println!("\n5. Apply Changes");
    let sync_pairs: &[(&PathBuf, PathBuf)] = &[
        (&src_dirs.scripts, hex_dot_dir.join("scripts")),
        (&src_dirs.skills, hex_dot_dir.join("skills")),
        (&src_dirs.commands, hex_dot_dir.join("commands")),
        (&src_dirs.hooks, hex_dot_dir.join("hooks")),
    ];

    let mut applied = 0;
    for (src, dst) in sync_pairs {
        if src.exists() {
            match apply_sync_protected(
                src,
                dst,
                Some(&backup_dir),
                protection,
                Some(&mut owned_paths),
            ) {
                Ok(n) => applied += n,
                Err(e) => {
                    let message = format!("sync failed for {}: {e}", src.display());
                    eprintln!("  [FAIL] {message}");
                    failures.push(message);
                }
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
            match apply_sync_protected(
                src,
                dst,
                Some(&backup_dir),
                protection,
                Some(&mut owned_paths),
            ) {
                Ok(n) => applied += n,
                Err(e) => {
                    let message = format!("sync failed for {}: {e}", src.display());
                    eprintln!("  [FAIL] {message}");
                    failures.push(message);
                }
            }
        }
    }

    // Mirror commands to runtime slash-command dir
    let runtime_cmd_dir = hex_dir.join(".claude/commands");
    if src_dirs.commands.exists() {
        if let Err(e) = fs::create_dir_all(&runtime_cmd_dir) {
            let message = format!("could not create runtime command directory: {e}");
            eprintln!("  [FAIL] {message}");
            failures.push(message);
        } else if let Err(e) = apply_sync_protected(
            &src_dirs.commands,
            &runtime_cmd_dir,
            Some(&backup_dir),
            protection,
            Some(&mut owned_paths),
        ) {
            let message = format!("command mirror failed: {e}");
            eprintln!("  [FAIL] {message}");
            failures.push(message);
        }
    }

    // Deletion pass
    println!("  → Running deletion pass...");
    let mut deleted = 0;
    for (src, dst) in sync_pairs {
        if src.exists() {
            match deletion_pass_protected(dst, src, &backup_dir, protection, Some(&mut owned_paths))
            {
                Ok(n) => deleted += n,
                Err(e) => {
                    let message = format!("deletion pass failed for {}: {e}", dst.display());
                    eprintln!("  [FAIL] {message}");
                    failures.push(message);
                }
            }
        }
    }
    if src_dirs.commands.exists() {
        match deletion_pass_protected(
            &runtime_cmd_dir,
            &src_dirs.commands,
            &backup_dir,
            protection,
            Some(&mut owned_paths),
        ) {
            Ok(n) => deleted += n,
            Err(e) => {
                let message = format!("command deletion pass failed: {e}");
                eprintln!("  [FAIL] {message}");
                failures.push(message);
            }
        }
    }

    if deleted > 0 {
        println!("  [OK] Deletion pass: removed {deleted} stale file(s)");
    } else {
        println!("  → Deletion pass: nothing to prune");
    }

    if let Err(e) = make_scripts_executable(&owned_paths) {
        let message = format!("could not set script permissions: {e}");
        eprintln!("  [FAIL] {message}");
        failures.push(message);
    }

    for file in managed_files(&src_dirs, &hex_dir) {
        match apply_managed_file_protected(
            &file,
            Some(&backup_dir),
            protection,
            Some(&mut owned_paths),
        ) {
            Ok(n) => applied += n,
            Err(e) => {
                let message = format!("sync failed for {}: {e}", file.label);
                eprintln!("  [FAIL] {message}");
                failures.push(message);
            }
        }
    }

    // Update version.txt for v2 layout
    if let Some(src_ver_file) = &src_dirs.version_txt {
        if src_ver_file.exists() {
            let allowed = protection.map_or(Ok(()), |(workspace, snapshot)| {
                protect_sync_path(
                    workspace,
                    &hex_dot_dir.join("version.txt"),
                    Some(src_ver_file),
                    snapshot,
                    Some(&owned_paths),
                )
            });
            match allowed {
                Err(e) => {
                    let message = format!("version.txt operator edit conflict: {e}");
                    eprintln!("  [FAIL] {message}");
                    failures.push(message);
                }
                Ok(()) => {
                    match fs::read(src_ver_file).and_then(|bytes| {
                        fs::write(hex_dot_dir.join("version.txt"), &bytes)?;
                        Ok(bytes)
                    }) {
                        Ok(bytes) => {
                            owned_paths.insert(hex_dot_dir.join("version.txt"), Some(bytes));
                        }
                        Err(e) => {
                            let message = format!("version.txt read/write failed: {e}");
                            eprintln!("  [FAIL] {message}");
                            failures.push(message);
                        }
                    }
                }
            }
        }
    }

    println!("  [OK] Applied {applied} file(s)");

    if !failures.is_empty() {
        eprintln!(
            "  Upgrade INCOMPLETE: {} required step(s) failed; successful files and backups remain on disk.",
            failures.len()
        );
        return 1;
    }

    let _ = fs::remove_file(hex_dot_dir.join(".update-available"));

    let companion_result = crate::codeintel_upgrade::apply(&hex_dir, &source_dir, &companion_plan);

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

    if let Err(error) = companion_result {
        eprintln!("  Upgrade INCOMPLETE: code-intel update failed: {error}");
        eprintln!("  Completed Hex and companion updates remain installed. Retry resumes required companion work.");
        return 1;
    }

    // Record provenance only after every required file operation and the
    // binary deployment have been classified as successful.
    if let Err(e) = record_upgrade_sha(
        &config_file,
        &source_dir,
        &repo_url,
        protection,
        Some(&mut owned_paths),
    ) {
        eprintln!("  [FAIL] Upgrade source SHA could not be recorded: {e}");
        return 1;
    }

    // Commit the synced tracked files so the instance repo reflects the deployed
    // version. Closes the deployed-but-orphaned blind spot: without this, a live
    // deploy sits uncommitted and git — plus `hex upgrade`'s own change detection
    // — reads it as "nothing changed" indefinitely. Only ever commit into the
    // instance's OWN repo; a failure here is LOUD (S6) and exits nonzero so the
    // operator knows the deploy is live but unrecorded in git.
    if is_own_git_toplevel(&hex_dir) {
        // Wire the committed leak-guard hook so clones and instances get it
        // without a manual `git config` step. Idempotent; loud on failure but
        // never aborts the upgrade (a missing hooks wiring must not block a
        // version sync).
        configure_hooks_path(&hex_dir);
        // Read the version to name the commit. A missing/empty/unreadable
        // version.txt must NOT silently degrade (Standing Order S6: no quiet
        // failures) — the deploy still gets committed so the tree is consistent,
        // but the operator is warned loudly that the commit names "unknown".
        let version_file = hex_dot_dir.join("version.txt");
        let synced_version = match fs::read_to_string(&version_file) {
            Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
            Ok(_) => {
                eprintln!(
                    "  [WARN] {} is empty; the upgrade commit will name the version \"unknown\".",
                    version_file.display()
                );
                "unknown".to_string()
            }
            Err(e) => {
                eprintln!(
                    "  [WARN] Could not read {} ({e}); the upgrade commit will name the version \"unknown\".",
                    version_file.display()
                );
                "unknown".to_string()
            }
        };
        let snapshot = git_snapshot
            .as_ref()
            .expect("own git repo must have a pre-upgrade snapshot");
        match commit_synced_files_since(&hex_dir, &synced_version, snapshot, Some(&owned_paths)) {
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
                    "  Fix: inspect the scoped changed paths, then stage only those paths and commit them; do not use `git add -u -- .hex` while operator edits are present."
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
    use crate::test_child::{self, Fault as PrivateChildFault};
    use std::fs;
    use std::io::Write;
    #[cfg(target_os = "macos")]
    use std::os::unix::fs::MetadataExt;
    use std::thread;
    use std::time::Duration;

    fn write_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn private_upgrade_child(
        test_name: &str,
        home: &Path,
    ) -> std::result::Result<test_child::ChildOutput, String> {
        private_upgrade_child_with_fault(test_name, home, PrivateChildFault::None)
    }

    fn private_upgrade_child_with_fault(
        test_name: &str,
        home: &Path,
        fault: PrivateChildFault,
    ) -> std::result::Result<test_child::ChildOutput, String> {
        test_child::run_exact(test_name, home, home, fault)
    }

    fn private_upgrade_test_success(
        test_name: &str,
        output: test_child::ChildOutput,
    ) -> std::result::Result<(), String> {
        test_child::require_one_pass(test_name, output)
    }

    /// Legacy upgrade fixtures run only in an exact child with a disposable
    /// HOME and HEX_DIR. The parent never inherits fixture environment state.
    fn in_private_upgrade_test(test_name: &str) -> bool {
        if test_child::in_child(test_name) {
            test_child::stage("upgrade:child-entry").expect("test-child stage must flush");
            return false;
        }
        let home = tempfile::tempdir().expect("private test HOME");
        let output = private_upgrade_child(test_name, home.path())
            .unwrap_or_else(|error| panic!("private upgrade test {test_name} failed: {error}"));
        private_upgrade_test_success(test_name, output).unwrap_or_else(|error| panic!("{error}"));
        true
    }

    #[test]
    fn private_upgrade_runner_rejects_missing_selector() {
        let home = tempfile::tempdir().unwrap();
        let output =
            private_upgrade_child("upgrade::tests::missing_private_fixture", home.path()).unwrap();
        assert!(output.status.success());
        let error = private_upgrade_test_success("upgrade::tests::missing_private_fixture", output)
            .expect_err("a zero-test selector must be rejected");
        assert!(error.contains("did not prove one exact passing test"));
        assert!(error.contains("running 0 tests"));
    }

    #[test]
    fn private_upgrade_runner_retains_over_cap_output_tails() {
        const NAME: &str = "upgrade::tests::private_upgrade_runner_retains_over_cap_output_tails";
        const STDOUT_HEAD: &str = "OVERCAP_STDOUT_HEAD_ONLY";
        const STDOUT_TAIL: &str = "OVERCAP_STDOUT_TAIL_SENTINEL";
        const STDERR_HEAD: &str = "OVERCAP_STDERR_HEAD_ONLY";
        const STDERR_TAIL: &str = "OVERCAP_STDERR_TAIL_SENTINEL";
        if test_child::in_child(NAME) {
            test_child::stage("upgrade:over-cap-control-entered")
                .expect("over-cap control stage must flush");
            print!(
                "{STDOUT_HEAD}{}{}",
                "x".repeat(test_child::OUTPUT_TAIL_BYTES + 128),
                STDOUT_TAIL
            );
            eprint!(
                "{STDERR_HEAD}{}{}",
                "y".repeat(test_child::OUTPUT_TAIL_BYTES + 128),
                STDERR_TAIL
            );
            std::io::stdout()
                .flush()
                .expect("over-cap stdout must flush");
            std::io::stderr()
                .flush()
                .expect("over-cap stderr must flush");
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let output =
            private_upgrade_child(NAME, home.path()).expect("over-cap owned child must complete");
        assert!(
            output.stdout.total_bytes > test_child::OUTPUT_TAIL_BYTES && output.stdout.truncated,
            "stdout must retain total bytes and truncation after its cap"
        );
        assert!(
            output.stderr.total_bytes > test_child::OUTPUT_TAIL_BYTES && output.stderr.truncated,
            "stderr must retain total bytes and truncation after its cap"
        );
        let stdout_tail = String::from_utf8_lossy(&output.stdout.tail);
        let stderr_tail = String::from_utf8_lossy(&output.stderr.tail);
        assert!(stdout_tail.contains(STDOUT_TAIL) && !stdout_tail.contains(STDOUT_HEAD));
        assert!(stderr_tail.contains(STDERR_TAIL) && !stderr_tail.contains(STDERR_HEAD));
        assert!(
            output.status.success(),
            "over-cap child must exit successfully"
        );
        assert!(
            stdout_tail.contains(&format!("test {NAME} ... ok"))
                && stdout_tail.contains("test result: ok. 1 passed"),
            "the retained stdout tail must prove the exact child passed even though its leading count was trimmed"
        );
    }

    #[test]
    fn private_upgrade_runner_reaps_timed_out_owned_group() {
        const NAME: &str = "upgrade::tests::private_upgrade_runner_reaps_timed_out_owned_group";
        if test_child::in_child(NAME) {
            test_child::stage("upgrade:timeout-control-entered")
                .expect("timeout control stage must flush");
            let previous = unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
            assert_ne!(
                previous,
                libc::SIG_ERR,
                "timeout control must install its child-only SIGTERM disposition"
            );
            println!("TIMEOUT_SIGTERM_IGNORED");
            println!("TIMEOUT_STDOUT_SENTINEL");
            eprintln!("TIMEOUT_STDERR_SENTINEL");
            std::io::stdout()
                .flush()
                .expect("timeout stdout must flush");
            std::io::stderr()
                .flush()
                .expect("timeout stderr must flush");
            thread::sleep(Duration::from_secs(60));
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let error = private_upgrade_child(NAME, home.path())
            .expect_err("the private child must hit its absolute deadline");
        println!("TIMEOUT_CONTROL_EXPECTED_ERROR:{error}");
        assert!(error.contains("exceeded 15 seconds"));
        assert!(error.contains("HEX_TEST_CHILD_STAGE:upgrade:timeout-control-entered"));
        assert!(error.contains("TIMEOUT_SIGTERM_IGNORED"));
        assert!(error.contains("TIMEOUT_STDOUT_SENTINEL"));
        assert!(error.contains("TIMEOUT_STDERR_SENTINEL"));
        assert!(error.contains("direct child wait: success"));
        assert!(error.contains("cleanup elapsed_ms="));
        assert!(error.contains("grace wait: expired/still-running before group KILL"));
        assert!(error.contains("root child exit does not prove descendant"));
    }

    #[test]
    fn private_upgrade_runner_cleans_owned_child_on_pipe_setup_failure() {
        const NAME: &str =
            "upgrade::tests::private_upgrade_runner_cleans_owned_child_on_pipe_setup_failure";
        if test_child::in_child(NAME) {
            test_child::stage("upgrade:pipe-setup-control-entered")
                .expect("pipe setup control stage must flush");
            println!("PIPE_SETUP_STDOUT_SENTINEL");
            eprintln!("PIPE_SETUP_STDERR_SENTINEL");
            std::io::stdout()
                .flush()
                .expect("pipe setup stdout must flush");
            std::io::stderr()
                .flush()
                .expect("pipe setup stderr must flush");
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let stdout_error = private_upgrade_child_with_fault(
            NAME,
            home.path(),
            PrivateChildFault::BeforeStdoutReader,
        )
        .expect_err("the controlled stdout setup failure must clean the child");
        assert!(stdout_error.contains("controlled stdout pipe setup failure"));
        assert!(stdout_error.contains("owned root child reaped"));
        assert!(stdout_error.contains("direct child wait: success"));
        assert!(stdout_error.contains("cleanup elapsed_ms="));

        let error = private_upgrade_child_with_fault(
            NAME,
            home.path(),
            PrivateChildFault::BeforeStderrReader,
        )
        .expect_err("the controlled pipe setup failure must fail after spawning the child");
        assert!(error.contains("controlled stderr pipe setup failure"));
        assert!(error.contains("owned root child reaped"));
        assert!(error.contains("stdout reader: bytes="));
        assert!(!error.contains("stdout reader was not started"));
        assert!(error.contains("stderr reader was not started"));
        assert!(error.contains("cleanup elapsed_ms="));
    }

    #[test]
    fn private_upgrade_runner_joins_both_readers_before_reader_failure() {
        const NAME: &str =
            "upgrade::tests::private_upgrade_runner_joins_both_readers_before_reader_failure";
        if test_child::in_child(NAME) {
            test_child::stage("upgrade:reader-join-control-entered")
                .expect("reader join control stage must flush");
            println!("READER_STDOUT_SENTINEL");
            eprintln!("READER_STDERR_SENTINEL");
            std::io::stdout().flush().expect("reader stdout must flush");
            std::io::stderr().flush().expect("reader stderr must flush");
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let error = private_upgrade_child_with_fault(
            NAME,
            home.path(),
            PrivateChildFault::BothReadersFailAfterRead,
        )
        .expect_err("the controlled reader failures must be reported");
        assert!(error.contains("stdout reader failed: controlled stdout reader failure"));
        assert!(error.contains("stderr reader failed: controlled stderr reader failure"));
        assert!(error.contains("HEX_TEST_CHILD_STAGE:upgrade:reader-join-control-entered"));
        assert!(error.contains("READER_STDOUT_SENTINEL"));
        assert!(error.contains("READER_STDERR_SENTINEL"));
        assert!(error.contains("stdout reader: bytes="));
        assert!(error.contains("stderr reader: bytes="));
    }

    #[test]
    fn private_upgrade_runner_admission_is_bounded_and_releases_after_failure() {
        const NAME: &str =
            "upgrade::tests::private_upgrade_runner_admission_is_bounded_and_releases_after_failure";
        if test_child::in_child(NAME) {
            test_child::stage("upgrade:admission-control-entered")
                .expect("admission control stage must flush");
            println!("ADMISSION_STDOUT_SENTINEL");
            return;
        }

        let held = test_child::acquire_default_admission()
            .expect("the initial test-only admission acquisition must succeed");
        let admission_error = test_child::acquire_admission(Duration::from_secs(1))
            .expect_err("a held test-only admission guard must fail within its own deadline");
        assert!(admission_error.contains("admission exceeded 1 seconds before child spawn"));
        drop(held);

        let home = tempfile::tempdir().unwrap();
        let cleanup_error = private_upgrade_child_with_fault(
            NAME,
            home.path(),
            PrivateChildFault::BeforeStderrReader,
        )
        .expect_err("the real setup failure must release admission after cleanup");
        assert!(cleanup_error.contains("owned root child reaped"));
        assert!(cleanup_error.contains("cleanup elapsed_ms="));

        let output = private_upgrade_child(NAME, home.path())
            .expect("admission must release after the cleanup error");
        private_upgrade_test_success(NAME, output)
            .expect("released admission must run the real child");
    }

    #[test]
    fn preflight_counts_deletion_only_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source/scripts");
        let destination = tmp.path().join("instance/.hex/scripts");
        fs::create_dir_all(&source).unwrap();
        write_file(&destination.join("stale.sh"), "stale");

        let (changed, new_count, unchanged, log) =
            detect_managed_changes(&source, &destination, "scripts", true).unwrap();
        assert_eq!((changed, new_count, unchanged), (1, 0, 0));
        assert!(log.iter().any(|line| line.contains("- scripts/stale.sh")));
    }

    #[test]
    fn preflight_counts_command_mirror_only_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source/system/commands");
        let mirror = tmp.path().join("instance/.claude/commands");
        write_file(&source.join("hex.md"), "current");
        write_file(&mirror.join("obsolete.md"), "obsolete");

        let (changed, new_count, unchanged, log) =
            detect_managed_changes(&source, &mirror, "command mirror", true).unwrap();
        assert_eq!((changed, new_count, unchanged), (1, 1, 0));
        assert!(log
            .iter()
            .any(|line| line.contains("- command mirror/obsolete.md")));
        assert!(log
            .iter()
            .any(|line| line.contains("+ command mirror/hex.md")));
    }

    #[test]
    fn preflight_ignores_unmanaged_build_and_dependency_output() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source/scripts");
        let destination = tmp.path().join("instance/.hex/scripts");
        write_file(&source.join("target/generated.sh"), "build output");
        write_file(
            &source.join("node_modules/pkg/generated.sh"),
            "dependency output",
        );
        write_file(&destination.join("target/old.sh"), "build output");
        write_file(
            &destination.join("node_modules/pkg/old.sh"),
            "dependency output",
        );

        let (changed, new_count, unchanged, log) =
            detect_managed_changes(&source, &destination, "scripts", true).unwrap();
        assert_eq!((changed, new_count, unchanged), (0, 0, 0));
        assert!(log.is_empty());
    }

    fn binary_preflight_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let instance = tmp.path().join("instance");
        write_file(
            &source.join("system/harness/Cargo.toml"),
            "[package]\nversion = \"1.0.0\"\n",
        );
        write_file(
            &instance.join("VERSIONS"),
            "HEX_FOUNDATION_VERSION=v1.0.0\n",
        );
        write_file(
            &instance.join(".hex/bin/hex"),
            "#!/bin/sh\nprintf 'hex 1.0.0\\n'\n",
        );
        fs::set_permissions(
            instance.join(".hex/bin/hex"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        init_test_repo(&source);
        seed_commit(&source, "preflight fixture");
        let sha = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&source)
            .output()
            .unwrap();
        assert!(sha.status.success());
        fs::write(instance.join(".hex/bin/hex.sha"), sha.stdout).unwrap();
        (tmp, source, instance)
    }

    #[test]
    fn preflight_binary_metadata_preserves_true_noop() {
        if in_private_upgrade_test("upgrade::tests::preflight_binary_metadata_preserves_true_noop")
        {
            return;
        }
        let _env = crate::test_env::isolate_hex_dir();
        test_child::stage("upgrade:preflight-noop-body").expect("test-child stage must flush");
        let (_tmp, source, instance) = binary_preflight_fixture();
        assert!(!binary_is_stale(&instance, &source).unwrap());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn synthetic_managed_policy_evidence_fails_loudly_in_private_home() {
        if in_private_upgrade_test(
            "upgrade::tests::synthetic_managed_policy_evidence_fails_loudly_in_private_home",
        ) {
            return;
        }
        let (_tmp, source, instance) = binary_preflight_fixture();
        let home = PathBuf::from(std::env::var_os("HOME").expect("private HOME"));
        write_file(
            &home.join("Library/Application Support/Hex/build-signing/policy.json"),
            "{}",
        );
        let error = binary_is_stale(&instance, &source)
            .expect_err("synthetic managed evidence must preflight");
        assert_eq!(
            error.kind(),
            io::ErrorKind::NotFound,
            "synthetic evidence must fail loudly through source preflight: {error}"
        );
    }

    #[test]
    fn preflight_binary_metadata_rejects_failed_version_command() {
        let (_tmp, source, instance) = binary_preflight_fixture();
        write_file(
            &instance.join(".hex/bin/hex"),
            "#!/bin/sh\nprintf 'hex 1.0.0\\n'\nexit 9\n",
        );
        assert!(
            binary_is_stale(&instance, &source).is_err(),
            "failed executable must not prove current version"
        );
    }

    #[test]
    fn preflight_binary_metadata_rejects_unreadable_versions() {
        let (_tmp, source, instance) = binary_preflight_fixture();
        fs::remove_file(instance.join("VERSIONS")).unwrap();
        fs::create_dir(instance.join("VERSIONS")).unwrap();
        assert!(
            binary_is_stale(&instance, &source).is_err(),
            "required unreadable metadata must not pass no-op"
        );
    }

    #[test]
    fn preflight_binary_metadata_rejects_malformed_manifest_with_version_line() {
        let (_tmp, source, instance) = binary_preflight_fixture();
        write_file(
            &source.join("system/harness/Cargo.toml"),
            "[package]\nversion = \"1.0.0\"\nthis is not toml\n",
        );
        assert!(
            binary_is_stale(&instance, &source).is_err(),
            "finding a version line is not parsing a manifest"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preflight_reconciles_stale_versions_pin_when_other_inputs_match() {
        if in_private_upgrade_test(
            "upgrade::tests::preflight_reconciles_stale_versions_pin_when_other_inputs_match",
        ) {
            return;
        }
        test_child::stage("upgrade:stale-pin-body").expect("test-child stage must flush");
        let _env = crate::test_env::isolate_hex_dir();
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let instance = tmp.path().join("instance");
        let source_files = [
            "system/scripts/a.sh",
            "system/skills/s/SKILL.md",
            "system/commands/c.md",
            "system/hooks/h.sh",
            "system/iii/i.yml",
            "system/templates/t.txt",
            "system/version.txt",
            "templates/AGENTS.md",
        ];
        for relative in source_files {
            write_file(&source.join(relative), relative);
            let destination = match relative {
                "system/scripts/a.sh" => instance.join(".hex/scripts/a.sh"),
                "system/skills/s/SKILL.md" => instance.join(".hex/skills/s/SKILL.md"),
                "system/commands/c.md" => instance.join(".hex/commands/c.md"),
                "system/hooks/h.sh" => instance.join(".hex/hooks/h.sh"),
                "system/iii/i.yml" => instance.join(".hex/iii/i.yml"),
                "system/templates/t.txt" => instance.join(".hex/templates/t.txt"),
                "system/version.txt" => instance.join(".hex/version.txt"),
                "templates/AGENTS.md" => continue,
                _ => unreachable!(),
            };
            write_file(&destination, relative);
        }
        write_file(
            &instance.join(".claude/commands/c.md"),
            "system/commands/c.md",
        );
        write_file(
            &source.join("system/harness/Cargo.toml"),
            "[package]\nname = \"hex-harness\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );
        test_child::stage("upgrade:stale-pin-fixture-ready")
            .expect("stale-pin fixture-ready stage must flush");
        init_test_repo(&source);
        seed_commit(&source, "stale versions preflight fixture");
        test_child::stage("upgrade:stale-pin-git-ready")
            .expect("stale-pin git-ready stage must flush");

        write_file(&instance.join("AGENTS.md"), "# test instance\n");
        write_file(
            &instance.join("VERSIONS"),
            "# keep this comment\nHEX_FOUNDATION_VERSION=v0.0.0\nOTHER_PIN=v9\nHEX_FOUNDATION_VERSION=v0.0.0\n",
        );
        let bin = instance.join(".hex/bin/hex");
        write_file(&bin, "#!/bin/sh\nprintf 'hex 1.0.0\\n'\n");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let sha = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&source)
            .output()
            .unwrap();
        assert!(sha.status.success());
        fs::write(instance.join(".hex/bin/hex.sha"), sha.stdout).unwrap();

        std::env::set_var("HEX_DIR", &instance);
        let args = vec!["--local".to_string(), source.to_string_lossy().into_owned()];
        test_child::stage("upgrade:stale-pin-before-run")
            .expect("stale-pin before-run stage must flush");
        let exit = run(&args);
        test_child::stage("upgrade:stale-pin-after-run")
            .expect("stale-pin after-run stage must flush");
        assert_eq!(exit, 0);
        let versions = fs::read_to_string(instance.join("VERSIONS")).unwrap();
        assert_eq!(versions.matches("HEX_FOUNDATION_VERSION=").count(), 1);
        assert!(versions.contains("HEX_FOUNDATION_VERSION=v1.0.0"));
        assert!(versions.contains("# keep this comment"));
        assert!(versions.contains("OTHER_PIN=v9"));
        test_child::stage("upgrade:stale-pin-assertions-complete")
            .expect("stale-pin assertions-complete stage must flush");
    }

    #[cfg(unix)]
    #[test]
    fn run_build_failure_leaves_live_managed_files_unchanged() {
        let _env = crate::test_env::isolate_hex_dir();
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let instance = tmp.path().join("instance");
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
        seed_commit(&source, "failed candidate fixture");

        write_file(&instance.join("AGENTS.md"), "# instance\n");
        write_file(
            &instance.join("VERSIONS"),
            "HEX_FOUNDATION_VERSION=v1.0.0\n",
        );
        let bin = instance.join(".hex/bin/hex");
        write_file(&bin, "#!/bin/sh\nprintf 'hex 1.0.0\\n'");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        write_file(&instance.join(".hex/scripts/managed.sh"), "old script\n");
        write_file(
            &instance.join(".hex/managed_cargo_bridge.rs"),
            "old bridge\n",
        );

        std::env::set_var("HEX_DIR", &instance);
        let exit = run(&["--local".to_string(), source.to_string_lossy().into_owned()]);
        assert_eq!(exit, 1);
        assert_eq!(
            fs::read(instance.join(".hex/scripts/managed.sh")).unwrap(),
            b"old script\n"
        );
        assert_eq!(
            fs::read(instance.join(".hex/managed_cargo_bridge.rs")).unwrap(),
            b"old bridge\n"
        );
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
            backup_dir.join(".hex/hooks/scripts/hook.sh").exists(),
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
            bak.join("dst/stale.sh").exists(),
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

    /// A removable `.upgrade-cache.corrupt-*` residue directory is swept away
    /// and counted as removed. The live `.upgrade-cache` dir itself is left
    /// untouched.
    #[test]
    fn sweep_removes_removable_residue_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let hex_dir = tmp.path();
        let dot_hex = hex_dir.join(".hex");
        let residue = dot_hex.join(".upgrade-cache.corrupt-0");
        write_file(&residue.join("CLAUDE.md"), "stale");
        let live_cache = dot_hex.join(".upgrade-cache");
        write_file(&live_cache.join(".git/config"), "[core]\n");

        let (removed, failed) = sweep_corrupt_cache_residue(hex_dir);

        assert_eq!(removed, 1);
        assert!(failed.is_empty());
        assert!(!residue.exists(), "residue dir must be gone");
        assert!(live_cache.exists(), "live cache dir must be untouched");
    }

    /// A residue dir that cannot be removed is collected into `failed`
    /// without panicking. Reproduced by stripping write permission on the
    /// residue's parent (`.hex`), which blocks unlinking the directory entry
    /// even though its contents are still readable — the same shape as an
    /// OS-protected residue dir on the live instance.
    #[test]
    fn sweep_collects_unremovable_residue_without_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        let hex_dir = tmp.path();
        let dot_hex = hex_dir.join(".hex");
        let residue = dot_hex.join(".upgrade-cache.corrupt-0");
        write_file(&residue.join("CLAUDE.md"), "stale");

        let mut perms = fs::metadata(&dot_hex).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&dot_hex, perms).unwrap();

        // Confirm the restriction actually bites in this test environment.
        // Running as root (a CI container, a release runner) ignores
        // directory write permission entirely, which would make a hard
        // `removed == 0` assertion below false-fail rather than test
        // anything. Mirrors the `test_cache_is_healthy` pattern of gating an
        // environment-dependent assertion on a live probe.
        let probe = dot_hex.join("root-write-probe");
        let enforced = fs::write(&probe, b"probe").is_err();
        let _ = fs::remove_file(&probe);

        let (removed, failed) = sweep_corrupt_cache_residue(hex_dir);

        // Restore write perms so the tempdir can clean itself up on drop.
        let mut perms = fs::metadata(&dot_hex).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dot_hex, perms).unwrap();

        // The one guarantee this test makes regardless of environment: a
        // removal failure is collected, never panics.
        if enforced {
            assert_eq!(removed, 0);
            assert_eq!(failed, vec![residue]);
        } else {
            assert_eq!(
                removed, 1,
                "permission restriction unenforced in this env (likely root); \
                 residue should have been removed instead"
            );
            assert!(failed.is_empty());
        }
    }

    /// The binary step must report health honestly: an up-to-date skip is
    /// `true`; an unparseable Cargo.toml (can't even determine the target
    /// version — the source sync is broken) is `false`, which run() turns
    /// into a failed upgrade instead of "Upgrade complete." (OBS-017).
    #[cfg(unix)]
    #[test]
    fn sync_versions_file_returns_binary_step_health() {
        if in_private_upgrade_test("upgrade::tests::sync_versions_file_returns_binary_step_health")
        {
            return;
        }
        let _env = crate::test_env::isolate_hex_dir();
        test_child::stage("upgrade:sync-health-body").expect("test-child stage must flush");
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
        init_test_repo(&source_dir);
        seed_commit(&source_dir, "metadata health fixture");
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

    #[cfg(unix)]
    #[test]
    fn sync_versions_file_rejects_unreadable_installed_sha() {
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
        init_test_repo(&source_dir);
        seed_commit(&source_dir, "unreadable SHA fixture");
        let bin_dir = hex_dir.join(".hex/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let mock_bin = bin_dir.join("hex");
        fs::write(&mock_bin, "#!/bin/sh\necho hex 1.0.0\n").unwrap();
        fs::set_permissions(&mock_bin, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(hex_dir.join(".hex/bin/hex.sha")).unwrap();

        assert!(
            sync_versions_file(&hex_dir, &source_dir, &backup_dir).is_err(),
            "an unreadable present SHA must fail, not become an unverifiable skip"
        );
    }

    #[test]
    fn sync_versions_file_rejects_source_without_git_head() {
        let (tmp, source, instance) = binary_preflight_fixture();
        fs::write(source.join(".git/HEAD"), "ref: refs/heads/missing\n").unwrap();
        assert!(
            sync_versions_file(&instance, &source, &tmp.path().join("backup")).is_err(),
            "matching installed version cannot mask a broken source repository"
        );
    }

    #[test]
    fn sync_versions_file_rejects_invalid_utf8_installed_sha() {
        let (tmp, source, instance) = binary_preflight_fixture();
        fs::write(instance.join(".hex/bin/hex.sha"), [0xff]).unwrap();
        assert!(
            sync_versions_file(&instance, &source, &tmp.path().join("backup")).is_err(),
            "invalid installed SHA must not be treated as an absent optional file"
        );
    }

    /// The deploy-black-hole path itself (OBS-017): a rebuild is NEEDED
    /// (version mismatch) but the staged source build fails. The instance
    /// must remain untouched until that candidate build succeeds.
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
        init_test_repo(&source_dir);
        seed_commit(&source_dir, "rebuild failure fixture");
        // Installed binary reports an OLDER version → rebuild required.
        let bin_dir = hex_dir.join(".hex/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let mock_bin = bin_dir.join("hex");
        fs::write(&mock_bin, "#!/bin/sh\necho hex 1.0.0\n").unwrap();
        fs::set_permissions(&mock_bin, fs::Permissions::from_mode(0o755)).unwrap();
        write_file(
            &source_dir.join("system/managed_cargo_bridge.rs"),
            "new bridge source",
        );
        write_file(
            &hex_dir.join(".hex/managed_cargo_bridge.rs"),
            "installed bridge remains unchanged",
        );

        assert!(
            sync_versions_file(&hex_dir, &source_dir, &backup_dir).is_err(),
            "a broken rebuild path must fail the binary step (deploy black hole)"
        );
        assert_eq!(
            fs::read(hex_dir.join("VERSIONS")).unwrap(),
            b"HEX_FOUNDATION_VERSION=v0.1.0\n",
            "a failed staged build must not update managed metadata"
        );
        assert_eq!(
            fs::read(hex_dir.join(".hex/managed_cargo_bridge.rs")).unwrap(),
            b"installed bridge remains unchanged",
            "a failed staged build must not update managed bridge source"
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
        if in_private_upgrade_test(
            "upgrade::tests::sync_versions_file_preserves_boi_version_and_comments",
        ) {
            return;
        }
        let _env = crate::test_env::isolate_hex_dir();
        test_child::stage("upgrade:sync-preserve-body").expect("test-child stage must flush");
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
        init_test_repo(&source_dir);
        seed_commit(&source_dir, "versions preservation fixture");

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
        let msg =
            binary_step_failure_message(&BinaryStepFailure::RestartFailed(restart_err.clone()));

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
        if in_private_upgrade_test(
            "upgrade::tests::sync_versions_file_return_type_carries_the_failure_kind",
        ) {
            return;
        }
        let _env = crate::test_env::isolate_hex_dir();
        test_child::stage("upgrade:sync-kind-body").expect("test-child stage must flush");
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
        init_test_repo(&source_dir);
        seed_commit(&source_dir, "binary health fixture");
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

    #[test]
    fn commit_synced_files_preserves_preexisting_dirty_hex_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".hex/operator-staged.txt"), "base staged\n");
        write_file(&ws.join(".hex/operator-unstaged.txt"), "base unstaged\n");
        write_file(&ws.join(".hex/upgrade-owned.txt"), "base upgrade\n");
        seed_commit(ws, "seed");

        write_file(&ws.join(".hex/operator-staged.txt"), "operator staged\n");
        Command::new("git")
            .args(["add", ".hex/operator-staged.txt"])
            .current_dir(ws)
            .status()
            .unwrap();
        write_file(
            &ws.join(".hex/operator-unstaged.txt"),
            "operator unstaged\n",
        );
        let snapshot = upgrade_git_snapshot(ws).unwrap();

        write_file(&ws.join(".hex/upgrade-owned.txt"), "upgrade result\n");
        let made = commit_synced_files_since(ws, "9.9.9-test", &snapshot, None).unwrap();
        assert!(made);
        assert!(!path_is_dirty(ws, ".hex/upgrade-owned.txt"));
        assert!(path_is_dirty(ws, ".hex/operator-staged.txt"));
        assert!(path_is_dirty(ws, ".hex/operator-unstaged.txt"));
        let staged = Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(ws)
            .output()
            .unwrap();
        let staged_files = String::from_utf8_lossy(&staged.stdout);
        assert!(staged_files.contains(".hex/operator-staged.txt"));

        let committed = Command::new("git")
            .args(["show", "--format=", "--name-only", "HEAD"])
            .current_dir(ws)
            .output()
            .unwrap();
        let files = String::from_utf8_lossy(&committed.stdout);
        assert!(files.contains(".hex/upgrade-owned.txt"));
        assert!(!files.contains("operator-staged.txt"));
        assert!(!files.contains("operator-unstaged.txt"));
    }

    #[test]
    fn protected_sync_does_not_overwrite_preexisting_dirty_hex_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".hex/scripts/foo.sh"), "base\n");
        seed_commit(ws, "seed");
        write_file(&ws.join(".hex/scripts/foo.sh"), "operator\n");
        let snapshot = upgrade_git_snapshot(ws).unwrap();
        let source = ws.join("source");
        write_file(&source.join("foo.sh"), "foundation\n");
        let result = apply_sync_protected(
            &source,
            &ws.join(".hex/scripts"),
            None,
            Some((ws, &snapshot)),
            None,
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(ws.join(".hex/scripts/foo.sh")).unwrap(),
            "operator\n"
        );
    }

    #[test]
    fn protected_sync_rejects_clean_file_edited_before_first_write() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".hex/scripts/foo.sh"), "base\n");
        seed_commit(ws, "seed");
        let snapshot = upgrade_git_snapshot(ws).unwrap();
        write_file(&ws.join(".hex/scripts/foo.sh"), "operator-before-write\n");
        let source = ws.join("source");
        write_file(&source.join("foo.sh"), "foundation\n");
        let result = apply_sync_protected(
            &source,
            &ws.join(".hex/scripts"),
            None,
            Some((ws, &snapshot)),
            None,
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(ws.join(".hex/scripts/foo.sh")).unwrap(),
            "operator-before-write\n"
        );
    }

    #[test]
    fn planned_snapshot_records_absence_and_rejects_unplanned_path() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join("seed"), "seed");
        seed_commit(ws, "seed");
        let planned = ws.join(".hex/scripts/new.sh");
        let snapshot = upgrade_git_snapshot_for(ws, std::slice::from_ref(&planned)).unwrap();
        assert_eq!(
            snapshot
                .baseline_content
                .get(Path::new(".hex/scripts/new.sh")),
            Some(&None)
        );
        assert!(protect_generated_path(ws, &planned, b"new", &snapshot, None).is_ok());
        assert!(
            protect_generated_path(ws, &ws.join(".hex/unplanned"), b"new", &snapshot, None)
                .is_err()
        );
    }

    #[test]
    fn repeated_owned_sync_accepts_own_write_but_rejects_intervening_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        let dst = ws.join(".hex/scripts/repeat.sh");
        write_file(&dst, "base");
        seed_commit(ws, "seed");
        let snapshot = upgrade_git_snapshot_for(ws, std::slice::from_ref(&dst)).unwrap();
        let src = ws.join("source");
        write_file(&src.join("repeat.sh"), "first");
        let mut owned = HashMap::new();
        apply_sync_protected(
            &src,
            &ws.join(".hex/scripts"),
            None,
            Some((ws, &snapshot)),
            Some(&mut owned),
        )
        .unwrap();
        write_file(&src.join("repeat.sh"), "second");
        apply_sync_protected(
            &src,
            &ws.join(".hex/scripts"),
            None,
            Some((ws, &snapshot)),
            Some(&mut owned),
        )
        .unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"second");
        write_file(&dst, "operator");
        write_file(&src.join("repeat.sh"), "third");
        assert!(apply_sync_protected(
            &src,
            &ws.join(".hex/scripts"),
            None,
            Some((ws, &snapshot)),
            Some(&mut owned)
        )
        .is_err());
        assert_eq!(fs::read(&dst).unwrap(), b"operator");
    }

    #[test]
    fn exact_inventory_includes_crate_roots_and_excludes_runtime_state() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("instance");
        let source = tmp.path().join("source");
        for file in [
            "harness/Cargo.toml",
            "harness/build.rs",
            "harness/src/lib.rs",
            "code-intel/Cargo.toml",
            "code-intel/src/lib.rs",
            "scripts/run.sh",
            "iii/config.toml",
            "templates/example",
            "commands/do.md",
            "managed_cargo_bridge.rs",
        ] {
            write_file(&source.join("system").join(file), "source");
        }
        for file in ["harness/target/debug/secret", "harness/node_modules/secret"] {
            write_file(&source.join("system").join(file), "ignored source output");
        }
        for file in [
            "harness/Cargo.toml",
            "harness/build.rs",
            "harness/src/old.rs",
            "harness/target/debug/cache",
            "code-intel/Cargo.toml",
            "iii/data/state.db",
            "iii/workers/node_modules/cache",
            "templates/operator-only",
            "memory.db",
            "credentials.env",
            "commands/old.md",
            "managed_cargo_bridge.rs",
        ] {
            write_file(&ws.join(".hex").join(file), "existing");
        }
        let sources = source_dirs_for_layout("v2", &source).unwrap();
        let plan = planned_upgrade_paths(&ws, &source, &sources).unwrap();
        for file in [
            "harness/Cargo.toml",
            "harness/build.rs",
            "harness/src/lib.rs",
            "harness/src/old.rs",
            "code-intel/Cargo.toml",
            "scripts/run.sh",
            "commands/old.md",
            "managed_cargo_bridge.rs",
        ] {
            assert!(plan.contains(&ws.join(".hex").join(file)), "missing {file}");
        }
        for file in [
            "harness/target/debug/cache",
            "harness/target/debug/secret",
            "harness/node_modules/secret",
            "iii/data/state.db",
            "iii/workers/node_modules/cache",
            "templates/operator-only",
            "memory.db",
            "credentials.env",
        ] {
            assert!(
                !plan.contains(&ws.join(".hex").join(file)),
                "unplanned runtime read: {file}"
            );
        }
        assert!(plan.contains(&ws.join(".claude/commands/do.md")));
        init_test_repo(&ws);
        seed_commit(&ws, "seed");
        let snapshot = upgrade_git_snapshot_for(&ws, &plan).unwrap();
        assert_eq!(snapshot.baseline_content.len(), plan.len());
        assert!(snapshot
            .baseline_content
            .contains_key(Path::new(".hex/harness/Cargo.toml")));
        assert!(snapshot
            .baseline_content
            .contains_key(Path::new(".hex/managed_cargo_bridge.rs")));
        // The real writer accepts both root files and new files from this exact inventory.
        let mut owned = HashMap::new();
        apply_sync_protected(
            &source.join("system/harness"),
            &ws.join(".hex/harness"),
            None,
            Some((&ws, &snapshot)),
            Some(&mut owned),
        )
        .unwrap();
        assert_eq!(
            fs::read(ws.join(".hex/harness/Cargo.toml")).unwrap(),
            b"source"
        );
        assert!(!ws.join(".hex/harness/target/debug/secret").exists());
    }

    #[test]
    fn commit_and_inventory_do_not_read_ignored_runtime_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".gitignore"), ".hex/memory.db\n.hex/iii/data/\n");
        let managed = ws.join(".hex/scripts/run.sh");
        write_file(&managed, "base");
        seed_commit(ws, "seed");
        let ignored = ws.join(".hex/memory.db");
        write_file(&ignored, "private runtime bytes");
        fs::set_permissions(&ignored, fs::Permissions::from_mode(0o000)).unwrap();
        let result = (|| -> Result<bool, String> {
            let snapshot = upgrade_git_snapshot_for(ws, std::slice::from_ref(&managed))?;
            assert_eq!(snapshot.baseline_content.len(), 1);
            write_file(&managed, "new");
            let owned = HashMap::from([(managed, Some(b"new".to_vec()))]);
            commit_synced_files_since(ws, "test", &snapshot, Some(&owned))
        })();
        fs::set_permissions(&ignored, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(result.unwrap());
        assert_eq!(fs::read(&ignored).unwrap(), b"private runtime bytes");
    }

    #[test]
    fn protected_backups_keep_same_relative_names_in_distinct_scopes() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".hex/scripts/foo.sh"), "script-old\n");
        write_file(&ws.join(".hex/hooks/foo.sh"), "hook-old\n");
        write_file(&ws.join(".hex/commands/foo.sh"), "command-old\n");
        write_file(&ws.join(".claude/commands/foo.sh"), "mirror-old\n");
        seed_commit(ws, "seed");
        let snapshot = upgrade_git_snapshot(ws).unwrap();
        let script_src = ws.join("script-source");
        let hook_src = ws.join("hook-source");
        write_file(&script_src.join("foo.sh"), "script-new\n");
        write_file(&hook_src.join("foo.sh"), "hook-new\n");
        let backup = ws.join(".hex/.upgrade-backup-test");
        apply_sync_protected(
            &script_src,
            &ws.join(".hex/scripts"),
            Some(&backup),
            Some((ws, &snapshot)),
            None,
        )
        .unwrap();
        apply_sync_protected(
            &hook_src,
            &ws.join(".hex/hooks"),
            Some(&backup),
            Some((ws, &snapshot)),
            None,
        )
        .unwrap();
        for destination in [".hex/commands", ".claude/commands"] {
            apply_sync_protected(
                &script_src,
                &ws.join(destination),
                Some(&backup),
                Some((ws, &snapshot)),
                None,
            )
            .unwrap();
        }
        assert_eq!(
            fs::read_to_string(backup.join(".hex/commands/foo.sh")).unwrap(),
            "command-old\n"
        );
        assert_eq!(
            fs::read_to_string(backup.join(".claude/commands/foo.sh")).unwrap(),
            "mirror-old\n"
        );
        assert_eq!(
            fs::read_to_string(backup.join(".hex/scripts/foo.sh")).unwrap(),
            "script-old\n"
        );
        assert_eq!(
            fs::read_to_string(backup.join(".hex/hooks/foo.sh")).unwrap(),
            "hook-old\n"
        );
    }

    #[test]
    fn commit_owned_paths_ignores_unrelated_new_dirty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".hex/owned.txt"), "base\n");
        seed_commit(ws, "seed");
        let snapshot = upgrade_git_snapshot(ws).unwrap();
        write_file(&ws.join(".hex/owned.txt"), "upgrade\n");
        write_file(&ws.join(".hex/operator-new.txt"), "operator\n");
        let mut owned = HashMap::new();
        owned.insert(
            ws.join(".hex/owned.txt"),
            Some(fs::read(ws.join(".hex/owned.txt")).unwrap()),
        );
        assert!(commit_synced_files_since(ws, "9.9.9-test", &snapshot, Some(&owned)).unwrap());
        assert!(path_is_dirty(ws, ".hex/operator-new.txt"));
        assert!(!path_is_dirty(ws, ".hex/owned.txt"));
    }

    #[test]
    fn commit_owned_path_rejects_edit_after_upgrade_write() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".hex/owned.txt"), "base\n");
        seed_commit(ws, "seed");
        let snapshot = upgrade_git_snapshot(ws).unwrap();
        write_file(&ws.join(".hex/owned.txt"), "upgrade\n");
        let mut owned = HashMap::new();
        owned.insert(
            ws.join(".hex/owned.txt"),
            Some(fs::read(ws.join(".hex/owned.txt")).unwrap()),
        );
        write_file(&ws.join(".hex/owned.txt"), "operator-after\n");
        let result = commit_synced_files_since(ws, "9.9.9-test", &snapshot, Some(&owned));
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(ws.join(".hex/owned.txt")).unwrap(),
            "operator-after\n"
        );
    }

    #[test]
    fn commit_owned_path_rejects_operator_index_change_even_if_worktree_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        init_test_repo(ws);
        write_file(&ws.join(".hex/owned.txt"), "base\n");
        seed_commit(ws, "seed");
        let snapshot = upgrade_git_snapshot(ws).unwrap();
        write_file(&ws.join(".hex/owned.txt"), "upgrade\n");
        let mut owned = HashMap::new();
        owned.insert(
            ws.join(".hex/owned.txt"),
            Some(fs::read(ws.join(".hex/owned.txt")).unwrap()),
        );
        write_file(&ws.join(".hex/owned.txt"), "operator-staged\n");
        Command::new("git")
            .args(["add", ".hex/owned.txt"])
            .current_dir(ws)
            .status()
            .unwrap();
        write_file(&ws.join(".hex/owned.txt"), "upgrade\n");
        let result = commit_synced_files_since(ws, "9.9.9-test", &snapshot, Some(&owned));
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(ws.join(".hex/owned.txt")).unwrap(),
            "upgrade\n"
        );
        let staged = Command::new("git")
            .args(["show", ":.hex/owned.txt"])
            .current_dir(ws)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&staged.stdout), "operator-staged\n");
    }
}
