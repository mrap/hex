//! Unified `hex consolidate` orchestrator.
//!
//! Single entry point that runs all consolidation layers:
//!   L1   — structural (doctor::consolidate)         — deterministic, no LLM
//!   L2   — memory DB    (memory::consolidate)       — deterministic, no LLM
//!   L2.5 — learnings promotion (learnings::run_promote) — deterministic, no LLM
//!   L3   — operating-model audit (provider::generate)   — FULL mode only
//!
//! Modes:
//!   Quick — L1 + L2 + L2.5 (deterministic; safe to run nightly).
//!   Full  — everything + L3 (writes an audit file for human review; never auto-edits sources).
//!
//! Per S6 (no quiet failures): every layer's failure is surfaced loudly to
//! stderr and reflected in the exit code. L3 failure does NOT abort L1+L2.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Quick,
    Full,
}

/// Exit code policy: workspace FINDINGS (L1 doctor lint) are reported, not
/// fatal. Only OPERATIONAL errors (L2 op failure, DB unopenable, L3 LLM
/// failure, backstop failure) fail the run. 472 consecutive cron "errors"
/// that were really lint findings taught us this (2026-06-11 assessment).
fn exit_code_for(l1_findings: i32, any_error: bool) -> i32 {
    let _ = l1_findings; // reported in summary + artifacts, never exit-fatal
    if any_error {
        1
    } else {
        0
    }
}

const LOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Full consolidation is the sole producer of the nightly audit; it WAITS for
/// the lock (a quick tick budget is ≤ ~12 min — see op_transcript_backstop —
/// so 45 min covers two stuck ticks). Quick overlaps are normal; skip fast.
fn lock_wait_budget(mode: Mode) -> std::time::Duration {
    match mode {
        Mode::Full => std::time::Duration::from_secs(45 * 60),
        _ => std::time::Duration::ZERO,
    }
}

fn acquire_lock(lock_file: &std::fs::File, budget: std::time::Duration) -> bool {
    use fs2::FileExt;
    let start = std::time::Instant::now();
    loop {
        if lock_file.try_lock_exclusive().is_ok() {
            return true;
        }
        if start.elapsed() >= budget {
            return false;
        }
        std::thread::sleep(LOCK_POLL_INTERVAL.min(budget.saturating_sub(start.elapsed())));
    }
}

