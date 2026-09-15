//! Repo-structure lints, kept in one binary (audit item 10, partial merge -
//! moving these into a `hex lint-gates` host is deferred; see the plan).
//!
//! Two lint groups live here:
//!   1. Shellout targets (G1): every Rust shellout target named in
//!      `system/harness/src/` must exist in the repo. A failure means a
//!      script was renamed or deleted without updating the Rust caller (the
//!      shellout-rename bug documented in D5b).
//!   2. Architecture-docs registry (docs/architecture/README.md §4): every
//!      data row in the Registry table must link a real doc carrying the
//!      standard's machine-readable headers, so a malformed row cannot
//!      silently evade enforcement.
//!
//! Both ride `cargo test` and therefore every BOI/workflow gate battery.

use regex::Regex;
use std::path::PathBuf;
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Shellout targets (formerly tests/shellout_paths.rs)
// ---------------------------------------------------------------------------
//
// Checks two sets of paths:
//   1. `const *_REL: &str = "..."` constants in system/harness/src/ - these are
//      explicit module-level declarations of hard shellout dependencies.
//   2. A curated supplement list of critical shellout paths that are referenced
//      via dynamic .join() calls rather than named constants.
//
// Path mapping: .hex/scripts/X → system/scripts/X (the install step copies
// system/scripts/ into .hex/scripts/).

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is system/harness/ - go up two levels to repo root.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("system/")
        .parent()
        .expect("repo root")
        .to_path_buf()
}

/// Map .hex/scripts/X → system/scripts/X. Returns None for paths we don't track.
fn map_to_repo_path(raw: &str) -> Option<String> {
    if let Some(rest) = raw.strip_prefix(".hex/scripts/") {
        Some(format!("system/scripts/{}", rest))
    } else if raw.starts_with("system/scripts/") {
        Some(raw.to_string())
    } else {
        // Skip .hex/secrets/, .hex/templates/, evolution/, etc.
        None
    }
}

/// Supplement: hard shellout paths not expressed as `const *_REL` constants.
/// These are critical runtime dependencies verified by code inspection.
fn supplement_paths() -> Vec<(&'static str, &'static str)> {
    vec![
        // integration_check_all.rs + integration_cmd.rs both shell out to this
        (
            "hex_dir.join(\"system/scripts/hex-integration-check.sh\")",
            "system/scripts/hex-integration-check.sh",
        ),
    ]
}

