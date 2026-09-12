use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run(hex_dir: &Path) -> i32 {
    let now_utc = chrono_utc_now();
    let mut issues: Vec<String> = Vec::new();
    let mut lines: Vec<String> = Vec::new();

    // 1. Memory index rebuild — best-effort via native entry point
    match crate::memory::index::run_index(hex_dir, false) {
        0 => lines.push("OK: memory index rebuilt".to_string()),
        _ => {
            lines.push("memory reindex: skipped (no native entry point)".to_string());
        }
    }

    // 2. Stale-reference check — scan key docs for broken relative links
    let scan_targets = stale_ref_scan_targets(hex_dir);
    let stale_refs = find_stale_refs(hex_dir, &scan_targets);
    if stale_refs.is_empty() {
        lines.push("OK: no stale file references found".to_string());
    } else {
        for r in &stale_refs {
            let msg = format!("STALE: {r}");
            lines.push(msg.clone());
            issues.push(msg);
        }
    }

    // 3. Evolution pipeline files
    for f in &["observations.md", "suggestions.md", "changelog.md"] {
        let p = hex_dir.join("evolution").join(f);
        if p.exists() {
            lines.push(format!("OK: evolution/{f} exists"));
        } else {
            let msg = format!("ISSUE: missing evolution/{f}");
            lines.push(msg.clone());
            issues.push(msg);
        }
    }

    // 4. Orphan project dirs
    for msg in orphan_project_issues(hex_dir) {
        lines.push(msg.clone());
        issues.push(msg);
    }

    // 5. Consolidator-freshness check
    let audit_freshness = check_audit_freshness(hex_dir);
    match audit_freshness {
        Ok(msg) => lines.push(msg),
        Err(msg) => {
            lines.push(msg.clone());
            issues.push(msg);
        }
    }

    // Build report
    let header = format!("=== Consolidation Report — {now_utc} ===");
    let issue_count = issues.len();
    let mut report_lines = vec![header.clone()];
    report_lines.extend(lines.clone());
    report_lines.push(String::new());
    report_lines.push(format!("Issues found: {issue_count}"));
    let report = report_lines.join("\n");

    println!("{report}");

    // Write report to evolution/consolidation-latest.log
    let log_dir = hex_dir.join("evolution");
    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!("hex doctor consolidate: cannot create evolution/: {e}");
    } else {
        let log_path = log_dir.join("consolidation-latest.log");
        match fs::File::create(&log_path) {
            Ok(mut f) => {
                let _ = f.write_all(report.as_bytes());
                let _ = f.write_all(b"\n");
            }
            Err(e) => eprintln!("hex doctor consolidate: cannot write log: {e}"),
        }
    }

    if issue_count == 0 {
        0
    } else {
        1
    }
}

/// Scan `hex_dir/projects/*` for orphaned project directories: any dir (other
/// than `_archive`) missing its required file(s). Returns one ORPHAN/ISSUE
/// message per problem found, in the same form `run()` reports them.
fn orphan_project_issues(hex_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let projects_dir = hex_dir.join("projects");
    if projects_dir.is_dir() {
        match fs::read_dir(&projects_dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str == "_archive" {
                        continue;
                    }
                    let has_context = path.join("context.md").exists();
                    // charter.yaml is no longer required: the agent fleet and per-project
                    // charters were removed in the fleet teardown. Requiring it would flag
                    // every project as ORPHAN.
                    // checkpoint.md is no longer required either: it belonged to the same
                    // retired fleet era. CLAUDE.md names context.md as the canonical
                    // project file, so that's the only file we require here.
                    if !has_context {
                        out.push(format!("ORPHAN: projects/{name_str} missing context.md"));
                    }
                }
            }
            Err(e) => {
                out.push(format!("ISSUE: cannot read projects/ dir: {e}"));
            }
        }
    } else {
        out.push("ISSUE: projects/ directory does not exist".to_string());
    }
    out
}

fn chrono_utc_now() -> String {
    // Format current UTC time as ISO-8601 without pulling in chrono
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400; // days since epoch
                             // Compute year/month/day from days-since-epoch (Gregorian)
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm: civil calendar from days-since-epoch (1970-01-01)
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn stale_ref_scan_targets(hex_dir: &Path) -> Vec<PathBuf> {
    let mut targets = vec![
        hex_dir.join("CLAUDE.md"),
        hex_dir.join("AGENTS.md"),
        hex_dir.join("templates/AGENTS.md"),
    ];
    // docs/**/*.md
    let docs_dir = hex_dir.join("docs");
    collect_md_files(&docs_dir, &mut targets);
    targets
}

fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_md_files(&p, out);
            } else if p.extension().map(|e| e == "md").unwrap_or(false) {
                out.push(p);
            }
        }
    }
}

