//! Personalization scanner — faithful Rust port of
//! `system/scripts/sanitize-check.sh` (oss-releaser spec, scope item 2).
//!
//! Scans the repo for personalization that would break for other users:
//! hardcoded home paths, owner-specific identifiers, machine-specific
//! hostnames, runtime-specific invocations, and unguarded `.claude/` paths.
//!
//! ## Port semantics
//!
//! The bash original ran one `grep -rn` per category and piped the output
//! through `grep -v` exclusion filters. This port preserves that split:
//!
//! - the category **pattern** matches the raw file line;
//! - the **exclusion filters** match the composed `./path:line:content`
//!   string — exactly the line the bash pipeline filtered. This reproduces
//!   path-anchored filters (`^./CHANGELOG.md:`) and even the inert ones
//!   (see [`runtime-binary`'s `^\s*#`](registry)) bit-for-bit.
//!
//! Exit semantics are owned by the caller: an empty result is "clean"
//! (bash exit 0), non-empty is "violations found" (bash exit 1). The
//! `hex sanitize` CLI verb maps emptiness to the process exit code; the
//! release gate battery calls [`scan`] in-process and checks emptiness.
//!
//! ## Self-exclusion
//!
//! The bash script excluded its own matches via a `/<scriptname>:` filter.
//! This file instead tags every line that carries an identifier literal
//! (the pattern registry itself, plus test fixtures) with a
//! `personalization-audit` comment — a marker every category already
//! filters per line — so both this scanner and the legacy bash scanner
//! skip the registry without special-casing the file.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;
use walkdir::WalkDir;

/// One personalization violation found by [`scan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Category label, e.g. `hardcoded /Users/ path`.
    pub category: String,
    /// Repo-relative path in grep form (`./system/scripts/foo.sh`).
    pub path: String,
    /// 1-based line number.
    pub line: usize,
    /// Content of the matching line.
    pub content: String,
}

impl Violation {
    /// The `grep -rn` composed form `./path:line:content` — the string the
    /// exclusion filters run against, and what `--verbose` prints.
    fn composed(&self) -> String {
        format!("{}:{}:{}", self.path, self.line, self.content)
    }
}

// ---------------------------------------------------------------------------
// Registry — categories ported 1:1 from sanitize-check.sh, in script order.
// ---------------------------------------------------------------------------

/// Directories every common check skips (`--exclude-dir` set in the bash).
/// `.fastembed_cache` is the gitignored local embedding-model cache the
/// embed tests download (tokenizer vocab blobs contain arbitrary English
/// words, so extension-less full-tree checks would false-positive on it).
const COMMON_EXCLUDE_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".boi",
    "worktrees",
    "__pycache__",
    "dist",
    ".hex",
    ".claude",
    ".fastembed_cache",
];

/// `--exclude-dir` set of the `/opt/homebrew` check (deliberately narrower).
const BREW_EXCLUDE_DIRS: &[&str] = &[".git", "eval", "tests"];

/// `--exclude-dir` set of the secrets-path check (narrowest of all).
const SECRETS_EXCLUDE_DIRS: &[&str] = &[".git"];

/// `--exclude-dir` set of the runtime-binary and `.claude/`-path checks.
const RUNTIME_EXCLUDE_DIRS: &[&str] = &[".git", ".hex", "tests", "eval"];

/// The bash script's self-exclusion (`grep -v "/${SELF}:"`). Kept verbatim so
/// the legacy script passes this scanner during the parity window; harmless
/// once the script is deleted.
const SELF_FILTER: &str = r"/sanitize-check\.sh:";

/// Common false-positive filters shared by the `run_check` categories.
const COMMON_FILTERS: &[&str] = &[
    SELF_FILTER,
    "personalization-audit",
    "PATH=.*opt.homebrew",
    "Co-Authored",
    r"(?i)# example|# e\.g\.|example:",
];

/// File extensions most content checks scan (`--include` set in the bash).
const TEXT_EXTS: &[&str] = &["py", "sh", "yaml", "md", "json", "toml", "rs"];

/// One line-oriented category check.
struct LineCheck {
    /// Category label, used in reports and [`Violation::category`].
    label: &'static str,
    /// Pattern matched against the raw file line.
    pattern: Regex,
    /// Extensions to scan; `None` = every file (no `--include` in the bash).
    include_ext: Option<&'static [&'static str]>,
    /// Directory names pruned at any depth (grep `--exclude-dir`).
    exclude_dirs: &'static [&'static str],
    /// Exclusion filters over the composed `./path:line:content` string; a
    /// line matching ANY filter is not a violation.
    filters: Vec<Regex>,
    /// Suffix appended to the non-verbose category line.
    plain_suffix: &'static str,
    /// Suffix appended to the verbose category header.
    verbose_suffix: &'static str,
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static sanitize pattern must compile")
}