#[test]
fn all_shellout_targets_exist() {
    let repo = repo_root();
    let src_dir = repo.join("system/harness/src");

    // const *_REL: &str = "..." - explicit hard-shellout declarations
    let re_const = Regex::new(r#"const\s+\w+_REL\s*:\s*&str\s*=\s*"([^"]+)""#).unwrap();

    let mut failures: Vec<String> = Vec::new();
    let mut ok_count: usize = 0;

    let mut check = |label: &str, raw: &str| {
        if let Some(system_path) = map_to_repo_path(raw) {
            let full = repo.join(&system_path);
            if !full.exists() {
                failures.push(format!("MISSING  {label}: {raw} → {system_path}"));
            } else {
                ok_count += 1;
            }
        }
    };

    // Pass 1: scan source for const *_REL declarations
    for entry in WalkDir::new(&src_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
    {
        let rel_src = entry
            .path()
            .strip_prefix(&repo)
            .unwrap_or(entry.path())
            .display()
            .to_string();

        let content = match std::fs::read_to_string(entry.path()) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for cap in re_const.captures_iter(&content) {
            check(&rel_src, &cap[1]);
        }
    }

    // Pass 2: supplement list
    for (label, path) in supplement_paths() {
        check(label, path);
    }

    eprintln!("shellout_paths: {ok_count} OK, {} missing", failures.len());

    if !failures.is_empty() {
        panic!(
            "shellout_paths: {} shellout target(s) missing from repo:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

// ---------------------------------------------------------------------------
// Architecture-docs registry (formerly tests/arch_docs_registry.rs)
// ---------------------------------------------------------------------------
//
// The registry table in docs/architecture/README.md is the index of per-subsystem
// deep dives. This test makes it unbreakable: every data row must link a real doc
// carrying the standard's machine-readable headers, and a row whose Doc column has
// no parseable link FAILS (a malformed row cannot silently evade enforcement).

fn arch_docs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/architecture")
        .canonicalize()
        .expect("docs/architecture must exist at the repo root")
}

/// Extract the data rows of the `## Registry` table: skip the header and
/// separator rows, stop at the first non-table line after the table started.
fn registry_rows(readme: &str) -> Vec<String> {
    let mut rows = Vec::new();
    let mut in_section = false;
    let mut table_started = false;
    for line in readme.lines() {
        let t = line.trim();
        if t.starts_with("## ") {
            if in_section && table_started {
                break;
            }
            in_section = t == "## Registry";
            continue;
        }
        if !in_section {
            continue;
        }
        if t.starts_with('|') {
            table_started = true;
            let cells: Vec<&str> = t.trim_matches('|').split('|').collect();
            let is_separator = cells.iter().all(|c| {
                let c = c.trim();
                !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':')
            });
            let is_header = cells
                .first()
                .map(|c| c.trim().eq_ignore_ascii_case("subsystem"))
                .unwrap_or(false);
            if !is_separator && !is_header {
                rows.push(t.to_string());
            }
        } else if table_started && !t.is_empty() {
            break; // table ended (prose after the table, e.g. the Grandfathered note)
        }
    }
    rows
}

/// Extract a same-directory `*.md` link target from one table row, normalizing
/// `./` prefixes and stripping `#fragment`s. None = no parseable doc link.
fn doc_link(row: &str) -> Option<String> {
    let mut rest = row;
    while let Some(open) = rest.find("](") {
        // `open` + 2 skips the ASCII `](`, so it is always a char boundary;
        // `.get()` keeps the slice panic-free regardless (clippy::string_slice).
        let tail = rest.get(open + 2..).unwrap_or("");
        let close = tail.find(')')?;
        let mut target = tail.get(..close).unwrap_or("").trim();
        if let Some(stripped) = target.strip_prefix("./") {
            target = stripped;
        }
        let target = target.split('#').next().unwrap_or(target);
        if target.ends_with(".md") && !target.contains('/') {
            return Some(target.to_string());
        }
        rest = tail.get(close + 1..).unwrap_or("");
    }
    None
}

#[test]
fn registry_rows_link_real_docs_with_headers() {
    let dir = arch_docs_dir();
    let readme = std::fs::read_to_string(dir.join("README.md"))
        .expect("docs/architecture/README.md must exist");

    let rows = registry_rows(&readme);
    assert!(
        !rows.is_empty(),
        "the Registry table in docs/architecture/README.md has no data rows - \
         the table was removed or reformatted; fix the table or update this parser"
    );

    for row in rows {
        let doc = doc_link(&row).unwrap_or_else(|| {
            panic!(
                "registry row has no parseable same-directory .md link - every row's \
                 Doc column must link its deep dive (standard §4). Row: {row}"
            )
        });
        let path = dir.join(&doc);
        let body = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "registry lists {doc} but it is unreadable at {}: {e} - \
                 every registry row needs a real file (standard §3)",
                path.display()
            )
        });
        assert!(
            body.contains("verified-against:"),
            "{doc} is missing the `verified-against:` header (standard §1)"
        );
        assert!(
            body.contains("source-paths:"),
            "{doc} is missing the `source-paths:` header (standard §1)"
        );
    }
}

#[test]
fn parser_extracts_data_rows_and_links() {
    let sample = "\
## Registry

intro prose with a [decoy](decoy.md) link outside the table

| Subsystem | Doc |
|-----------|-----|
| Memory | [memory.md](./memory.md#anchor) |
| Broken row, no link | plain text |

Grandfathered: `docs/code-intel.md`.

## The Standard of Practice

| Other | [other.md](other.md) |";
    let rows = registry_rows(sample);
    assert_eq!(rows.len(), 2, "two data rows expected, got: {rows:?}");
    assert_eq!(doc_link(&rows[0]), Some("memory.md".to_string()));
    assert_eq!(
        doc_link(&rows[1]),
        None,
        "row without a link must parse as None (and fail the main test)"
    );
}