/// Run the unified consolidation pass. Returns a process exit code.
///   0 — no operational errors (L1 workspace findings are reported, not fatal)
///   1 — at least one operational error (L2 op failure, DB unopenable,
///       L3 LLM failure, backstop failure)
pub fn run(mode: Mode, max: bool, hex_dir: &Path) -> i32 {
    // Self-throttle the whole process (every thread + IO) unless --max.
    crate::throttle::apply("consolidate", max);

    // Single-instance guard: a long backfill tick (15-min cron) and the
    // following tick — or 03:00 full and 03:15 quick — must not race the
    // watermark or double-pay extract calls. Reuses the same file-lock
    // pattern as `memory::index::run_index` (`memory-index.lock` at
    // index.rs:722). Quick skips cleanly if held (overlap is normal) but
    // records `skipped-lock`; Full WAITS up to its budget and alerts on
    // timeout — a lock-skipped nightly is a MISSED nightly (2026-06-10).
    let db_path = crate::memory::db_path(hex_dir);
    let lock_path = db_path.with_file_name("memory-consolidate.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let lock_file = match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "hex memory consolidate: cannot open lock {}: {e}",
                lock_path.display()
            );
            return 1;
        }
    };
    if !acquire_lock(&lock_file, lock_wait_budget(mode)) {
        let (status, code) = match mode {
            Mode::Full => ("lock-timeout", 1), // nightly MISSED — this must be loud
            _ => ("skipped-lock", 0),          // overlap is normal for quick
        };
        eprintln!(
            "hex memory consolidate ({mode:?}): lock {} held past budget — {status}",
            lock_path.display()
        );
        crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
            source: "memory::consolidate".into(),
            event: format!("consolidate::{mode:?}").to_lowercase(),
            status: status.into(),
            duration_ms: None,
            exit_code: Some(code as i64),
            detail: Some(format!("lock={}", lock_path.display())),
        });
        if matches!(mode, Mode::Full) {
            crate::alert::notify(
                "consolidate-full-lock-timeout",
                "hex nightly consolidation MISSED",
                "full consolidate could not acquire memory-consolidate.lock within 45m",
            );
        }
        return code;
    }
    let _consolidate_lock = lock_file; // released when run returns

    let mut any_error = false;
    let mut l1_findings: i32 = 0;

    println!("=== hex memory consolidate ({:?}) ===", mode);

    // Layer 1 — STRUCTURAL (deterministic)
    println!("\n-- Layer 1: structural (doctor::consolidate) --");
    let l1 = crate::doctor::consolidate::run(hex_dir);
    if l1 != 0 {
        l1_findings = l1;
        println!("Layer 1: {l1} findings (reported, non-fatal)");
    }

    // Layer 2 — MEMORY DB (deterministic)
    println!("\n-- Layer 2: memory db (memory::consolidate) --");
    let db_path = crate::memory::db_path(hex_dir);
    match crate::memory::open_db(&db_path) {
        Ok(mut conn) => {
            // Phase A transcript-delta backstop: discover raw/transcripts/*.md
            // not yet registered and run the distill watermark pipeline over the
            // delta so corrections the live agent missed get captured. Runs
            // BEFORE the standard ops so `catchup-distill` sees the new rows.
            if let Err(e) = crate::memory::consolidate::op_transcript_backstop(&mut conn, hex_dir) {
                eprintln!("Layer 2 transcript-backstop FAILED: {e}");
                any_error = true;
            }
            match crate::memory::consolidate::run(&mut conn) {
                Ok(report) => {
                    println!(
                        "Layer 2 ok={} failed={}",
                        report.ok.len(),
                        report.failed.len()
                    );
                    for (name, err) in &report.failed {
                        eprintln!("Layer 2 op '{name}' FAILED: {err}");
                    }
                    if !report.failed.is_empty() {
                        any_error = true;
                    }
                }
                Err(e) => {
                    eprintln!("Layer 2 hard-failed: {e}");
                    any_error = true;
                }
            }
        }
        Err(e) => {
            eprintln!(
                "Layer 2 hard-failed: cannot open memory.db at {}: {e}",
                db_path.display()
            );
            any_error = true;
        }
    }

    // Layer 2.5 — LEARNINGS PROMOTION (deterministic, no LLM)
    // Scans me/learnings.md + raw/reflections for recurring pattern clusters and
    // writes promotion candidates to evolution/suggestions.md. Folded in from the
    // former standalone `hex memory learnings promote` command — one less surface.
    // Idempotent (processed clusters are deduped), so it's safe in `quick`/nightly.
    println!("\n-- Layer 2.5: learnings promotion (me/learnings.md → evolution/suggestions.md) --");
    crate::learnings::run_promote(hex_dir, false);

    // Layer 3 — OPERATING-MODEL AUDIT (LLM, FULL only)
    if matches!(mode, Mode::Full) {
        println!("\n-- Layer 3: operating-model audit (provider::generate) --");
        match run_layer3(hex_dir) {
            Ok(()) => println!("Layer 3 ok"),
            Err(e) => {
                eprintln!("Layer 3 FAILED (Layers 1+2 already ran): {e}");
                any_error = true;
            }
        }
    }

    // Stamp full-run completion so doctor's nightly-full-liveness check can
    // detect missed nights (lock-timeouts, harness-down, kills-in-flight).
    if matches!(mode, Mode::Full) && !any_error {
        if let Ok(conn) = crate::memory::open_db(&crate::memory::db_path(hex_dir)) {
            if let Err(e) = conn.execute(
                "INSERT INTO metadata(key, value) VALUES('last_full_consolidated', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![chrono::Local::now().to_rfc3339()],
            ) {
                eprintln!("consolidate: failed to stamp last_full_consolidated: {e}");
            }
        }
    }

    let code = exit_code_for(l1_findings, any_error);
    println!(
        "\n=== consolidate done (exit={code}, findings={l1_findings}, errors={}) ===",
        any_error
    );
    code
}