fn compile(patterns: &[&str]) -> Vec<Regex> {
    patterns.iter().map(|p| re(p)).collect()
}

/// The full category registry: the categories ported from the bash script
/// first, in script order, then post-port additions (banned strings).
/// (The tenth ported category — unguarded `.claude/` paths — is file-level,
/// not line-level, and lives in [`claude_path_check_file`].)
fn registry() -> Vec<LineCheck> {
    let mut checks = Vec::new();

    // Absolute user home paths (hardcoded, not via $HOME or ~).
    // Allowed: "/Users/test/..." as a deliberately-fake test fixture path;
    // CHANGELOG entries that document past fixes.
    checks.push(LineCheck {
        label: "hardcoded /Users/ path",
        pattern: re(r"/Users/[a-zA-Z]"),
        include_ext: Some(TEXT_EXTS),
        exclude_dirs: COMMON_EXCLUDE_DIRS,
        filters: {
            let mut f = compile(COMMON_FILTERS);
            // Word boundary so prose forms (the fixture path in backticks,
            // followed by punctuation, or at end of sentence) pass while a
            // longer username sharing the prefix still flags — the leak-guard
            // docs describe the rule they enforce.
            f.push(re(r"/Users/test\b")); // personalization-audit: filter literal
            f.push(re(r"^\./CHANGELOG\.md:"));
            f
        },
        plain_suffix: "",
        verbose_suffix: "",
    });

    // Owner-specific identifiers (exclude projects/ — development workspace,
    // not distributed content). Deliberately matches hyphenated/scoped forms
    // only, so canonical upstream URLs (github.com/<user>/<repo>, e.g. the
    // Cargo.toml git pins) never trip it — covered by a unit test below.
    checks.push(LineCheck {
        label: "mrap-specific identifier",
        pattern: re(r"mrap-hex|mrap-mrap|mike@mrap|mrap\.me|Mike Rapadas"), // personalization-audit: pattern registry
        include_ext: Some(TEXT_EXTS),
        exclude_dirs: &[
            ".git",
            "target",
            "node_modules",
            ".boi",
            "worktrees",
            "__pycache__",
            "dist",
            ".hex",
            ".claude",
            "projects",
        ],
        filters: compile(COMMON_FILTERS),
        plain_suffix: "",
        verbose_suffix: "",
    });

    // Hardcoded ~/hex or $HOME/hex in CODE without an HEX_DIR fallback.
    // Allowed: lines that also reference HEX_DIR; install.sh (installer's
    // default destination); .md docs (descriptive references); doc comments
    // inside source.
    checks.push(LineCheck {
        label: "hardcoded ~/hex or $HOME/hex (use HEX_DIR)",
        pattern: re(r"(\$HOME/hex|~/hex)([^a-zA-Z_/-]|$)"), // personalization-audit: pattern registry
        include_ext: Some(&["py", "sh", "yaml", "json", "toml", "rs"]),
        exclude_dirs: &[
            ".git",
            "target",
            "node_modules",
            ".boi",
            "worktrees",
            "__pycache__",
            "dist",
            ".hex",
            ".claude",
            "projects",
        ],
        filters: compile(&[
            SELF_FILTER,
            "personalization-audit",
            "no-hardcoded-hex-paths",
            "HEX_DIR",
            r"/install\.sh:",
            r"^[^:]+:[0-9]+:\s*///",
            r"(?i)# example|# e\.g\.|example:",
        ]),
        plain_suffix: " — use ${HEX_DIR:-$HOME/hex} or get_hex_dir()",
        verbose_suffix: "",
    });

    // Slack-specific channel IDs.
    checks.push(LineCheck {
        label: "Slack channel IDs",
        pattern: re(r"C0AQZR31EET|C0AUEAFASQP|C0B05456Z2L"), // personalization-audit: pattern registry
        include_ext: None,
        exclude_dirs: COMMON_EXCLUDE_DIRS,
        filters: compile(COMMON_FILTERS),
        plain_suffix: "",
        verbose_suffix: "",
    });

    // Tailscale hostname/IP specific to one machine (skip .rs — Rust
    // integration consts are intentional configuration defaults overridable
    // via env vars; check docs/scripts only).
    checks.push(LineCheck {
        label: "Tailscale hostname/IP",
        pattern: re(r"tailbd5748|mac-mini\.tail|100\.101\.9\."), // personalization-audit: pattern registry
        include_ext: Some(&["py", "sh", "yaml", "md", "json", "toml"]),
        exclude_dirs: COMMON_EXCLUDE_DIRS,
        filters: compile(COMMON_FILTERS),
        plain_suffix: "",
        verbose_suffix: "",
    });

    // macOS LaunchAgent plists tied to the owner's namespace. The four
    // canonical signing product identifiers are app bundle identities, not
    // LaunchAgent labels, and are intentionally retained in signing code and
    // its tests. Other labels in the personal namespace still fail closed.
    checks.push(LineCheck {
        label: "com.mrap. LaunchAgent",
        pattern: re(r"com\.mrap\."), // personalization-audit: pattern registry
        include_ext: Some(&["py", "sh", "plist"]),
        exclude_dirs: COMMON_EXCLUDE_DIRS,
        filters: {
            let mut f = compile(COMMON_FILTERS);
            f.push(re(r"com\.mrap\.(?:hex(?:\.(?:scipd|cq))?|boi)\b")); // personalization-audit: canonical signing identities
            f.push(re(
                r#"com\.mrap\.(?:" \+ (?:product|identity)|\{self\.product\})"#,
            )); // personalization-audit: canonical signing identity construction
            f
        },
        plain_suffix: "",
        verbose_suffix: "",
    });

    // Hardcoded /opt/homebrew when NOT behind an existence guard. Legitimate
    // uses (inside "if [ -d /opt/homebrew ]" blocks, macOS VM builders) are
    // excluded.
    checks.push(LineCheck {
        label: "hardcoded /opt/homebrew",
        pattern: re("/opt/homebrew"),
        include_ext: Some(&["py", "sh"]),
        exclude_dirs: BREW_EXCLUDE_DIRS,
        filters: compile(&[
            SELF_FILTER,
            "if.*-d.*opt/homebrew",
            r"\[ -d.*opt/homebrew",
            r"\[\[ -d.*opt/homebrew",
            "opt/homebrew.*&&|&&.*opt/homebrew",
            "_add_to_path",
            "personalization-audit",
            "PATH=.*opt.homebrew",
        ]),
        plain_suffix: "",
        verbose_suffix: "",
    });

    // Hardcoded secrets paths with actual credentials (not generic
    // placeholders).
    checks.push(LineCheck {
        label: "hardcoded secrets path",
        pattern: re(r"secrets/slack-bot-token|\.hex/secrets/[a-zA-Z][a-zA-Z0-9_-]*\.(env|key)"),
        include_ext: Some(&["py", "sh"]),
        exclude_dirs: SECRETS_EXCLUDE_DIRS,
        filters: compile(&[
            SELF_FILTER,
            "personalization-audit",
            "PATH=.*opt.homebrew",
            "<name>|REPLACE_ME|YOUR_",
            "HEX_DIR.*secrets|HEX_ROOT.*secrets",
        ]),
        plain_suffix: "",
        verbose_suffix: "",
    });

    // Claude-Code-only: direct 'claude -p' or 'claude exec' invocations that
    // bypass the runtime abstraction (hex_invoke / env.sh wrapper). New
    // scripts should use hex_invoke so they work on both Claude Code and
    // Codex runtimes.
    checks.push(LineCheck {
        label: "hardcoded-runtime-binary",
        pattern: re(r"claude\s+-p\b|exec\s+claude\b|\bcodex exec\b"),
        include_ext: Some(&["sh"]),
        exclude_dirs: RUNTIME_EXCLUDE_DIRS,
        filters: compile(&[
            SELF_FILTER,
            r"/env\.sh:",
            r"/runtime\.sh:",
            r"hex-agent-spawn\.sh:",
            r"llm-cli\.sh:",
            r"system-introspection\.sh:",
            r"hex-ui-feedback-tick\.sh:",
            "personalization-audit",
            "PATH=.*opt.homebrew",
            // Inert in the bash too (composed lines start "./path:", never
            // "#"), kept for filter-registry parity.
            r"^\s*#",
            r"\.legacy\.",
        ]),
        plain_suffix: " — use hex_invoke instead of direct claude/codex invocation",
        verbose_suffix: " — use hex_invoke instead of direct claude/codex invocation",
    });

    // Banned strings — post-port addition (not in the bash original). The
    // sunset session-manager's name keeps creeping back into docs, comments,
    // and test names (purged 4× as of 2026-06-11); this check makes the purge
    // permanent. The pattern encodes the word's second letter as a `\x61` hex
    // escape (resolved by the regex crate) so this source file never carries
    // the literal it bans. Every file type is scanned (`grep -ri` semantics,
    // Slack-ID precedent) and docs/ and tests/ are deliberately NOT excluded
    // — that's where the recurring hits live.
    checks.push(LineCheck {
        label: "banned string: sunset session-manager name",
        pattern: re(r"(?i)h\x61ppy"),
        include_ext: None,
        exclude_dirs: COMMON_EXCLUDE_DIRS,
        filters: compile(COMMON_FILTERS),
        plain_suffix: " — sunset tool name; reword (see purge 2026-06-11)",
        verbose_suffix: "",
    });

    checks
}

