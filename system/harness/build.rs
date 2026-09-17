use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    let git_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Append a "-dirty" marker when the working tree has uncommitted changes to
    // tracked files, so harness_version distinguishes a dirty build (e.g. an
    // uncommitted deploy) from its base commit — an uncommitted deploy read as
    // "nothing changed" was the 8-day recall-plateau blind spot. `git status
    // --porcelain --untracked-files=no` lists ONLY tracked-file changes (staged
    // or unstaged); a non-empty result means dirty. Kept cheap (one status call)
    // and infallible: if git status itself cannot run, or exits non-zero, we omit
    // the marker and emit a loud cargo:warning (S6 — no quiet failures) rather
    // than failing the build.
    let git_sha = match Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
    {
        Ok(o) if o.status.success() => {
            if o.stdout.iter().any(|b| !b.is_ascii_whitespace()) {
                format!("{git_sha}-dirty")
            } else {
                git_sha
            }
        }
        Ok(o) => {
            println!(
                "cargo:warning=hex build: `git status` exited {:?} ({}); dirty marker \
                 omitted from HEX_GIT_SHA",
                o.status.code(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
            git_sha
        }
        Err(e) => {
            println!(
                "cargo:warning=hex build: could not run `git status` ({e}); dirty marker \
                 omitted from HEX_GIT_SHA"
            );
            git_sha
        }
    };

    println!("cargo:rustc-env=HEX_GIT_SHA={}", git_sha);

    // Watch the whole harness source tree so the dirty check above reruns on ANY
    // tracked-source edit — not just the *.worker.rs files watched below. Without
    // this, editing e.g. src/memory/assemble.rs would NOT rerun build.rs and the
    // crate would recompile against a cached (possibly clean) HEX_GIT_SHA — the
    // exact stale-version blind spot this task exists to close. A directory watch
    // is cheap (cargo scans mtimes) and covers the motivating case.
    println!("cargo:rerun-if-changed={manifest_dir}/src");

    let out_dir = std::env::var("OUT_DIR").unwrap();

    // Generate personal_mods.rs by DISCOVERING the personal overlay's probe modules
    // (`<overlay_root>/.hex/harness-personal/integration_*.rs`) — never by hardcoding
    // the user's integration names in this (general) repo. Each probe file exposes
    // `pub fn run_probe() -> i32`; the registry maps a probe name derived from the
    // filename (`integration_<foo>.rs` → `<foo>`, `_`→`-`) to it. Only globbed under
    // --features personal; an absent overlay dir → an empty registry (foundation
    // builds clean with nothing personal present) — UNLESS both overlay dirs are
    // absent, in which case `resolve_overlay_root`'s caller below fails loudly
    // instead (S6): a resolved root with nothing under it usually means the wrong
    // root was resolved (e.g. HEX_DIR pinned to a build sandbox), not that the
    // instance genuinely has no personal overlay.
    println!("cargo:rerun-if-env-changed=HEX_OVERLAY_DIR");
    println!("cargo:rerun-if-env-changed=HEX_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let personal_enabled = std::env::var("CARGO_FEATURE_PERSONAL").is_ok();
    let overlay_root = if personal_enabled {
        let root = resolve_overlay_root();
        let modules_dir = std::path::Path::new(&root).join(".hex/modules");
        let harness_personal_dir = std::path::Path::new(&root).join(".hex/harness-personal");
        if !modules_dir.exists() && !harness_personal_dir.exists() {
            panic!(
                "hex build (--features personal): personal overlay root resolved to \
                 {root:?}, but neither {modules_dir:?} nor {harness_personal_dir:?} \
                 exists — this build would silently drop every personal worker. \
                 Set HEX_OVERLAY_DIR (checked first), HEX_DIR, or HOME (fallback: \
                 $HOME/hex) to the real instance directory, not a sandbox path."
            );
        }
        Some(root)
    } else {
        None
    };
    let mut probe_entries: Vec<(String, String, String)> = Vec::new(); // (mod_ident, probe_name, abs_path)
    if let Some(root) = &overlay_root {
        let personal_dir = format!("{root}/.hex/harness-personal");
        println!("cargo:rerun-if-changed={personal_dir}");
        if let Ok(rd) = std::fs::read_dir(&personal_dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                let fname = match path.file_name().and_then(|s| s.to_str()) {
                    Some(f) => f.to_string(),
                    None => continue,
                };
                // Probe modules only: `integration_*.rs` (excludes release.rs, Cargo.toml, …).
                if !(fname.starts_with("integration_") && fname.ends_with(".rs")) {
                    continue;
                }
                println!("cargo:rerun-if-changed={}", path.display());
                let ident = fname.trim_end_matches(".rs").to_string(); // e.g. integration_<foo>
                let probe_name = ident
                    .strip_prefix("integration_")
                    .unwrap_or(&ident)
                    .replace('_', "-"); // mcp-exa
                probe_entries.push((ident, probe_name, path.to_str().unwrap().to_string()));
            }
        }
    }
    probe_entries.sort();
    let mut personal_mods = String::new();
    for (ident, _name, path) in &probe_entries {
        // `{path:?}` emits an escape-safe Rust string literal (overlay path is user-controlled).
        personal_mods.push_str(&format!("#[path = {path:?}] mod {ident};\n"));
    }
    personal_mods
        .push_str("type ProbeFn = fn() -> i32;\n\npub fn probe_registry() -> Vec<(&'static str, ProbeFn)> {\n    vec![");
    for (ident, name, _path) in &probe_entries {
        personal_mods.push_str(&format!("({name:?}, {ident}::run_probe as fn() -> i32), "));
    }
    personal_mods.push_str("]\n}\n");
    std::fs::write(format!("{}/personal_mods.rs", out_dir), personal_mods).unwrap();

    // ---- hex module discovery: recursive *.worker.rs glob → hex_modules.rs ----
    let mut roots: Vec<String> = vec![format!("{manifest_dir}/src/modules")];
    // Personal modules root (out-of-crate) only under --features personal.
    // Reuses the same `overlay_root` resolved (and existence-checked) above so
    // both discovery passes agree on the root and share one loud failure.
    if let Some(root) = &overlay_root {
        roots.push(format!("{root}/.hex/modules"));
    }

    let mut entries: Vec<(String, String)> = Vec::new(); // (mod_ident, abs_path)
    for root in &roots {
        println!("cargo:rerun-if-changed={root}");
        collect_worker_files(
            std::path::Path::new(root),
            std::path::Path::new(root),
            &mut entries,
        );
    }
    entries.sort();

    // Loud (S6) on ident collision — two files mapping to the same mod ident
    // would otherwise surface as an opaque rustc "defined multiple times" error.
    let mut seen = std::collections::HashSet::new();
    for (ident, path) in &entries {
        if !seen.insert(ident.clone()) {
            panic!("hex module: ident collision on '{ident}' (from '{path}') — rename the file");
        }
    }

    let mut gen = String::new();
    for (ident, path) in &entries {
        // `{path:?}` emits a properly-escaped Rust string literal (the personal
        // root is user-controlled — a path with `"`/`\` would otherwise produce
        // invalid generated Rust).
        gen.push_str(&format!("#[path = {path:?}] pub mod {ident};\n"));
    }
    gen.push_str("pub fn module_registry() -> Vec<crate::worker::Worker> {\n    vec![");
    for (ident, _) in &entries {
        gen.push_str(&format!("{ident}::worker(), "));
    }
    gen.push_str("]\n}\n");
    gen.push_str("pub fn module_paths() -> Vec<(String, &'static str)> {\n    vec![");
    for (ident, path) in &entries {
        gen.push_str(&format!("({ident}::worker().name.clone(), {path:?}), "));
    }
    gen.push_str("]\n}\n");
    std::fs::write(format!("{out_dir}/hex_modules.rs"), gen).unwrap();
}

/// Resolve the personal overlay root directory for `--features personal`
/// builds: `HEX_OVERLAY_DIR` first, then `HEX_DIR`, then `$HOME/hex`.
///
/// `HEX_OVERLAY_DIR` is checked first because a checked-out
/// `.cargo/config.toml` can carry a `[env]` table with `force = true` on
/// `HEX_DIR` (see that file), which overrides HEX_DIR for every process cargo
/// launches from a checkout of this repo — including this build script — no
/// matter what an ancestor process (e.g. `hex upgrade`) tries to set it to.
/// `HEX_OVERLAY_DIR` carries no such forced entry, so it survives that
/// override; `hex upgrade` sets it explicitly (see `harness_build_env` in
/// `src/upgrade.rs`). `HEX_DIR` and `$HOME/hex` remain as fallbacks for a bare
/// `cargo build --features personal` run outside that upgrade path.
fn resolve_overlay_root() -> String {
    std::env::var("HEX_OVERLAY_DIR")
        .or_else(|_| std::env::var("HEX_DIR"))
        .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/hex")))
        .expect(
            "HEX_OVERLAY_DIR, HEX_DIR, or HOME must be set to locate the personal overlay \
             (.hex/harness-personal, .hex/modules)",
        )
}

/// Recursively collect `*.worker.rs` files under `dir`. `root` is the glob root
/// used to derive a unique snake_case mod ident from the relative path
/// (`trading/kalshi.worker.rs` → `trading_kalshi`). Absent dir = no-op.
fn collect_worker_files(
    dir: &std::path::Path,
    root: &std::path::Path,
    out: &mut Vec<(String, String)>,
) {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return, // absent / unreadable → contribute nothing
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_worker_files(&path, root, out);
        } else if path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|n| n.ends_with(".worker.rs"))
            .unwrap_or(false)
        {
            let rel = path.strip_prefix(root).unwrap();
            let rel_str = rel.to_str().unwrap();
            let stem = rel_str.trim_end_matches(".worker.rs");
            let ident: String = stem
                .chars()
                .map(|c| {
                    if c == '/' || c == '.' || c == '-' {
                        '_'
                    } else {
                        c
                    }
                })
                .collect();
            if ident.is_empty()
                || ident
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(true)
                || !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                panic!(
                    "hex module: '{}' does not yield a valid Rust identifier (got '{}')",
                    path.display(),
                    ident
                );
            }
            // Per-file rerun trigger so edits to a nested module rebuild
            // (the per-root dir watch alone can miss deep-subdir file changes).
            println!("cargo:rerun-if-changed={}", path.display());
            out.push((ident, path.to_str().unwrap().to_string()));
        }
    }
}
