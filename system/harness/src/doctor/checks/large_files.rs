// Doctor check that flags unignored files over 256 MB sitting in HEX_DIR.
//
// Incident: $HEX_DIR/projects/system-improvement/incidents/git-cpu-codex-snapshot-2026-09-09.md
// A 3.28 GB untracked disk image sat in the hex tree. Codex snapshots and
// `git add` re-hash every unignored file each turn, so that one file made
// every Codex turn slow. This check catches the next stray large file
// before it repeats the same CPU burn.

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LIMIT_BYTES: u64 = 256 * 1024 * 1024;

pub struct LargeUnignoredFiles;

impl DoctorCheck for LargeUnignoredFiles {
    fn name(&self) -> &str {
        "large-unignored-files"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        if !is_git_repo(&ctx.hex_dir) {
            return CheckResult::skip("not a git repository; nothing to scan");
        }

        let paths = match unignored_paths(&ctx.hex_dir) {
            Ok(paths) => paths,
            Err(reason) => {
                return CheckResult::warn(format!("could not list unignored files: {reason}"));
            }
        };

        let entries: Vec<(PathBuf, u64)> = paths
            .into_iter()
            .filter_map(|rel| {
                let full = ctx.hex_dir.join(&rel);
                let size = std::fs::metadata(&full).ok()?.len();
                Some((rel, size))
            })
            .collect();

        let over_limit = files_over_limit(entries, LIMIT_BYTES);

        if over_limit.is_empty() {
            return CheckResult::pass("no unignored file over 256 MB");
        }

        let mut details = String::new();
        for (path, size) in &over_limit {
            let mb = size / (1024 * 1024);
            details.push_str(&format!("{} ({} MB)\n", path.display(), mb));
        }
        details.push_str("move it out of the tree or add it to .gitignore");

        CheckResult::fail(format!(
            "{} file(s) over 256 MB not ignored by git",
            over_limit.len()
        ))
        .with_details(details)
    }
}

/// True when `hex_dir` is a git repository, either by having a `.git` entry
/// (dir for a normal repo, file for a worktree) or by `git rev-parse`
/// succeeding there.
fn is_git_repo(hex_dir: &Path) -> bool {
    if hex_dir.join(".git").exists() {
        return true;
    }
    Command::new("git")
        .arg("rev-parse")
        .arg("--git-dir")
        .current_dir(hex_dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Enumerate every path git tracks or would add by default (tracked and
/// untracked, minus what `.gitignore` excludes) under `hex_dir`, relative to
/// it.
fn unignored_paths(hex_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(hex_dir)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let paths = output
        .stdout
        .split(|&b| b == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| PathBuf::from(String::from_utf8_lossy(chunk).into_owned()))
        .collect();

    Ok(paths)
}

/// Pure helper: keep only entries strictly over `limit`, largest first.
pub fn files_over_limit<I: IntoIterator<Item = (PathBuf, u64)>>(
    entries: I,
    limit: u64,
) -> Vec<(PathBuf, u64)> {
    let mut kept: Vec<(PathBuf, u64)> = entries
        .into_iter()
        .filter(|(_, size)| *size > limit)
        .collect();
    kept.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_over_limit_keeps_only_entries_strictly_over_the_limit() {
        let limit = 100;
        let entries = vec![
            (PathBuf::from("under.bin"), limit - 1),
            (PathBuf::from("at.bin"), limit),
            (PathBuf::from("over.bin"), limit + 1),
        ];
        let kept = files_over_limit(entries, limit);
        assert_eq!(kept, vec![(PathBuf::from("over.bin"), limit + 1)]);
    }

    #[test]
    fn files_over_limit_sorts_largest_first() {
        let limit = 100;
        let entries = vec![
            (PathBuf::from("small.bin"), limit + 1),
            (PathBuf::from("large.bin"), limit + 100),
            (PathBuf::from("medium.bin"), limit + 50),
        ];
        let kept = files_over_limit(entries, limit);
        assert_eq!(
            kept,
            vec![
                (PathBuf::from("large.bin"), limit + 100),
                (PathBuf::from("medium.bin"), limit + 50),
                (PathBuf::from("small.bin"), limit + 1),
            ]
        );
    }
}
