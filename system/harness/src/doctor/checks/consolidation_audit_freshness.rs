//! ERROR when the newest `evolution/consolidation-audit-*.md` is older than 3
//! days — the nightly FULL consolidation (Layer 3 operating-model audit) has
//! silently stopped producing audit artifacts (OAuth expiry, the empty-content
//! billing bug, or a harness-down night). This is the doctor-visible guardrail
//! for the exact dead-feed class the hill climber depends on.
//!
//! SKIP (never ERROR) when:
//!   - the `hex-memory-maintenance` worker (which owns the nightly full run) is
//!     disabled via `hex module disable` — the operator turned the feed off; or
//!   - `evolution/` does not exist; or
//!   - `evolution/` exists but holds no `consolidation-audit-*.md` yet.
//!
//! The last two are one family: "no audits to measure." An instance can have an
//! `evolution/` dir from the Improvement Engine (observations.md / suggestions.md)
//! without ever having run a full consolidation, so an empty result must NOT
//! red out — a new doctor ERROR is an individually-fatal veto in the climber's
//! metric hierarchy, and a genuinely dead feed on a live instance still leaves
//! stale audit files on disk, which trips the ERROR branch below.
//!
//! Freshness is read from the `YYYY-MM-DD` suffix in the filename (the date the
//! audit is *for*), using `chrono::Local` to match `consolidate.rs`'s writer.

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};

/// Worker that owns the nightly full consolidation (Layer 3 audit). This is the
/// bare worker-name key `module_state::is_disabled` is called with at fire time
/// (see `worker/runtime.rs`). Disabling it stops the audit feed.
const FULL_CONSOLIDATION_WORKER: &str = "hex-memory-maintenance";

/// Audit files older than this many days trip the ERROR.
const STALE_DAYS: i64 = 3;

pub struct ConsolidationAuditFreshness;

impl DoctorCheck for ConsolidationAuditFreshness {
    fn name(&self) -> &str {
        "consolidation-audit-freshness"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        // SKIP when the operator disabled the worker that produces the audit.
        // Fail-open on an unreadable state db (matches module_state's stance):
        // a broken control surface must not silently mask a stale feed.
        if crate::module_state::disabled_set(&ctx.hex_dir)
            .map(|s| s.contains(FULL_CONSOLIDATION_WORKER))
            .unwrap_or(false)
        {
            return CheckResult::skip(format!(
                "worker '{FULL_CONSOLIDATION_WORKER}' disabled — full consolidation off, audit freshness not checked"
            ));
        }

        let evo = ctx.hex_dir.join("evolution");
        if !evo.is_dir() {
            return CheckResult::skip("evolution/ absent — no consolidation audits to check");
        }

        match newest_audit_date(&evo) {
            None => CheckResult::skip(
                "no consolidation-audit-*.md in evolution/ yet — nothing to measure",
            ),
            Some(date) => {
                let today = chrono::Local::now().date_naive();
                let age = (today - date).num_days();
                if age > STALE_DAYS {
                    CheckResult::fail(format!(
                        "newest consolidation audit is {age}d old ({date}) — full consolidation Layer 3 stopped producing audits (>{STALE_DAYS}d stale); run `hex memory consolidate full`"
                    ))
                } else {
                    CheckResult::pass(format!("newest consolidation audit {age}d old ({date})"))
                }
            }
        }
    }
}

/// Scan `evo` for `consolidation-audit-YYYY-MM-DD.md` and return the newest
/// (maximum) parseable date. Names that don't match the pattern are ignored.
fn newest_audit_date(evo: &std::path::Path) -> Option<chrono::NaiveDate> {
    let mut newest: Option<chrono::NaiveDate> = None;
    for entry in std::fs::read_dir(evo).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let date_str = name
            .strip_prefix("consolidation-audit-")
            .and_then(|s| s.strip_suffix(".md"));
        if let Some(ds) = date_str {
            if let Ok(d) = chrono::NaiveDate::parse_from_str(ds, "%Y-%m-%d") {
                if newest.map(|n| d > n).unwrap_or(true) {
                    newest = Some(d);
                }
            }
        }
    }
    newest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::check::{Context, DoctorCheck, Status};

    fn ctx_for(dir: &std::path::Path) -> Context {
        Context::new(dir.to_path_buf(), false)
    }

    fn write_audit(evo: &std::path::Path, date: &str) {
        std::fs::create_dir_all(evo).unwrap();
        std::fs::write(
            evo.join(format!("consolidation-audit-{date}.md")),
            "audit\n",
        )
        .unwrap();
    }

    #[test]
    fn fresh_audit_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let evo = tmp.path().join("evolution");
        let today = chrono::Local::now().date_naive();
        write_audit(&evo, &today.format("%Y-%m-%d").to_string());
        let res = ConsolidationAuditFreshness.run(&ctx_for(tmp.path()));
        assert_eq!(
            res.status,
            Status::Pass,
            "fresh audit must PASS, got {res:?}"
        );
    }

    #[test]
    fn stale_audit_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let evo = tmp.path().join("evolution");
        // 10 days old — well past the 3-day threshold.
        let old = chrono::Local::now().date_naive() - chrono::Duration::days(10);
        write_audit(&evo, &old.format("%Y-%m-%d").to_string());
        let res = ConsolidationAuditFreshness.run(&ctx_for(tmp.path()));
        assert_eq!(
            res.status,
            Status::Fail,
            "stale audit must FAIL, got {res:?}"
        );
    }

    #[test]
    fn absent_evolution_dir_skips() {
        let tmp = tempfile::tempdir().unwrap();
        // No evolution/ created at all.
        let res = ConsolidationAuditFreshness.run(&ctx_for(tmp.path()));
        assert_eq!(
            res.status,
            Status::Skip,
            "absent dir must SKIP, got {res:?}"
        );
    }

    #[test]
    fn empty_evolution_dir_skips() {
        // evolution/ exists (e.g. from Improvement Engine observations) but no
        // audit files yet — must SKIP, not red out (veto-safe).
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("evolution")).unwrap();
        let res = ConsolidationAuditFreshness.run(&ctx_for(tmp.path()));
        assert_eq!(res.status, Status::Skip, "empty dir must SKIP, got {res:?}");
    }

    #[test]
    fn picks_newest_of_several() {
        let tmp = tempfile::tempdir().unwrap();
        let evo = tmp.path().join("evolution");
        // A very old file plus a fresh one — newest wins → PASS.
        write_audit(&evo, "2026-01-01");
        let today = chrono::Local::now().date_naive();
        write_audit(&evo, &today.format("%Y-%m-%d").to_string());
        let res = ConsolidationAuditFreshness.run(&ctx_for(tmp.path()));
        assert_eq!(
            res.status,
            Status::Pass,
            "newest (fresh) file must win → PASS, got {res:?}"
        );
    }

    #[test]
    fn disabled_worker_skips() {
        // Disable the full-consolidation worker → SKIP even with a stale file.
        let tmp = tempfile::tempdir().unwrap();
        let evo = tmp.path().join("evolution");
        write_audit(&evo, "2026-01-01"); // stale, but should be masked by disable
        crate::module_state::set_disabled(tmp.path(), FULL_CONSOLIDATION_WORKER, true).unwrap();
        let res = ConsolidationAuditFreshness.run(&ctx_for(tmp.path()));
        assert_eq!(
            res.status,
            Status::Skip,
            "disabled worker must SKIP, got {res:?}"
        );
    }
}
