use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;

/// check_5: .agents/skills is a symlink pointing to .hex/skills.
pub struct AgentsSkillsSymlink;

impl DoctorCheck for AgentsSkillsSymlink {
    fn name(&self) -> &str {
        "agents-skills-symlink"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let agents_skills = ctx.hex_dir.join(".agents/skills");
        let target = ctx.hex_dir.join(".hex/skills");

        if agents_skills.is_symlink() {
            // Check it resolves
            if agents_skills.exists() {
                return CheckResult::pass(".agents/skills symlinked correctly");
            } else {
                if ctx.fix {
                    let _ = fs::remove_file(&agents_skills);
                    if std::os::unix::fs::symlink(&target, &agents_skills).is_ok() {
                        return CheckResult::fixed(".agents/skills symlink repaired");
                    }
                }
                return CheckResult::warn(".agents/skills symlink is broken");
            }
        }

        if agents_skills.exists() {
            return CheckResult::warn(".agents/skills exists but is not a symlink");
        }

        // Not present at all
        if ctx.fix {
            if let Some(parent) = agents_skills.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if std::os::unix::fs::symlink(&target, &agents_skills).is_ok() {
                return CheckResult::fixed(".agents/skills symlink created");
            }
        }
        CheckResult::fail(".agents/skills symlink missing — run bootstrap to fix")
    }
}

/// check_12: No broken symlinks under .hex/ or .agents/.
pub struct NoBrokenSymlinks;

impl DoctorCheck for NoBrokenSymlinks {
    fn name(&self) -> &str {
        "no-broken-symlinks"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let dirs = [ctx.hex_dir.join(".hex"), ctx.hex_dir.join(".agents")];
        let mut all_broken: Vec<String> = Vec::new();

        for dir in &dirs {
            if !dir.is_dir() {
                continue;
            }
            collect_broken_symlinks(dir, &mut all_broken);
        }

        // Upgrade-residue directories (`.upgrade-cache.corrupt-*`, left aside by
        // clear_cache_dir() when it can't fully delete a corrupt cache) are
        // allowlisted: broken links inside them are expected and ignored, not
        // reported as a health failure.
        let (ignored, broken): (Vec<String>, Vec<String>) = all_broken
            .into_iter()
            .partition(|p| is_under_upgrade_residue(std::path::Path::new(p)));
        let ignored_count = ignored.len();
        let suffix = if ignored_count > 0 {
            format!(" ({ignored_count} under upgrade residue ignored)")
        } else {
            String::new()
        };

        if broken.is_empty() {
            return CheckResult::pass(format!("no broken symlinks found{suffix}"));
        }

        let count = broken.len();
        if ctx.fix {
            let mut removed = 0usize;
            for path in &broken {
                if fs::remove_file(path).is_ok() {
                    removed += 1;
                }
            }
            if removed == count {
                return CheckResult::fixed(format!("Removed {count} broken symlink(s){suffix}"));
            }
            return CheckResult::warn(format!(
                "Removed {removed}/{count} broken symlink(s){suffix}"
            ));
        }

        CheckResult::fail(format!("{count} broken symlink(s) found{suffix}"))
            .with_details(broken.join("\n"))
    }
}

/// True if any path component starts with `.upgrade-cache.corrupt-` — the
/// naming `clear_cache_dir()` uses for a cache it moved aside instead of
/// deleting.
fn is_under_upgrade_residue(path: &std::path::Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| s.starts_with(".upgrade-cache.corrupt-"))
            .unwrap_or(false)
    })
}

fn collect_broken_symlinks(dir: &std::path::Path, out: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() && !path.exists() {
            out.push(path.display().to_string());
        } else if path.is_dir() && !path.is_symlink() {
            collect_broken_symlinks(&path, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::check::Status;
    use std::path::PathBuf;

    fn ctx_for(tmp: &tempfile::TempDir) -> Context {
        Context {
            hex_dir: tmp.path().to_path_buf(),
            home: PathBuf::from("/tmp"),
            fix: false,
        }
    }

    /// A broken symlink under `.upgrade-cache.corrupt-*` residue is ignored:
    /// the check passes and the summary names how many were ignored.
    #[test]
    fn broken_symlink_under_upgrade_residue_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let residue_dir = tmp.path().join(".hex/.upgrade-cache.corrupt-0");
        fs::create_dir_all(&residue_dir).unwrap();
        let link = residue_dir.join("CLAUDE.md");
        std::os::unix::fs::symlink(residue_dir.join("does-not-exist"), &link).unwrap();

        let ctx = ctx_for(&tmp);
        let result = NoBrokenSymlinks.run(&ctx);

        assert_eq!(result.status, Status::Pass, "message was: {}", result.message);
        assert!(
            result.message.contains("1 under upgrade residue ignored"),
            "message was: {}",
            result.message
        );
    }

    /// A broken symlink outside any upgrade-residue directory still fails
    /// the check, unaffected by the allowlist.
    #[test]
    fn broken_symlink_elsewhere_still_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".hex/skills");
        fs::create_dir_all(&dir).unwrap();
        let link = dir.join("broken");
        std::os::unix::fs::symlink(dir.join("does-not-exist"), &link).unwrap();

        let ctx = ctx_for(&tmp);
        let result = NoBrokenSymlinks.run(&ctx);

        assert_eq!(result.status, Status::Fail, "message was: {}", result.message);
        assert!(
            result.message.contains("1 broken symlink"),
            "message was: {}",
            result.message
        );
        assert!(
            !result.message.contains("upgrade residue"),
            "message was: {}",
            result.message
        );
    }

    /// A real broken link and a residue-ignored one at the same time: the
    /// check still fails on the real one, and the summary still names the
    /// ignored count. Guards against a partition that swallows the real
    /// failure instead of only filtering out the allowlisted one.
    #[test]
    fn real_failure_and_ignored_residue_both_reported() {
        let tmp = tempfile::tempdir().unwrap();

        let residue_dir = tmp.path().join(".hex/.upgrade-cache.corrupt-0");
        fs::create_dir_all(&residue_dir).unwrap();
        std::os::unix::fs::symlink(
            residue_dir.join("does-not-exist"),
            residue_dir.join("CLAUDE.md"),
        )
        .unwrap();

        let dir = tmp.path().join(".hex/skills");
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(dir.join("does-not-exist"), dir.join("broken")).unwrap();

        let ctx = ctx_for(&tmp);
        let result = NoBrokenSymlinks.run(&ctx);

        assert_eq!(result.status, Status::Fail, "message was: {}", result.message);
        assert!(
            result.message.contains("1 broken symlink"),
            "message was: {}",
            result.message
        );
        assert!(
            result.message.contains("1 under upgrade residue ignored"),
            "message was: {}",
            result.message
        );
    }
}