// ---------------------------------------------------------------------------
// Tenth category — unguarded `.claude/` paths. File-level, two-stage:
// a file (not line) is exempt by name or by carrying a runtime guard.
// ---------------------------------------------------------------------------

const CLAUDE_PATH_LABEL: &str = "hardcoded-.claude/-path-no-fallback";
const CLAUDE_PATH_SUFFIX: &str = " — add HEX_RUNTIME guard or .codex fallback";

/// Path-level exemptions for the `.claude/` check (`grep -v` over the
/// `grep -rln` file list in the bash). Note the self filter here has no
/// trailing colon — it matched a path, not a composed line.
const CLAUDE_PATH_FILE_FILTERS: &[&str] = &[
    r"/sanitize-check\.sh",
    r"/env\.sh$",
    r"/runtime\.sh$",
    r"/doctor\.sh$",
    r"/install\.sh$",
    r"/backup_session\.sh$",
    r"/consolidate\.sh$",
    r"/cleanup-project-jsonl\.sh$",
    r"/test_upgrade_prune\.sh$",
    "personalization-audit",
    "PATH=.*opt.homebrew",
    r"\.legacy\.",
];

/// Scripts referencing `.claude/` directly break on Codex which uses
/// `.codex/`. A file is allowed if its path is exempt or its content also
/// references `HEX_RUNTIME`, `.codex`, or `CLAUDE_PROJECT_MEMORY` as a
/// fallback guard; otherwise every `.claude/` line is a violation.
fn claude_path_check_file(rel_path: &str, content: &str) -> Vec<Violation> {
    let path = format!("./{rel_path}");
    if CLAUDE_PATH_FILE_FILTERS
        .iter()
        .any(|f| re(f).is_match(&path))
    {
        return Vec::new();
    }
    if re(r"HEX_RUNTIME|\.codex|CLAUDE_PROJECT_MEMORY").is_match(content) {
        return Vec::new();
    }
    let pattern = re(r"\.claude/");
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| pattern.is_match(line))
        .map(|(idx, line)| Violation {
            category: CLAUDE_PATH_LABEL.to_string(),
            path: path.clone(),
            line: idx + 1,
            content: line.to_string(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Artifact-detection category — the second leak class (build artifacts that
// are mechanically uncommittable). Keyed on the git index (`git ls-files`),
// NOT a filesystem walk: the tree gitignores `target/` and uses out-of-repo
// `CARGO_TARGET_DIR` plus per-worktree `target-cq` dirs, so a raw walk would
// false-positive on gitignored build dirs. Only paths git actually tracks or
// has staged can leak toward a public branch, so those are what we check.
// ---------------------------------------------------------------------------

const ARTIFACT_LABEL: &str = "committed build artifact (deny set)";
const ARTIFACT_SUFFIX: &str =
    " — target*/, node_modules/, *.rlib, *.rmeta, *.o, .DS_Store must never be committed";

/// Deny-set matcher over a repo-relative, `/`-separated path (git form). True
/// when any directory component is `node_modules` or begins with `target`, or
/// the basename ends in `.rlib`/`.rmeta`/`.o` or is exactly `.DS_Store`.
fn is_artifact_path(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return false;
    }
    // Every element except the last is a directory component.
    for comp in &parts[..parts.len() - 1] {
        if *comp == "node_modules" || comp.starts_with("target") {
            return true;
        }
    }
    let base = *parts.last().expect("parts is non-empty");
    base == ".DS_Store"
        || base.ends_with(".rlib")
        || base.ends_with(".rmeta")
        || base.ends_with(".o")
}

/// Scan the git index for deny-set build artifacts. A non-git tree has no
/// tracked paths — zero artifacts, which is correct (not a swallowed error).
/// A git command that genuinely fails IS surfaced loudly (S6: no quiet
/// failures) via the returned error.
fn tracked_artifacts(root: &Path) -> Result<Vec<Violation>> {
    if !root.join(".git").exists() {
        return Ok(Vec::new());
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .with_context(|| format!("sanitize: cannot run `git ls-files` in {}", root.display()))?;
    if !out.status.success() {
        anyhow::bail!(
            "sanitize: `git ls-files` failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut found = Vec::new();
    for chunk in out.stdout.split(|b| *b == 0) {
        if chunk.is_empty() {
            continue;
        }
        let rel = String::from_utf8_lossy(chunk).into_owned();
        if is_artifact_path(&rel) {
            found.push(Violation {
                category: ARTIFACT_LABEL.to_string(),
                path: format!("./{rel}"),
                line: 0,
                content: rel,
            });
        }
    }
    Ok(found)
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Run one line check against a single file's content. Pattern matches the
/// raw line; filters match the composed `./path:line:content` string.
fn check_content(check: &LineCheck, rel_path: &str, content: &str) -> Vec<Violation> {
    let mut out = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if !check.pattern.is_match(line) {
            continue;
        }
        let violation = Violation {
            category: check.label.to_string(),
            path: format!("./{rel_path}"),
            line: idx + 1,
            content: line.to_string(),
        };
        let composed = violation.composed();
        if check.filters.iter().any(|f| f.is_match(&composed)) {
            continue;
        }
        out.push(violation);
    }
    out
}

/// Does `path` pass the check's `--include` extension set?
fn included(check: &LineCheck, path: &Path) -> bool {
    match check.include_ext {
        None => true,
        Some(exts) => path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e)),
    }
}

/// Walk `root`, pruning excluded directory NAMES at any depth (grep
/// `--exclude-dir` semantics — basename match, dirs only). Symlinks are not
/// followed and not read, matching `grep -r`.
fn walk_files(root: &Path, exclude_dirs: &[&str]) -> Vec<(String, PathBuf)> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 || !e.file_type().is_dir() {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            !exclude_dirs.iter().any(|d| *d == name)
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let rel = e
                .path()
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .into_owned();
            Some((rel, e.into_path()))
        })
        .collect()
}

/// Read a file as lossy UTF-8. Unreadable files are skipped, matching the
/// bash's `grep ... 2>/dev/null`.
fn read_lossy(path: &Path) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

// ---------------------------------------------------------------------------
// Reporting — same shape and streams as the bash (red → stderr, green/plain
// → stdout, ANSI always on).
// ---------------------------------------------------------------------------

fn red(msg: &str) {
    eprintln!("\x1b[31m{msg}\x1b[0m");
}

fn green(msg: &str) {
    println!("\x1b[32m{msg}\x1b[0m");
}

fn report_category(
    label: &str,
    plain_suffix: &str,
    verbose_suffix: &str,
    found: &[Violation],
    verbose: bool,
) {
    if verbose {
        red(&format!(
            "  [{label}] {} violation(s){verbose_suffix}:",
            found.len()
        ));
        for v in found {
            eprintln!("    {}", v.composed());
        }
    } else {
        red(&format!(
            "  [{label}] {} violation(s){plain_suffix}",
            found.len()
        ));
    }
}

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Scan `repo_root` for personalization violations, printing the same report
/// the bash script printed (`verbose` = the `--verbose` flag: per-line
/// detail instead of per-category counts).
///
/// Returns every violation found. Empty ⇒ clean (the bash exited 0);
/// non-empty ⇒ violations (the bash exited 1). Callers own the exit code:
/// the `hex sanitize` CLI verb maps emptiness to 0/1, and the release gate
/// battery consumes the result in-process.
pub fn scan(repo_root: &Path, verbose: bool) -> Result<Vec<Violation>> {
    let root = repo_root
        .canonicalize()
        .with_context(|| format!("sanitize: cannot access repo root {}", repo_root.display()))?;

    println!("Scanning for personalization violations...");
    println!();

    let mut all: Vec<Violation> = Vec::new();
    let mut hit_labels: Vec<&'static str> = Vec::new();

    for check in registry() {
        let mut found = Vec::new();
        for (rel, path) in walk_files(&root, check.exclude_dirs) {
            if !included(&check, &path) {
                continue;
            }
            let Some(content) = read_lossy(&path) else {
                continue;
            };
            found.extend(check_content(&check, &rel, &content));
        }
        if !found.is_empty() {
            hit_labels.push(check.label);
            report_category(
                check.label,
                check.plain_suffix,
                check.verbose_suffix,
                &found,
                verbose,
            );
            all.extend(found);
        }
    }

    let mut found = Vec::new();
    for (rel, path) in walk_files(&root, RUNTIME_EXCLUDE_DIRS) {
        let is_script = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "sh" || e == "py");
        if !is_script {
            continue;
        }
        let Some(content) = read_lossy(&path) else {
            continue;
        };
        found.extend(claude_path_check_file(&rel, &content));
    }
    if !found.is_empty() {
        hit_labels.push(CLAUDE_PATH_LABEL);
        report_category(
            CLAUDE_PATH_LABEL,
            CLAUDE_PATH_SUFFIX,
            CLAUDE_PATH_SUFFIX,
            &found,
            verbose,
        );
        all.extend(found);
    }

    // Second leak class: git-tracked build artifacts. Appended AFTER every
    // content category so the `/Users/` category always registers first.
    let artifacts = tracked_artifacts(&root)?;
    if !artifacts.is_empty() {
        hit_labels.push(ARTIFACT_LABEL);
        report_category(
            ARTIFACT_LABEL,
            ARTIFACT_SUFFIX,
            ARTIFACT_SUFFIX,
            &artifacts,
            verbose,
        );
        all.extend(artifacts);
    }

    println!();
    if hit_labels.is_empty() {
        green("CLEAN — no personalization violations found");
    } else {
        red(&format!("VIOLATIONS FOUND in: {}", hit_labels.join(" ")));
        red("Run with --verbose for details. Fix before pushing.");
    }

    Ok(all)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn check(label: &str) -> LineCheck {
        registry()
            .into_iter()
            .find(|c| c.label == label)
            .unwrap_or_else(|| panic!("no check labelled {label}"))
    }

    /// Run one content string through every line category + the file-level
    /// `.claude/` category, as if it were the file at `rel_path`.
    fn sweep(rel_path: &str, content: &str) -> Vec<Violation> {
        let path = Path::new(rel_path);
        let mut out = Vec::new();
        for c in registry() {
            if included(&c, path) {
                out.extend(check_content(&c, rel_path, content));
            }
        }
        let is_script = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "sh" || e == "py");
        if is_script {
            out.extend(claude_path_check_file(rel_path, content));
        }
        out
    }

    // -- Known-violation fixture ---------------------------------------------

    #[test]
    fn users_path_fixture_is_flagged() {
        let fixture = "DATA=/Users/alice/hex-data/input.csv"; // personalization-audit: test fixture
        let v = check_content(
            &check("hardcoded /Users/ path"),
            "system/scripts/foo.sh",
            fixture,
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].path, "./system/scripts/foo.sh");
        assert_eq!(v[0].line, 1);
        assert_eq!(v[0].category, "hardcoded /Users/ path");
    }

    #[test]
    fn users_test_fixture_prose_forms_pass_but_prefix_names_flag() {
        let c = check("hardcoded /Users/ path");
        // Sanctioned fake-fixture path in code and in prose (backticks,
        // punctuation, end of line) — all pass.
        for good in [
            r#"path = "/Users/test/hex/file.txt""#,
            "the `/Users/test` fixture allowance",
            "The single allowance is /Users/test",
            "allowance is /Users/test.",
        ] {
            assert!(
                check_content(&c, "docs/x.md", good).is_empty(),
                "must pass: {good}"
            );
        }
        // A real-looking username sharing the prefix still flags.
        let bad = "DATA=/Users/testuser/input.csv"; // personalization-audit: test fixture
        assert_eq!(check_content(&c, "docs/x.md", bad).len(), 1);
    }

    #[test]
    fn identifier_fixture_is_flagged() {
        let fixture = "workspace = mrap-hex"; // personalization-audit: test fixture
        let v = check_content(&check("mrap-specific identifier"), "docs/setup.md", fixture);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn canonical_signing_ids_pass_but_personal_launchagent_labels_flag() {
        let c = check("com.mrap. LaunchAgent");
        for canonical_id in [
            "com.mrap.hex",
            "com.mrap.boi",
            "com.mrap.hex.scipd",
            "com.mrap.hex.cq",
        ] {
            assert!(
                check_content(&c, "system/scripts/macos-signing.py", canonical_id).is_empty(),
                "canonical signing identity must pass: {canonical_id}"
            );
        }
        for canonical_construction in [
            r#"self.identifier = "com.mrap." + product"#,
            r#"identifier "com.mrap.{self.product}""#,
            r#"result["identifier"] = "com.mrap." + identity"#,
        ] {
            assert!(
                check_content(&c, "tests/test_macos_signing.py", canonical_construction).is_empty(),
                "canonical signing identity construction must pass: {canonical_construction}"
            );
        }
        let personal_label = "<key>Label</key><string>com.mrap.private-agent</string>";
        assert_eq!(
            check_content(
                &c,
                "system/templates/launchd/private-agent.plist",
                personal_label
            )
            .len(),
            1
        );
    }

    #[test]
    fn hex_path_fixture_flagged_and_hex_dir_guarded_line_excluded() {
        let c = check("hardcoded ~/hex or $HOME/hex (use HEX_DIR)");
        let bad = "cd ~/hex && ls"; // personalization-audit: test fixture
        assert_eq!(check_content(&c, "system/scripts/x.sh", bad).len(), 1);
        // A HEX_DIR fallback on the same line is the sanctioned form.
        let good = r#"ROOT="${HEX_DIR:-$HOME/hex}""#;
        assert!(check_content(&c, "system/scripts/x.sh", good).is_empty());
    }

    // -- The github.com exclusion --------------------------------------------
    // Canonical upstream URLs carry the owner's username but must never trip
    // the identifier registry (the patterns are deliberately hyphenated /
    // scoped forms, never the bare username).

    #[test]
    fn github_canonical_url_is_not_flagged() {
        let content = r#"iii-sdk = { git = "https://github.com/mrap/hex-iii", rev = "cbc21ca" }"#;
        assert!(sweep("system/harness/Cargo.toml", content).is_empty());
        let doc = "Clone https://github.com/mrap/hex-foundation to get started.";
        assert!(sweep("README.md", doc).is_empty());
    }

    // -- Exclusion rules ------------------------------------------------------

    #[test]
    fn users_test_fixture_path_is_excluded() {
        let c = check("hardcoded /Users/ path");
        let line = r#"path = "/Users/test/workspace/repo""#; // personalization-audit: test fixture
        assert!(check_content(&c, "tests/fixture.toml", line).is_empty());
    }

    #[test]
    fn changelog_users_line_is_excluded() {
        let c = check("hardcoded /Users/ path");
        let line = "- fixed a hardcoded /Users/alice path in env.sh"; // personalization-audit: test fixture
        assert!(check_content(&c, "CHANGELOG.md", line).is_empty());
        // ...but only for CHANGELOG.md itself.
        assert_eq!(check_content(&c, "docs/notes.md", line).len(), 1);
    }

    #[test]
    fn example_comment_line_is_excluded() {
        let c = check("hardcoded /Users/ path");
        let line = "cp /Users/alice/in.txt out.txt  # example: copying input"; // personalization-audit: test fixture
        assert!(check_content(&c, "docs/guide.md", line).is_empty());
    }

    #[test]
    fn legacy_bash_scanner_lines_are_self_excluded() {
        let c = check("mrap-specific identifier");
        let line = "run_check mrap-hex"; // personalization-audit: test fixture
        assert!(check_content(&c, "system/scripts/sanitize-check.sh", line).is_empty());
        assert_eq!(check_content(&c, "system/scripts/other.sh", line).len(), 1);
    }

    #[test]
    fn brew_guarded_lines_are_excluded() {
        let c = check("hardcoded /opt/homebrew");
        assert_eq!(
            check_content(&c, "system/scripts/x.sh", "BREW=/opt/homebrew/bin/brew").len(),
            1
        );
        let guarded = "if [ -d /opt/homebrew ]; then PATH=/opt/homebrew/bin:$PATH; fi";
        assert!(check_content(&c, "system/scripts/x.sh", guarded).is_empty());
    }

    // -- .claude/ path check (file-level, two-stage) --------------------------

    #[test]
    fn claude_path_guard_decision_table() {
        // Unguarded reference → violation.
        let v = claude_path_check_file("system/scripts/sync.sh", "cat .claude/settings.json");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].category, CLAUDE_PATH_LABEL);
        // Runtime guard in the same file → allowed.
        let guarded = "DIR=.claude/\n[ \"$HEX_RUNTIME\" = codex ] && DIR=.codex/";
        assert!(claude_path_check_file("system/scripts/sync.sh", guarded).is_empty());
        // Allowlisted filename → allowed.
        assert!(
            claude_path_check_file("system/scripts/env.sh", "cat .claude/settings.json").is_empty()
        );
    }

    // -- Clean content ---------------------------------------------------------

    #[test]
    fn clean_content_has_no_violations() {
        let content = "fn main() {\n    println!(\"hello\");\n}\n";
        assert!(sweep("system/harness/src/example_clean.rs", content).is_empty());
        let script = "#!/usr/bin/env bash\nset -euo pipefail\necho ok\n";
        assert!(sweep("system/scripts/ok.sh", script).is_empty());
    }

    // -- Banned strings ---------------------------------------------------------
    // Fixtures build the banned word by concatenation (`concat!`) so this
    // source file never contains the literal — the same invariant the check
    // itself guards.

    const BANNED_LABEL: &str = "banned string: sunset session-manager name";

    #[test]
    fn banned_string_planted_in_temp_repo_is_flagged_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Mixed case — the check is case-insensitive.
        fs::write(
            root.join("notes.md"),
            concat!("the H", "aPpY", " path needs rewording\n"),
        )
        .unwrap();
        let found = scan(root, false).unwrap();
        assert_eq!(found.len(), 1, "expected exactly one violation: {found:?}");
        assert_eq!(found[0].category, BANNED_LABEL);
        assert_eq!(found[0].path, "./notes.md");
        assert_eq!(found[0].line, 1);
    }

    #[test]
    fn banned_string_clean_repo_has_zero_violations() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("notes.md"), "the success path is documented\n").unwrap();
        fs::write(root.join("ok.sh"), "#!/usr/bin/env bash\necho ok\n").unwrap();
        let found = scan(root, false).unwrap();
        assert!(
            found.iter().all(|v| v.category != BANNED_LABEL),
            "clean repo tripped the banned-string check: {found:?}"
        );
        assert!(found.is_empty(), "clean repo has violations: {found:?}");
    }

    #[test]
    fn banned_string_matches_every_extension() {
        // `include_ext: None` — grep -ri semantics, any file type.
        let c = check(BANNED_LABEL);
        let fixture = concat!("status = \"unh", "appy\"\n");
        let v = check_content(&c, "config/state.ini", fixture);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].category, BANNED_LABEL);
        assert_eq!(sweep("config/state.ini", fixture).len(), 1);
        // The word inside identifiers is caught too.
        let fn_line = concat!("fn def_h", "appy_path_exit_0() {}");
        assert_eq!(check_content(&c, "tests/cli.rs", fn_line).len(), 1);
    }

    // -- End-to-end scan over a temp tree --------------------------------------

    #[test]
    fn scan_finds_exactly_the_planted_violation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("bad.sh"), "DATA=/Users/alice/data.csv\n").unwrap(); // personalization-audit: test fixture
        fs::write(
            root.join("ok.md"),
            "See https://github.com/mrap/hex-foundation for the canonical repo.\n",
        )
        .unwrap();
        // Excluded dir: common checks must not descend into .hex/.
        fs::create_dir_all(root.join(".hex")).unwrap();
        fs::write(root.join(".hex").join("skip.sh"), "X=/Users/alice/hidden\n").unwrap(); // personalization-audit: test fixture
                                                                                          // Extension not in the include set for the /Users/ check.
        fs::write(root.join("note.txt"), "/Users/alice/notes\n").unwrap(); // personalization-audit: test fixture

        let found = scan(root, false).unwrap();
        assert_eq!(found.len(), 1, "expected exactly one violation: {found:?}");
        assert_eq!(found[0].category, "hardcoded /Users/ path");
        assert_eq!(found[0].path, "./bad.sh");
        assert_eq!(found[0].line, 1);

        // Verbose mode changes printing only, never the result.
        let verbose = scan(root, true).unwrap();
        assert_eq!(verbose, found);
    }

    #[test]
    fn scan_errors_on_missing_root() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(scan(&missing, false).is_err());
    }

    // -- Artifact deny-set matcher ----------------------------------------------

    #[test]
    fn artifact_matcher_denies_deny_set_and_allows_source() {
        // Deny set: any target*/ dir, node_modules/, *.rlib, *.rmeta, *.o, .DS_Store.
        assert!(is_artifact_path("target/release/app"));
        assert!(is_artifact_path("target-iso/app.rlib"));
        assert!(is_artifact_path("target-cq/deps/foo.rmeta"));
        assert!(is_artifact_path("system/harness/target/x.o"));
        assert!(is_artifact_path("web/node_modules/pkg/index.js"));
        assert!(is_artifact_path("build/app.o"));
        assert!(is_artifact_path("docs/.DS_Store"));
        // Source, config, and docs are never artifacts.
        assert!(!is_artifact_path("system/harness/src/sanitize.rs"));
        assert!(!is_artifact_path("docs/notes.org")); // ".org" ends in "org", not ".o"
        assert!(!is_artifact_path("README.md"));
        assert!(!is_artifact_path("Cargo.toml"));
        // The `target*/` glob is intentionally broad: any dir beginning with
        // "target" is denied (target/, target-iso/, target-cq/, targeting/).
        assert!(is_artifact_path("targeting/plan.md"));
    }

    // -- Parity harness ---------------------------------------------------------
    // Scans the real repo this crate lives in; run explicitly alongside the
    // legacy bash script to verify both agree the tree is clean:
    //   cargo test -p hex-harness --lib sanitize:: -- --ignored

    #[test]
    #[ignore = "full-tree scan of the live repo; run explicitly for parity checks"]
    fn parity_full_tree_scan_is_clean() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let found = scan(&root, true).unwrap();
        assert!(
            found.is_empty(),
            "repo tree has personalization violations: {found:#?}"
        );
    }
}