fn find_stale_refs(hex_dir: &Path, targets: &[PathBuf]) -> Vec<String> {
    let mut stale = Vec::new();
    for file in targets {
        if !file.exists() {
            continue;
        }
        let content = match fs::read_to_string(file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let rel_file = file.strip_prefix(hex_dir).unwrap_or(file);
        for link in extract_relative_links(&content) {
            // Resolve relative to the file's directory
            let resolved = if let Some(parent) = file.parent() {
                parent.join(&link)
            } else {
                hex_dir.join(&link)
            };
            // Normalize path (remove ../ etc)
            let canonical = normalize_path(&resolved);
            if !canonical.exists() {
                stale.push(format!("{}: broken link to '{link}'", rel_file.display()));
            }
        }
    }
    stale
}

fn extract_relative_links(content: &str) -> Vec<String> {
    // Match only genuine markdown links: ](path) — the `]` before `(` is required.
    // Reject: whitespace in target, http/https/mailto/# schemes.
    let mut links = Vec::new();
    let extensions = [".md", ".yaml", ".sh", ".py", ".rs"];
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Require `](` pattern — skip bare `(`
        if i > 0 && bytes[i] == b'(' && bytes[i - 1] == b']' {
            let start = i + 1;
            let mut end = start;
            let mut depth = 1usize;
            while end < bytes.len() && depth > 0 {
                match bytes[end] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                if depth > 0 {
                    end += 1;
                }
            }
            if depth == 0 && end > start {
                // Boundary proof: `start` is one past an ASCII '(' and `end` sits at
                // an ASCII ')' (or content.len()). ASCII bytes never occur inside a
                // multi-byte UTF-8 sequence, so both bounds are char boundaries.
                #[allow(clippy::string_slice)]
                let candidate = &content[start..end];
                // Strip trailing anchors
                let candidate = candidate.split('#').next().unwrap_or(candidate).trim();
                if !candidate.is_empty()
                    && !candidate.contains(char::is_whitespace)
                    && !candidate.starts_with("http://")
                    && !candidate.starts_with("https://")
                    && !candidate.starts_with('#')
                    && !candidate.starts_with("mailto:")
                    && extensions.iter().any(|ext| candidate.ends_with(ext))
                {
                    links.push(candidate.to_string());
                }
            }
            i = end + 1;
        } else {
            i += 1;
        }
    }
    links
}

fn normalize_path(path: &Path) -> PathBuf {
    // Resolve .. and . without hitting the filesystem
    let mut components = Vec::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

fn check_audit_freshness(hex_dir: &Path) -> Result<String, String> {
    let evo_dir = hex_dir.join("evolution");
    if !evo_dir.is_dir() {
        return Err("LLM consolidation stale — run hex memory consolidate full".to_string());
    }
    // A nightly artifact checked with a 30-day window masked the 2026-06-10
    // miss for what would have been weeks — keep this tight (48h).
    let fresh_window_secs: u64 = 48 * 3600;
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut newest_mtime: Option<u64> = None;

    if let Ok(entries) = fs::read_dir(&evo_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("consolidation-audit-") && name_str.ends_with(".md") {
                if let Ok(meta) = p.metadata() {
                    if let Ok(modified) = meta.modified() {
                        let mtime = modified
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        newest_mtime = Some(newest_mtime.map_or(mtime, |m| m.max(mtime)));
                    }
                }
            }
        }
    }

    match newest_mtime {
        None => Err("LLM consolidation stale — run hex memory consolidate full".to_string()),
        Some(mtime) if now_secs.saturating_sub(mtime) > fresh_window_secs => {
            Err("LLM consolidation stale — run hex memory consolidate full".to_string())
        }
        Some(_) => Ok("OK: LLM consolidation audit is fresh".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_project_with_context_only_is_not_orphan() {
        let (td, _g) = crate::telemetry::test_support::isolate();
        let proj = td.path().join("projects").join("alpha");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("context.md"), "hi").unwrap();
        let issues = orphan_project_issues(td.path());
        assert!(
            issues.is_empty(),
            "project with context.md only must not be ORPHAN: {issues:?}"
        );
    }

    #[test]
    fn orphan_project_with_neither_file_is_orphan() {
        let (td, _g) = crate::telemetry::test_support::isolate();
        let proj = td.path().join("projects").join("beta");
        fs::create_dir_all(&proj).unwrap();
        let issues = orphan_project_issues(td.path());
        assert_eq!(issues.len(), 1, "expected exactly one ORPHAN issue: {issues:?}");
        assert!(
            issues[0].contains("context.md"),
            "message must name context.md: {issues:?}"
        );
        assert!(
            !issues[0].contains("checkpoint.md"),
            "message must not name checkpoint.md: {issues:?}"
        );
    }

    #[test]
    fn orphan_archive_dir_is_skipped() {
        let (td, _g) = crate::telemetry::test_support::isolate();
        let archive = td.path().join("projects").join("_archive");
        fs::create_dir_all(&archive).unwrap();
        let issues = orphan_project_issues(td.path());
        assert!(
            issues.is_empty(),
            "_archive dir must be skipped even with no files: {issues:?}"
        );
    }

    #[test]
    fn consolidate_prose_parens_not_extracted() {
        let content = "git tag must match (enforced by release.sh) here";
        let links = extract_relative_links(content);
        assert!(
            links.is_empty(),
            "prose parens should not be extracted: {links:?}"
        );
    }

    #[test]
    fn consolidate_real_link_is_extracted() {
        let content = "See [the guide](docs/guide.md) for details.";
        let links = extract_relative_links(content);
        assert_eq!(links, vec!["docs/guide.md"]);
    }

    #[test]
    fn consolidate_https_url_is_skipped() {
        let content = "See [external](https://example.com/page.md) link.";
        let links = extract_relative_links(content);
        assert!(links.is_empty(), "https URLs should be skipped: {links:?}");
    }

    #[test]
    fn consolidate_http_url_is_skipped() {
        let content = "See [external](http://example.com/page.md) link.";
        let links = extract_relative_links(content);
        assert!(links.is_empty(), "http URLs should be skipped: {links:?}");
    }

    #[test]
    fn consolidate_anchor_only_is_skipped() {
        let content = "See [section](#heading) here.";
        let links = extract_relative_links(content);
        assert!(
            links.is_empty(),
            "anchor-only links should be skipped: {links:?}"
        );
    }
}