/// Layer 3 — operating-model audit (FULL mode only).
///
/// Reads CLAUDE.md + me/learnings.md, sends them to the LLM via
/// `memory::provider::generate`, then writes the audit body to
/// `evolution/consolidation-audit-YYYY-MM-DD.md` and appends an entry to
/// `evolution/consolidation-log-YYYY-MM-DD.md`. Never edits the source
/// operating-model files (CLAUDE.md / me/learnings.md). Surfaces provider
/// failures loudly (Rule S6) but does not abort Layers 1+2 — that's enforced
/// by the caller in `run()`.
fn run_layer3(hex_dir: &Path) -> Result<()> {
    let claude_path = hex_dir.join("CLAUDE.md");
    let learnings_path = hex_dir.join("me").join("learnings.md");

    let claude = fs::read_to_string(&claude_path)
        .with_context(|| format!("read {}", claude_path.display()))?;
    let learnings = fs::read_to_string(&learnings_path)
        .with_context(|| format!("read {}", learnings_path.display()))?;

    let prompt = build_audit_prompt(&claude, &learnings);

    // Model, max_tokens, base_url, and api_key_env all resolved via llm_config.
    // HEX_CONSOLIDATE_MODEL remains supported as an alias for consolidate_audit
    // inside llm_config::resolve (back-compat).
    let body = crate::memory::provider::generate_for("consolidate_audit", &prompt)
        .map_err(|e| anyhow::anyhow!("provider::generate_for(consolidate_audit) failed: {e}"))?;

    // Silent-billing guard: an empty response is billed but worthless (task
    // Tx6nx2jhv). Turn it into a loud, nonzero failure BEFORE any artifact write.
    reject_empty_audit_body(&body)?;

    let path = write_audit_artifact(hex_dir, &body)?;
    println!("Layer 3 wrote audit: {}", path.display());
    Ok(())
}

/// Silent-billing guard (task Tx6nx2jhv). The `consolidate_audit` LLM call can
/// return empty or whitespace-only content when all output tokens are consumed
/// by hidden reasoning (observed 2026-08-16..19 in instance telemetry): the call
/// is billed (~$0.21/night) yet writes no audit, and today that failure is
/// invisible — telemetry logs "ok" and no artifact appears. Turn it into a LOUD,
/// nonzero error (Rule S6): stderr + an error telemetry event, then bail. Any
/// body with real content returns `Ok(())`.
fn reject_empty_audit_body(body: &str) -> Result<()> {
    if body.trim().is_empty() {
        eprintln!(
            "Layer 3 FAILED: consolidate_audit returned EMPTY content (0 \
             non-whitespace chars) — the call was billed but no audit was written. \
             Likely all output tokens were consumed by hidden reasoning; \
             raise consolidate_audit max_tokens (llm_config.rs)."
        );
        crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
            source: "memory::consolidate".into(),
            event: "consolidate::layer3".into(),
            status: "empty-llm-response".into(),
            duration_ms: None,
            exit_code: Some(1),
            detail: Some("consolidate_audit returned empty/whitespace-only content".into()),
        });
        anyhow::bail!("consolidate_audit LLM returned empty/whitespace-only content");
    }
    Ok(())
}

fn build_audit_prompt(claude_md: &str, learnings_md: &str) -> String {
    format!(
        "You are auditing the operating model of a long-running AI agent.\n\
         Read the two documents below and report:\n\
           - Contradictions (rules that conflict)\n\
           - Duplicate rules (semantic overlap)\n\
           - Aspirational/unenforceable rules\n\
           - Stale references (files/commands/skills that no longer exist)\n\
         Categorize each finding as one of: REMOVE | MERGE | UPDATE | REVIEW.\n\
         Output Markdown. Do NOT propose rewrites of the source files — list findings only.\n\
         \n\
         === CLAUDE.md ===\n{claude_md}\n\
         \n\
         === me/learnings.md ===\n{learnings_md}\n"
    )
}

/// Pure I/O helper: write the audit body to a dated file and append a log
/// entry. Never touches CLAUDE.md or me/learnings.md. Returns the path of
/// the audit file written.
pub(crate) fn write_audit_artifact(hex_dir: &Path, body: &str) -> Result<PathBuf> {
    use std::io::Write;

    // Defense in depth for the silent-billing guard (task Tx6nx2jhv): never
    // persist a header-only artifact for an empty/whitespace-only body. The
    // Layer 3 caller already rejects this loudly via `reject_empty_audit_body`;
    // refusing here as well means no code path can ever write an empty audit.
    if body.trim().is_empty() {
        anyhow::bail!(
            "refusing to write consolidation audit: body is empty/whitespace-only \
             (consolidate_audit returned no usable content)"
        );
    }

    let evo = hex_dir.join("evolution");
    fs::create_dir_all(&evo).with_context(|| format!("mkdir {}", evo.display()))?;

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let audit_path = evo.join(format!("consolidation-audit-{date}.md"));
    let log_path = evo.join(format!("consolidation-log-{date}.md"));

    let header = format!(
        "# Consolidation Audit — {date}\n\
         \n\
         Generated by `hex memory consolidate full`. Findings only — no source-file edits.\n\
         \n",
    );
    let mut audit = String::with_capacity(header.len() + body.len() + 1);
    audit.push_str(&header);
    audit.push_str(body);
    if !audit.ends_with('\n') {
        audit.push('\n');
    }
    fs::write(&audit_path, &audit).with_context(|| format!("write {}", audit_path.display()))?;

    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    writeln!(
        log,
        "- {date}: wrote {}",
        audit_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("consolidation-audit.md")
    )
    .with_context(|| format!("append {}", log_path.display()))?;

    Ok(audit_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_child;
    use fs2::FileExt;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn fake_hex_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fs::write(p.join("CLAUDE.md"), "").unwrap();
        let evo = p.join("evolution");
        fs::create_dir_all(&evo).unwrap();
        fs::write(evo.join("observations.md"), "").unwrap();
        fs::write(evo.join("suggestions.md"), "").unwrap();
        fs::write(evo.join("changelog.md"), "").unwrap();
        fs::create_dir_all(p.join("projects")).unwrap();
        let me = p.join("me");
        fs::create_dir_all(&me).unwrap();
        fs::write(me.join("learnings.md"), "").unwrap();
        dir
    }

    fn fake_hex_dir_at(root: &Path) {
        fs::write(root.join("CLAUDE.md"), "").unwrap();
        let evo = root.join("evolution");
        fs::create_dir_all(&evo).unwrap();
        fs::write(evo.join("observations.md"), "").unwrap();
        fs::write(evo.join("suggestions.md"), "").unwrap();
        fs::write(evo.join("changelog.md"), "").unwrap();
        fs::create_dir_all(root.join("projects")).unwrap();
        let me = root.join("me");
        fs::create_dir_all(&me).unwrap();
        fs::write(me.join("learnings.md"), "").unwrap();
    }

    fn run_quick_in_exact_child(name: &str) -> bool {
        if test_child::in_child(name) {
            test_child::stage("consolidate:child-entry").expect("test-child stage must flush");
            return false;
        }
        let home = tempfile::tempdir().expect("private consolidate HOME");
        let fixture = tempfile::tempdir().expect("private consolidate fixture");
        let output =
            test_child::run_exact(name, home.path(), fixture.path(), test_child::Fault::None)
                .unwrap_or_else(|error| panic!("private consolidate test {name} failed: {error}"));
        let stdout = String::from_utf8_lossy(&output.stdout.tail);
        assert!(
            stdout.contains("hex memory index: another run is in progress — skipping"),
            "held fixture index lock must take the busy-index branch: {stdout}"
        );
        test_child::require_one_pass(name, output).unwrap_or_else(|error| panic!("{error}"));
        true
    }

    struct HeldFixtureIndexLock {
        file: std::fs::File,
        path: PathBuf,
        #[cfg(unix)]
        device: u64,
        #[cfg(unix)]
        inode: u64,
    }

    fn hold_fixture_index_lock(root: &Path) -> HeldFixtureIndexLock {
        let db_path = crate::memory::db_path(root);
        let lock_path = db_path.with_file_name("memory-index.lock");
        fs::create_dir_all(lock_path.parent().expect("index lock parent")).unwrap();
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        held.try_lock_exclusive().unwrap();
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let held_metadata = held.metadata().unwrap();
            let contender_metadata = contender.metadata().unwrap();
            assert_eq!(
                (held_metadata.dev(), held_metadata.ino()),
                (contender_metadata.dev(), contender_metadata.ino()),
                "the contention check must use a second descriptor for the same lock inode"
            );
            let error = contender
                .try_lock_exclusive()
                .expect_err("a second descriptor must observe the held fixture index lock");
            let actual_errno = error.raw_os_error();
            assert!(
                actual_errno == Some(libc::EWOULDBLOCK) || actual_errno == Some(libc::EAGAIN),
                "second descriptor must fail with EWOULDBLOCK/EAGAIN, got {error}"
            );
            HeldFixtureIndexLock {
                file: held,
                path: lock_path,
                device: held_metadata.dev(),
                inode: held_metadata.ino(),
            }
        }
        #[cfg(not(unix))]
        unreachable!("exact fixture lock proof requires Unix")
    }

    fn assert_held_fixture_lock_is_same_inode(held: &HeldFixtureIndexLock) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let descriptor = held.file.metadata().expect("held fixture lock metadata");
            let path = fs::metadata(&held.path).expect("fixture lock path metadata");
            assert_eq!(
                (descriptor.dev(), descriptor.ino()),
                (held.device, held.inode),
                "held fixture descriptor changed during orchestration"
            );
            assert_eq!(
                (path.dev(), path.ino()),
                (held.device, held.inode),
                "fixture lock path changed during orchestration"
            );
        }
    }

    /// RED test for task T36af0tn0 (Phase A transcript-delta backstop).
    ///
    /// Behavior under test: `consolidate::run(Mode::Quick, ...)` must include a
    /// new layer that scans `raw/transcripts/*.md`, reads from each file's
    /// memory.db watermark forward (reusing `memory::distill::watermark`), runs
    /// the distill pipeline on the delta, and advances the watermark. It must
    /// tolerate not-yet-parsed transcripts / missing LLM gracefully (no crash),
    /// and a second run must be a no-op (exactly-once: no duplicated rows, no
    /// regressed watermark).
    ///
    /// Today's `op_catchup_distill` only looks at rows ALREADY in
    /// `transcript_files`; it never *discovers* a seeded `raw/transcripts/*.md`
    /// that hasn't been registered by `parse-transcripts`. The backstop must
    /// close that gap so corrections the live agent missed get captured.
    #[test]
    fn quick_transcript_backstop_registers_seeded_transcript_and_is_idempotent() {
        const NAME: &str =
            "consolidate::tests::quick_transcript_backstop_registers_seeded_transcript_and_is_idempotent";
        if run_quick_in_exact_child(NAME) {
            return;
        }
        let root = PathBuf::from(std::env::var_os("HEX_DIR").expect("private fixture HEX_DIR"));
        fake_hex_dir_at(&root);
        let index_lock = hold_fixture_index_lock(&root);
        test_child::stage("consolidate:transcript-fixture-ready")
            .expect("test-child stage must flush");
        let trans_dir = root.join("raw").join("transcripts");
        fs::create_dir_all(&trans_dir).unwrap();
        let sample = trans_dir.join("2026-06-05.md");
        fs::write(
            &sample,
            "user: please always run tests before claiming done.\nagent: noted TDD is mandatory.\n"
                .repeat(300),
        )
        .unwrap();
        let sample_str = sample.to_str().unwrap().to_string();

        // Disable LLM so extract is deferred — the backstop must still tolerate
        // this without crashing (graceful gap-tolerance per the spec).
        let code = run(Mode::Quick, true, &root);
        assert_held_fixture_lock_is_same_inode(&index_lock);
        assert!(code == 0 || code == 1, "unexpected exit code {code}");

        let db = crate::memory::db_path(&root);
        let conn = crate::memory::open_db(&db).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transcript_files WHERE path=?1",
                rusqlite::params![sample_str.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            count >= 1,
            "Phase A transcript backstop must discover and register \
             raw/transcripts/*.md in memory.db's transcript_files \
             (none was registered for {sample_str})"
        );
        drop(conn);

        // Second invocation must be a no-op — no duplicate row, no regression.
        let code2 = run(Mode::Quick, true, &root);
        assert_held_fixture_lock_is_same_inode(&index_lock);
        assert!(code2 == 0 || code2 == 1, "unexpected exit code {code2}");

        let conn = crate::memory::open_db(&db).unwrap();
        let count2: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transcript_files WHERE path=?1",
                rusqlite::params![sample_str.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, count2,
            "second backstop run must not duplicate the transcript_files row \
             (exactly-once contract)"
        );
        assert_eq!(
            crate::memory::distill::watermark::last_offset(&conn, &sample_str).unwrap(),
            0,
            "forced Deferred extraction must leave the transcript watermark at zero"
        );
        drop(conn);
        let events = crate::telemetry::recent(64).expect("read private deferred telemetry");
        let expected_detail = format!("path={sample_str} offset=0");
        assert!(
            events.iter().any(|event| {
                event.source == "memory::distill"
                    && event.event == "distill::slice"
                    && event.status == "deferred"
                    && event.detail.as_deref().is_some_and(|detail| {
                        detail.contains(&expected_detail)
                            && detail.contains("strikes=0")
                            && detail.contains("reason=provider DEFERRED: forced extract Deferred")
                            && detail.contains("HEX_DISTILL_FORCE_EXTRACT_FAIL=deferred test seam")
                    })
            }),
            "forced Deferred extraction must record the memory::distill distill::slice deferred event with fixture path and forced-seam detail"
        );
        assert!(
            !root.join(".fastembed_cache").exists(),
            "busy-index fixture must not create an embedding model cache"
        );
    }

    #[test]
    fn lock_policy_full_waits_quick_skips() {
        assert_eq!(
            lock_wait_budget(Mode::Full),
            std::time::Duration::from_secs(45 * 60)
        );
        assert_eq!(lock_wait_budget(Mode::Quick), std::time::Duration::ZERO);
    }

    #[test]
    fn l1_findings_do_not_fail_the_run() {
        // exit_code_for is the new pure aggregation fn introduced in Step 2
        assert_eq!(exit_code_for(/*l1_findings=*/ 20, /*any_error=*/ false), 0);
        assert_eq!(exit_code_for(0, true), 1);
        assert_eq!(exit_code_for(5, true), 1);
        assert_eq!(exit_code_for(0, false), 0);
    }

    #[test]
    fn quick_mode_runs_l1_and_l2_and_writes_structural_log() {
        const NAME: &str =
            "consolidate::tests::quick_mode_runs_l1_and_l2_and_writes_structural_log";
        if run_quick_in_exact_child(NAME) {
            return;
        }
        let root = PathBuf::from(std::env::var_os("HEX_DIR").expect("private fixture HEX_DIR"));
        fake_hex_dir_at(&root);
        let index_lock = hold_fixture_index_lock(&root);
        test_child::stage("consolidate:structural-fixture-ready")
            .expect("test-child stage must flush");
        let code = run(Mode::Quick, true, &root);
        assert_held_fixture_lock_is_same_inode(&index_lock);
        assert!(code == 0 || code == 1, "unexpected exit code {code}");
        assert!(
            root.join("evolution")
                .join("consolidation-latest.log")
                .exists(),
            "Layer 1 must write consolidation-latest.log"
        );
        assert!(
            !root.join(".fastembed_cache").exists(),
            "busy-index fixture must not create an embedding model cache"
        );
    }

    #[test]
    fn write_audit_artifact_creates_dated_file_and_appends_log_without_touching_sources() {
        // RED test for task Txjh0xxy9 (Layer-3 audit writer).
        // Behavior under test: a pure I/O helper that, given a fake LLM
        // audit body, writes evolution/consolidation-audit-YYYY-MM-DD.md
        // and appends to evolution/consolidation-log-YYYY-MM-DD.md, and
        // NEVER modifies CLAUDE.md or me/learnings.md.
        let dir = fake_hex_dir();
        let claude_before = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        let learn_before = fs::read_to_string(dir.path().join("me").join("learnings.md")).unwrap();

        let body = "## Audit\n- REMOVE: stale rule\n- MERGE: duplicate rule\n";
        let audit_path = super::write_audit_artifact(dir.path(), body)
            .expect("write_audit_artifact should succeed");

        // Audit file name must match consolidation-audit-YYYY-MM-DD.md
        let fname = audit_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        assert!(
            fname.starts_with("consolidation-audit-") && fname.ends_with(".md"),
            "expected dated audit file, got {fname}"
        );
        let date_part = fname
            .trim_start_matches("consolidation-audit-")
            .trim_end_matches(".md");
        assert_eq!(date_part.len(), 10, "expected YYYY-MM-DD, got {date_part}");
        let parts: Vec<&str> = date_part.split('-').collect();
        assert_eq!(parts.len(), 3, "expected YYYY-MM-DD, got {date_part}");
        assert!(parts[0].chars().all(|c| c.is_ascii_digit()));
        assert!(parts[1].chars().all(|c| c.is_ascii_digit()));
        assert!(parts[2].chars().all(|c| c.is_ascii_digit()));

        // Audit file contains the body we passed in.
        let written = fs::read_to_string(&audit_path).unwrap();
        assert!(
            written.contains("REMOVE: stale rule"),
            "audit file must include LLM body"
        );

        // Log file exists and was appended to for this date.
        let log_path = dir
            .path()
            .join("evolution")
            .join(format!("consolidation-log-{date_part}.md"));
        assert!(
            log_path.exists(),
            "consolidation-log-{date_part}.md must exist"
        );
        let log = fs::read_to_string(&log_path).unwrap();
        assert!(!log.trim().is_empty(), "log entry must be appended");

        // Source operating-model files must NOT be modified.
        let claude_after = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        let learn_after = fs::read_to_string(dir.path().join("me").join("learnings.md")).unwrap();
        assert_eq!(claude_before, claude_after, "CLAUDE.md must not be edited");
        assert_eq!(learn_before, learn_after, "learnings.md must not be edited");
    }

    #[test]
    fn write_audit_artifact_rejects_empty_or_whitespace_body() {
        // RED test for task Tx6nx2jhv (consolidate_audit silent-billing bug).
        //
        // Observed bug (2026-08-16..19): the consolidate_audit LLM call returns
        // EMPTY content when all output tokens are consumed by hidden reasoning.
        // Today write_audit_artifact happily writes a header-only file and
        // returns Ok, so the empty response is billed but never surfaces as a
        // failure. Fix requirement: an empty or whitespace-only LLM audit body
        // is a HARD error — the call site must return a nonzero result rather
        // than silently persisting an empty audit.
        //
        // This red test pins the empty-body hard error at the testable seam
        // (write_audit_artifact must refuse an empty/whitespace body and write
        // NO audit file). It does NOT pin the error-telemetry emission on the
        // Layer 3 path — see this task's write_red_tests summary: the implement
        // phase must ALSO add a `telemetry::record_loud` error event + loud
        // stderr on the empty-response path in run_layer3/run.
        let dir = fake_hex_dir();

        for body in ["", "   ", "\n\t  \n"] {
            let result = super::write_audit_artifact(dir.path(), body);
            assert!(
                result.is_err(),
                "empty/whitespace body {body:?} must be a hard error, got {result:?}"
            );
        }

        // No audit file may be written for an empty body (no silent header-only
        // artifact that hides the billing waste).
        let evo = dir.path().join("evolution");
        if evo.exists() {
            let wrote_audit = fs::read_dir(&evo).unwrap().filter_map(|e| e.ok()).any(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("consolidation-audit-"))
                    .unwrap_or(false)
            });
            assert!(
                !wrote_audit,
                "no consolidation-audit file may be written for an empty body"
            );
        }
    }

    /// Task Tx6nx2jhv: the Layer 3 empty-response guard is a HARD error AND emits
    /// an error telemetry event (Rule S6: no quiet failures). We point HEX_DIR at
    /// a temp dir so `record_loud` writes into a throwaway telemetry db rather
    /// than a developer's real one, then read the event back to prove it landed.
    #[test]
    fn reject_empty_audit_body_is_hard_error() {
        let _environment = crate::test_env::isolate_hex_dir();

        for body in ["", "   ", "\n\t  \n"] {
            assert!(
                super::reject_empty_audit_body(body).is_err(),
                "empty/whitespace body {body:?} must be a hard error"
            );
        }

        // Real content is accepted.
        assert!(
            super::reject_empty_audit_body("- REVIEW: a real finding\n").is_ok(),
            "a non-empty audit body must pass the guard"
        );

        // The empty-response path must ALSO emit an error telemetry event. HEX_DIR
        // points at the temp db, so the guard's `record_loud` wrote there — assert
        // a non-ok event for the Layer 3 audit landed.
        let events = crate::telemetry::recent(16).expect("read telemetry");
        let err_event = events
            .iter()
            .find(|e| e.event == "consolidate::layer3" && e.status == "empty-llm-response")
            .expect("empty-response path must record an error telemetry event");
        assert_ne!(
            err_event.status, "ok",
            "the empty-response telemetry event must be non-ok"
        );
        assert_eq!(
            err_event.exit_code,
            Some(1),
            "the empty-response telemetry event must carry a nonzero exit code"
        );
    }

    /// Regression: there must be exactly ONE consolidate orchestrator, and it
    /// lives at `hex memory consolidate` (`MemoryCommands::Consolidate`). The old
    /// `hex doctor consolidate` fragment and any top-level `hex consolidate`
    /// (`Commands::Consolidate`) must stay folded in. If anyone reintroduces a
    /// `DoctorCommands::Consolidate` or top-level `Commands::Consolidate` variant
    /// in main.rs, this guard fires.
    #[test]
    fn consolidate_lives_only_under_memory() {
        let main_rs = include_str!("main.rs");
        // The single canonical orchestrator must be present under memory.
        assert!(
            main_rs.contains("MemoryCommands::Consolidate"),
            "MemoryCommands::Consolidate must exist — `hex memory consolidate` is \
             the one canonical consolidate orchestrator"
        );
        // The doctor-side fragment must not return.
        assert!(
            !main_rs.contains("DoctorCommands::Consolidate"),
            "DoctorCommands::Consolidate must not be reintroduced — \
             use `hex memory consolidate` instead"
        );
        // No top-level `hex consolidate` dispatch arm — it now nests under memory.
        // (Match the qualified dispatch path, which is unambiguous, rather than the
        // bare enum-variant indentation that `MemoryCommands::Consolidate` shares.)
        assert!(
            !main_rs.contains("\n        Commands::Consolidate {"),
            "top-level Commands::Consolidate dispatch must not be reintroduced — \
             use `hex memory consolidate` instead"
        );
    }

    /// Regression: the unified `hex consolidate` surface must expose exactly
    /// `full` and `quick` modes. If either variant is renamed or removed, this
    /// guard fires before the help/CLI breaks for users.
    #[test]
    fn consolidate_exposes_full_and_quick_modes() {
        // The Mode enum is the single source of truth for the two modes.
        // Exhaustive match — adding a third mode without updating the contract
        // will fail to compile here, which is the desired tripwire.
        for m in [Mode::Quick, Mode::Full] {
            match m {
                Mode::Quick => {}
                Mode::Full => {}
            }
        }

        // And the clap dispatch in main.rs must still wire both variants.
        let main_rs = include_str!("main.rs");
        assert!(
            main_rs.contains("ConsolidateCommands::Quick"),
            "main.rs must dispatch ConsolidateCommands::Quick"
        );
        assert!(
            main_rs.contains("ConsolidateCommands::Full"),
            "main.rs must dispatch ConsolidateCommands::Full"
        );
    }

    #[test]
    fn quick_mode_does_not_write_audit_file() {
        const NAME: &str = "consolidate::tests::quick_mode_does_not_write_audit_file";
        if run_quick_in_exact_child(NAME) {
            return;
        }
        let root = PathBuf::from(std::env::var_os("HEX_DIR").expect("private fixture HEX_DIR"));
        fake_hex_dir_at(&root);
        let index_lock = hold_fixture_index_lock(&root);
        test_child::stage("consolidate:no-audit-fixture-ready")
            .expect("test-child stage must flush");
        let _ = run(Mode::Quick, true, &root);
        assert_held_fixture_lock_is_same_inode(&index_lock);
        let evo = root.join("evolution");
        for entry in fs::read_dir(&evo).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(
                !name.starts_with("consolidation-audit-"),
                "quick must not write LLM audit: found {name}"
            );
        }
        assert!(
            !root.join(".fastembed_cache").exists(),
            "busy-index fixture must not create an embedding model cache"
        );
    }
}
