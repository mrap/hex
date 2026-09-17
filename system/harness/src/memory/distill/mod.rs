pub mod cap;
pub mod dedup;
pub mod extract;
pub mod judge;
pub mod watermark;

use crate::memory::predicates;
use crate::memory::provider::ProviderError;
use extract::Candidate;
use rusqlite::Connection;
use ulid::Ulid;

#[derive(Default, Debug)]
pub struct DistillReport {
    pub adds: u32,
    pub updates: u32,
    pub noops: u32,
    pub flags: u32,
}

/// Hard floor for the budget bisection. Cause-agnostic — handles output
/// truncation and content-filter rejections by shrinking the input slice.
const BUDGET_FLOOR_TOKENS: u32 = 2_000;
/// Default input cap when no llm_config override is present.
const DEFAULT_INPUT_BUDGET_TOKENS: u32 = 48_000;
/// Consecutive failures at which a slice is loudly skipped (poison-slice
/// escape hatch). The cron is the loop — the next tick takes the next slice.
const STRIKE_LIMIT: u32 = 3;

/// Cap for the appended `reason=` fragment. The whole `detail` string is meant
/// to stay under ~300 chars so events.db rows stay scannable; `path=` already
/// eats a chunk, so hold the reason to ~150 chars. The reason is TRUNCATED,
/// never dropped — a truncated cause still beats the reason-less rows that cost
/// an hour of interactive reproduction to diagnose on 2026-08-16.
const REASON_MAX_CHARS: usize = 150;

/// Emit a `distill::slice` telemetry event. Observational; never fails the
/// caller (see `telemetry::record_loud`). For non-`ok` outcomes
/// (deferred/failed/skipped) pass `Some(reason)`: the error string is appended
/// as `reason=<...>` (newline-flattened, char-truncated) so ops can diagnose a
/// bad slice from the events.db row alone. Pass `None` on the success path.
fn telemetry_slice(
    path: &str,
    start_offset: i64,
    bytes: i64,
    est_tokens: u32,
    outcome: &str,
    strikes: u32,
    reason: Option<&str>,
) {
    let mut detail = format!(
        "path={} offset={} bytes={} est_tokens={} strikes={}",
        path, start_offset, bytes, est_tokens, strikes
    );
    if let Some(reason) = reason {
        // Flatten newlines (one telemetry row = one line) and char-truncate so a
        // multibyte boundary can never panic the slice.
        let flattened = reason.replace('\n', " ");
        let clipped: String = flattened.chars().take(REASON_MAX_CHARS).collect();
        detail.push_str(&format!(" reason={}", clipped));
    }
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "memory::distill".into(),
        event: "distill::slice".into(),
        status: outcome.into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(detail),
    });
}

/// Run extract on the supplied span. The `HEX_DISTILL_FORCE_EXTRACT_FAIL`
/// env-var is a test seam: when set to a non-empty value, returns a
/// deterministic failure without any network call. The value selects the
/// `ProviderError` variant so tests can exercise BOTH Err arms:
///   * `"deferred"` (case-insensitive) -> `Deferred` (config problem: missing
///     prompt file / missing OPENROUTER_API_KEY) — must NOT strike or advance.
///   * any other non-empty value -> `Upstream` (network/API error) — the legacy
///     default, which keeps the pre-existing strike/poison tests green.
fn extract_or_forced_fail(span: &str) -> Result<Vec<Candidate>, ProviderError> {
    if let Some(v) = std::env::var("HEX_DISTILL_FORCE_EXTRACT_FAIL")
        .ok()
        .filter(|v| !v.is_empty())
    {
        if v.eq_ignore_ascii_case("deferred") {
            return Err(ProviderError::Deferred(
                "forced extract Deferred (HEX_DISTILL_FORCE_EXTRACT_FAIL=deferred test seam)"
                    .to_string(),
            ));
        }
        return Err(ProviderError::Upstream(
            "forced extract failure (HEX_DISTILL_FORCE_EXTRACT_FAIL test seam)".to_string(),
        ));
    }
    extract::extract_from_span(span)
}

pub fn run_on_file(
    conn: &mut Connection,
    path: &str,
    min_tokens: usize,
) -> anyhow::Result<DistillReport> {
    let mut report = DistillReport::default();
    let full = std::fs::read_to_string(path)?;
    let len_i64 = full.len() as i64;

    // --- Resume hardening ---
    let mut offset = watermark::last_offset(conn, path)?;
    if offset > len_i64 {
        eprintln!(
            "[distill] resume reset: stored offset {} > file length {} for {} — restarting at 0",
            offset, len_i64, path
        );
        offset = 0;
    }
    if offset > 0 && !full.is_char_boundary(offset as usize) {
        let mut o = offset as usize;
        while o > 0 && !full.is_char_boundary(o) {
            o -= 1;
        }
        eprintln!(
            "[distill] resume reset: stored offset {} not a UTF-8 boundary for {} — rounding down to {}",
            offset, path, o
        );
        offset = o as i64;
    }
    if offset >= len_i64 {
        return Ok(report);
    }
    // SAFETY(string_slice): `offset` was rounded down to a char boundary above
    // (the is_char_boundary loop) and `offset >= len_i64` returned early, so it
    // is a valid char boundary within `full`.
    #[allow(clippy::string_slice)]
    let span = &full[offset as usize..];

    // Small-delta short-circuit (semantics unchanged: a span under min_tokens
    // produces exactly zero calls). Whole-span tokens check.
    if span.split_whitespace().count() < min_tokens {
        return Ok(report);
    }

    // --- Budget resolution (strike-aware bisection) ---
    let base_budget = crate::llm_config::resolve("memory_extract")
        .ok()
        .and_then(|c| c.max_input_tokens)
        .unwrap_or(DEFAULT_INPUT_BUDGET_TOKENS);
    let strikes = watermark::strikes(conn, path)?;
    let shift = strikes.min(20);
    let budget = (base_budget >> shift).max(BUDGET_FLOOR_TOKENS);

    // --- Cap the slice ---
    let cap_len = cap::cap_span(span, budget);
    let slice_end_offset = offset + cap_len as i64;
    let est_tokens = ((cap_len as f64) / 3.5).ceil() as u32;
    if cap_len < span.len() {
        eprintln!(
            "[distill] partial slice: file={} slice_bytes={} est_tokens={} bytes_remaining={} budget_tokens={} strikes={}",
            path,
            cap_len,
            est_tokens,
            span.len() - cap_len,
            budget,
            strikes
        );
    }
    // SAFETY(string_slice): `cap::cap_span` returns a char-boundary byte length
    // (either a boundary from char_indices, an index right after an ASCII '\n',
    // `span.len()`, or 0) — never a mid-char offset.
    #[allow(clippy::string_slice)]
    let slice = &span[..cap_len];

    // --- Extract (with deterministic test seam) ---
    let candidates = match extract_or_forced_fail(slice) {
        Ok(c) => c,
        Err(e) => {
            // Variant-match the failure. `Deferred` is a CONFIG problem (missing
            // prompt file, missing OPENROUTER_API_KEY, unresolvable llm.toml) —
            // NOT poisonous content. Striking/advancing on it silently discards
            // the corpus on a box that simply has not been handed a key yet,
            // which is the exact fleet-wide data-loss bug this task fixes. So
            // Deferred waits for ops: no strike, no watermark move, retry next
            // tick. `Upstream` (network/API error) AND `Truncated` (the
            // provider's own output cap cut the extraction response off,
            // U4/KTD6) both follow the strike/halving/poison-slice escape
            // hatch: a truncated extract response is content the model could
            // not finish inside its output budget, the same "this slice
            // defeated the model" shape as a genuine upstream failure, not a
            // config problem — it belongs in the same escape hatch, not a
            // third path. Match every variant explicitly so a future new
            // variant is a compile error forcing a deliberate decision, not a
            // silent fall-through into the strike path.
            let reason = e.to_string();
            match e {
                ProviderError::Deferred(_) => {
                    eprintln!(
                        "[distill] DEFERRED (config problem — waiting for ops, NOT striking): file={} bytes={}..{} budget_tokens={} reason={}",
                        path, offset, slice_end_offset, budget, reason
                    );
                    // Record with the CURRENT strike count, unchanged — Deferred
                    // must not touch it (a box mid-strike from real Upstream
                    // errors keeps those strikes).
                    telemetry_slice(
                        path,
                        offset,
                        cap_len as i64,
                        est_tokens,
                        "deferred",
                        strikes,
                        Some(reason.as_str()),
                    );
                    return Ok(report);
                }
                // Decision (U4/KTD6): Truncated is handled exactly like
                // Upstream. A truncated extract is a slice-too-large-for-the-
                // budget failure, not a config problem — it belongs in the
                // same strike/halving/poison-slice escape hatch as any other
                // upstream failure, so a repeatedly-truncated slice still gets
                // skipped after STRIKE_LIMIT instead of retrying forever.
                ProviderError::Upstream(_) | ProviderError::Truncated(_) => {
                    let new_strikes = strikes + 1;
                    if new_strikes == STRIKE_LIMIT - 1 {
                        // One strike from a skip: page the alert path while an
                        // operator can still intervene before content is passed
                        // over (rule S6 — unattended backfills must be loud).
                        record_flag_event(
                            "distill::strike-warning",
                            "error",
                            &format!(
                                "file at {} of {} strikes: {} reason={}",
                                new_strikes, STRIKE_LIMIT, path, reason
                            ),
                        );
                    }
                    if new_strikes >= STRIKE_LIMIT {
                        // Poison-slice escape hatch: advance past the slice
                        // LOUDLY so the cron is not trapped retrying forever.
                        eprintln!(
                            "[distill] POISON SLICE SKIP: file={} bytes={}..{} strikes={} budget_tokens={} reason={}",
                            path, offset, slice_end_offset, new_strikes, budget, reason
                        );
                        telemetry_slice(
                            path,
                            offset,
                            cap_len as i64,
                            est_tokens,
                            "skipped",
                            new_strikes,
                            Some(reason.as_str()),
                        );
                        record_flag_event(
                            "distill::poison-skip",
                            "error",
                            &format!(
                                "POISON SLICE SKIP file={} bytes={}..{} reason={}",
                                path, offset, slice_end_offset, reason
                            ),
                        );
                        let tx = conn.transaction()?;
                        watermark::advance_offset(&tx, path, slice_end_offset)?;
                        watermark::set_strikes(&tx, path, 0)?;
                        tx.commit()?;
                    } else {
                        eprintln!(
                            "[distill] extract failed (strike {} of {}): file={} budget_tokens={} err={}",
                            new_strikes, STRIKE_LIMIT, path, budget, reason
                        );
                        telemetry_slice(
                            path,
                            offset,
                            cap_len as i64,
                            est_tokens,
                            "failed",
                            new_strikes,
                            Some(reason.as_str()),
                        );
                        watermark::set_strikes(conn, path, new_strikes)?;
                    }
                    return Ok(report);
                }
            }
        }
    };

    let new_offset = slice_end_offset;
    let mut tx = conn.transaction()?;
    for c in candidates {
        // Per-candidate savepoint: one bad candidate (constraint violation,
        // malformed judge output) must never roll back the whole slice's good
        // facts — that was the blast radius of the 2026-08-17 FK bug. On error
        // the savepoint drops (rolls back just this candidate), telemetry
        // records it loudly, and the loop continues.
        let sp = tx.savepoint()?;
        let res: anyhow::Result<()> = (|| {
            let (pred, obj) = if predicates::validate(&c.predicate).is_ok() {
                (c.predicate.clone(), c.object.clone())
            } else {
                (
                    "_unmapped".to_string(),
                    format!("{}: {}", c.predicate, c.object),
                )
            };

            let effective = Candidate {
                predicate: pred.clone(),
                object: obj.clone(),
                ..c.clone()
            };
            let outcome = dedup::classify(&sp, &effective, None)?;
            match outcome {
                dedup::DedupOutcome::Noop { existing_id } => {
                    report.noops += 1;
                    sp.execute(
                        "UPDATE facts SET access_count=access_count+1, last_accessed=datetime('now') WHERE id=?1",
                        [existing_id],
                    )?;
                }
                dedup::DedupOutcome::CleanAdd => {
                    insert_fact_with_history(&sp, &c.subject, &pred, &obj, c.importance, path)?;
                    report.adds += 1;
                }
                dedup::DedupOutcome::Ambiguous { nearest_ids } => {
                    let existing: Vec<(String, String, String, String)> = nearest_ids
                        .iter()
                        .filter_map(|nid| {
                            sp.query_row(
                                "SELECT id,subject,predicate,object FROM facts WHERE id=?1",
                                [nid],
                                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                            )
                            .ok()
                        })
                        .collect();
                    let decision = match judge::judge(&c.subject, &pred, &obj, "", &existing) {
                        Ok(d) => d,
                        Err(e) => {
                            // Judge ProviderError MUST NOT discard the slice's
                            // extract work, and MUST NOT write a fact_history
                            // row with no target: fact_id is NOT NULL with an
                            // enforced FK, so the old ""-keyed FLAG insert was
                            // the exact constraint violation that rolled back
                            // whole slices. Loud telemetry carries the detail.
                            eprintln!(
                                "[distill] judge failed for ({},{},{}) in {}: {} — recording FLAG and continuing",
                                c.subject, pred, obj, path, e
                            );
                            record_flag_event(
                                "distill::judge-error",
                                "error",
                                &format!(
                                    "judge-error on ({},{},{}) in {}: {}",
                                    c.subject, pred, obj, path, e
                                ),
                            );
                            report.flags += 1;
                            return Ok(());
                        }
                    };
                    apply_judge_decision(&sp, &c, &pred, &obj, decision, path, &mut report)?;
                }
            }
            Ok(())
        })();
        match res {
            Ok(()) => sp.commit()?,
            Err(e) => {
                eprintln!(
                    "[distill] candidate failed (rolled back, slice continues): ({},{},{}) in {}: {}",
                    c.subject, c.predicate, c.object, path, e
                );
                record_flag_event(
                    "distill::candidate-error",
                    "error",
                    &format!(
                        "candidate rolled back ({},{},{}) in {}: {}",
                        c.subject, c.predicate, c.object, path, e
                    ),
                );
            }
        }
    }
    watermark::advance_offset(&tx, path, new_offset)?;
    watermark::set_strikes(&tx, path, 0)?;
    tx.commit()?;
    telemetry_slice(path, offset, cap_len as i64, est_tokens, "ok", 0, None);
    Ok(report)
}

/// Insert a fact plus its ADD history row. fact_history.fact_id references the
/// fact created here, so the FK is satisfied by construction.
fn insert_fact_with_history(
    conn: &rusqlite::Connection,
    subject: &str,
    pred: &str,
    obj: &str,
    importance: f32,
    path: &str,
) -> anyhow::Result<String> {
    let id = Ulid::new().to_string();
    conn.execute(
        "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,source_ref)
         VALUES (?1,?2,?3,?4,?5,datetime('now'),datetime('now'),?6)",
        rusqlite::params![id, subject, pred, obj, importance, path],
    )?;
    conn.execute(
        "INSERT INTO fact_history (fact_id,op,new_value,ts) VALUES (?1,'ADD',?2,datetime('now'))",
        rusqlite::params![id, obj],
    )?;
    Ok(id)
}

/// Record a FLAG-class observation that has no valid facts row to attach to.
/// fact_history.fact_id is NOT NULL and FK-enforced, so target-less flags go
/// to telemetry (loud, greppable) instead of a guaranteed constraint violation.
fn record_flag_event(event: &str, status: &str, detail: &str) {
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "memory::distill".into(),
        event: event.into(),
        status: status.into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(detail.chars().take(300).collect::<String>()),
    });
}

/// Apply a judge decision for a candidate. Split out of the slice loop so the
/// FLAG/UPDATE/ADD write paths are unit-testable against an FK-enforcing
/// connection without a live judge (the target-less FLAG path is the exact
/// 2026-08-17 production failure).
fn apply_judge_decision(
    conn: &rusqlite::Connection,
    c: &Candidate,
    pred: &str,
    obj: &str,
    decision: judge::Decision,
    path: &str,
    report: &mut DistillReport,
) -> anyhow::Result<()> {
    match decision.action {
        judge::Action::Add => {
            insert_fact_with_history(conn, &c.subject, pred, obj, c.importance, path)?;
            report.adds += 1;
        }
        judge::Action::Update => {
            if let Some(tid) = decision.target_id {
                let prev: String =
                    conn.query_row("SELECT object FROM facts WHERE id=?1", [&tid], |r| r.get(0))?;
                conn.execute(
                    "UPDATE facts SET object=?1, updated_at=datetime('now') WHERE id=?2",
                    rusqlite::params![obj, tid],
                )?;
                conn.execute(
                    "INSERT INTO fact_history (fact_id,op,prev_value,new_value,ts) VALUES (?1,'UPDATE',?2,?3,datetime('now'))",
                    rusqlite::params![tid, prev, obj],
                )?;
                report.updates += 1;
            } else {
                // UPDATE with no target is a judge protocol violation — do not
                // silently drop it (rule S6). Record and count as a flag.
                record_flag_event(
                    "distill::flag-untargeted",
                    "ok",
                    &format!(
                        "judge said UPDATE without target_id for ({},{},{}) in {}: {}",
                        c.subject, pred, obj, path, decision.reason
                    ),
                );
                report.flags += 1;
            }
        }
        judge::Action::Noop => {
            report.noops += 1;
        }
        judge::Action::Flag => match decision.target_id {
            Some(tid) if !tid.is_empty() => {
                conn.execute(
                    "INSERT INTO fact_history (fact_id,op,new_value,ts) VALUES (?1,'FLAG',?2,datetime('now'))",
                    rusqlite::params![
                        tid,
                        format!(
                            "contested by: ({},{},{}) — {}",
                            c.subject, pred, obj, decision.reason
                        )
                    ],
                )?;
                report.flags += 1;
            }
            _ => {
                record_flag_event(
                    "distill::flag-untargeted",
                    "ok",
                    &format!(
                        "contested, no target: ({},{},{}) in {} — {}",
                        c.subject, pred, obj, path, decision.reason
                    ),
                );
                report.flags += 1;
            }
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fixture_conn() -> Connection {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        conn
    }

    /// FK regression (2026-08-17/18 production failure): a judge FLAG with no
    /// target_id used to insert fact_history.fact_id='' — a guaranteed FOREIGN
    /// KEY violation with FKs enforced, which rolled back the entire slice
    /// transaction and trapped the file in a retry-spend loop. It must now
    /// succeed, write NO fact_history row, and record loud telemetry instead.
    #[test]
    fn untargeted_flag_does_not_violate_fk_or_error() {
        let (_td, _g) = crate::telemetry::test_support::isolate();
        let conn = fixture_conn();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        let mut report = DistillReport::default();
        let c = Candidate {
            subject: "Mike".into(),
            predicate: "prefers".into(),
            object: "tea".into(),
            importance: 0.5,
        };
        let d = judge::Decision {
            action: judge::Action::Flag,
            target_id: None,
            reason: "ambiguous vs existing".into(),
        };
        apply_judge_decision(&conn, &c, "prefers", "tea", d, "/tmp/t.md", &mut report)
            .expect("target-less FLAG must not error (this was the FK bug)");
        assert_eq!(report.flags, 1);
        let hist: i64 = conn
            .query_row("SELECT COUNT(*) FROM fact_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            hist, 0,
            "no fact_history row may be written without a valid target"
        );
        let rows = crate::telemetry::recent(10).unwrap();
        assert!(
            rows.iter().any(|r| r.event == "distill::flag-untargeted"),
            "target-less FLAG must be recorded in telemetry"
        );
    }

    /// The legitimate FLAG path (valid target) must keep writing fact_history.
    #[test]
    fn targeted_flag_still_writes_history_row() {
        let (_td, _g) = crate::telemetry::test_support::isolate();
        let conn = fixture_conn();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        let mut report = DistillReport::default();
        let id =
            insert_fact_with_history(&conn, "Mike", "prefers", "coffee", 0.5, "/tmp/t.md").unwrap();
        let c = Candidate {
            subject: "Mike".into(),
            predicate: "prefers".into(),
            object: "tea".into(),
            importance: 0.5,
        };
        let d = judge::Decision {
            action: judge::Action::Flag,
            target_id: Some(id.clone()),
            reason: "contradicts".into(),
        };
        apply_judge_decision(&conn, &c, "prefers", "tea", d, "/tmp/t.md", &mut report).unwrap();
        let flags: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fact_history WHERE op='FLAG' AND fact_id=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(flags, 1, "a targeted FLAG keeps its fact_history row");
    }

    /// Red test for Te4qev9ed: strike escalation + 3-strike loud skip.
    ///
    /// When the extract step deterministically fails for a slice, run_on_file
    /// must:
    ///   1. Not advance the watermark on the first two failures (strikes 1, 2)
    ///      — leave room for the next cron tick to retry with halved budget.
    ///   2. After the third consecutive failure (at the budget floor), LOUDLY
    ///      skip the slice by advancing the watermark past it, so a poisoned
    ///      slice cannot wedge the file forever.
    ///
    /// The test uses a process-local env-var seam (`HEX_DISTILL_FORCE_EXTRACT_FAIL`)
    /// that the implementation must honor to make extract deterministically
    /// fail without any network. Cap.rs already exists; the test does NOT
    /// require a live LLM.
    #[test]
    fn deterministically_failing_slice_skipped_after_three_strikes() {
        let _guard = crate::telemetry::test_support::lock_env();

        // Isolated HEX_DIR with a stub extract prompt so extract gets past the
        // template read. No OPENROUTER_API_KEY set → live provider would
        // return Deferred, but the impl-under-test must instead consult the
        // forced-fail seam below.
        let td = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(td.path().join(".hex/memory/prompts")).unwrap();
        std::fs::write(
            td.path().join(".hex/memory/prompts/extract.txt"),
            "extract predicates: [[PREDICATES]]\n",
        )
        .unwrap();
        std::env::set_var("HEX_DIR", td.path());
        std::env::remove_var("OPENROUTER_API_KEY");
        std::env::set_var("HEX_DISTILL_FORCE_EXTRACT_FAIL", "1");

        // A transcript file with enough content to exceed min_tokens and to be
        // sliced. Content is intentionally non-empty so the cap+slice path is
        // exercised (rather than the small-delta short-circuit).
        let mut transcript = String::new();
        for _ in 0..200 {
            transcript
                .push_str("This is a line of transcript content with several tokens in it.\n");
        }
        let file = td.path().join("transcript.md");
        std::fs::write(&file, &transcript).unwrap();
        let path = file.to_str().unwrap();

        let mut conn = fixture_conn();
        let min_tokens = 5usize;

        // Strikes 1 & 2: watermark must remain at 0 (retry next tick).
        let _ = run_on_file(&mut conn, path, min_tokens);
        let offset_after_1 = watermark::last_offset(&conn, path).unwrap();
        assert_eq!(
            offset_after_1, 0,
            "strike 1: watermark must not advance on extract failure"
        );

        let _ = run_on_file(&mut conn, path, min_tokens);
        let offset_after_2 = watermark::last_offset(&conn, path).unwrap();
        assert_eq!(
            offset_after_2, 0,
            "strike 2: watermark must not advance on extract failure"
        );

        // Strike 3: at floor → must LOUDLY SKIP the slice by advancing the
        // watermark past it (poison-slice escape hatch).
        let _ = run_on_file(&mut conn, path, min_tokens);
        let offset_after_3 = watermark::last_offset(&conn, path).unwrap();
        assert!(
            offset_after_3 > 0,
            "strike 3: watermark MUST advance past the poisoned slice \
             (got {}) — otherwise the cron retries forever",
            offset_after_3
        );

        // Upstream telemetry contract (mirror of the Deferred test, so BOTH
        // variants are covered): the strike-3 slice is recorded with outcome
        // "skipped", and the detail must carry the error string as reason=<...>
        // so ops can diagnose the cause from the events.db row alone.
        let rows = crate::telemetry::recent(50).unwrap();
        let slice_row = rows
            .iter()
            .find(|r| r.event == "distill::slice")
            .expect("a distill::slice telemetry row must be recorded");
        assert_eq!(
            slice_row.status, "skipped",
            "strike 3: Upstream poison slice must record telemetry outcome \"skipped\" (got {:?})",
            slice_row.status
        );
        let detail = slice_row.detail.as_deref().unwrap_or("");
        assert!(
            detail.contains("reason="),
            "strike 3: telemetry detail must carry reason=<error> for a skipped slice (got {:?})",
            slice_row.detail
        );

        // Alerting contract (2026-08-18): strike 2 pages the alert path while
        // an operator can still intervene, and the poison-skip itself is
        // recorded as a status="error" event, not only the "skipped" slice row.
        assert!(
            rows.iter()
                .any(|r| r.event == "distill::strike-warning" && r.status == "error"),
            "strike 2 must record a loud strike-warning event"
        );
        assert!(
            rows.iter()
                .any(|r| r.event == "distill::poison-skip" && r.status == "error"),
            "the poison skip must record a loud error event"
        );

        std::env::remove_var("HEX_DISTILL_FORCE_EXTRACT_FAIL");
        std::env::remove_var("HEX_DIR");
    }

    /// Red test for Tsp1zwwfk: a `Deferred` extract failure (config problem —
    /// e.g. missing prompt file or missing OPENROUTER_API_KEY) must NOT be
    /// treated like a poisonous slice.
    ///
    /// run_on_file's extract Err arm must variant-match: `Deferred` waits for
    /// ops instead of eating the corpus — it does NOT increment strikes and
    /// does NOT advance the watermark, no matter how many ticks fire. Only a
    /// genuine `Upstream` failure follows the strike/poison escape hatch (that
    /// path is covered by `deterministically_failing_slice_skipped_after_three_strikes`).
    ///
    /// This test drives the force-fail seam with the value "deferred", which
    /// the implementation must map to `ProviderError::Deferred`. Legacy /
    /// any-other values remain `Upstream` so the sibling strike test stays green.
    #[test]
    fn deferred_extract_failure_leaves_strikes_and_watermark_untouched() {
        let _guard = crate::telemetry::test_support::lock_env();

        // Isolated HEX_DIR, no OPENROUTER_API_KEY — mirrors a real box that has
        // not yet been handed a key. The forced-fail seam short-circuits before
        // any provider/template read, so no prompt file is needed here.
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("HEX_DIR", td.path());
        std::env::remove_var("OPENROUTER_API_KEY");
        std::env::set_var("HEX_DISTILL_FORCE_EXTRACT_FAIL", "deferred");

        let mut transcript = String::new();
        for _ in 0..200 {
            transcript
                .push_str("This is a line of transcript content with several tokens in it.\n");
        }
        let file = td.path().join("transcript.md");
        std::fs::write(&file, &transcript).unwrap();
        let path = file.to_str().unwrap();

        let mut conn = fixture_conn();
        let min_tokens = 5usize;

        // Three ticks in a row. A Deferred failure must leave BOTH the strike
        // counter and the watermark at zero every single time — otherwise a
        // box awaiting an API key silently discards transcript slices forever.
        for tick in 1..=3 {
            let _ = run_on_file(&mut conn, path, min_tokens);
            let offset = watermark::last_offset(&conn, path).unwrap();
            assert_eq!(
                offset, 0,
                "tick {}: Deferred must NOT advance the watermark (got {})",
                tick, offset
            );
            let strikes = watermark::strikes(&conn, path).unwrap();
            assert_eq!(
                strikes, 0,
                "tick {}: Deferred must NOT increment strikes (got {})",
                tick, strikes
            );

            // Telemetry contract: a Deferred slice is recorded with the distinct
            // outcome "deferred" (NOT "failed"/"skipped"), and the detail must
            // carry the error string as reason=<...> so ops can diagnose the
            // config cause from the events.db row alone.
            let rows = crate::telemetry::recent(50).unwrap();
            let slice_row = rows
                .iter()
                .find(|r| r.event == "distill::slice")
                .expect("a distill::slice telemetry row must be recorded per tick");
            assert_eq!(
                slice_row.status, "deferred",
                "tick {}: Deferred slice must record telemetry outcome \"deferred\" (got {:?})",
                tick, slice_row.status
            );
            let detail = slice_row.detail.as_deref().unwrap_or("");
            assert!(
                detail.contains("reason="),
                "tick {}: telemetry detail must carry reason=<error> for a deferred slice (got {:?})",
                tick, slice_row.detail
            );
        }

        std::env::remove_var("HEX_DISTILL_FORCE_EXTRACT_FAIL");
        std::env::remove_var("HEX_DIR");
    }

    /// The pipeline's first golden-path E2E test: a fake `claude` binary on a
    /// prepended PATH (the claude_cli.rs shim pattern) plays the LLM, llm.toml
    /// routes memory_extract through the claude-cli transport, and
    /// `run_on_file` drives extract -> dedup -> write against a real seeded
    /// transcript. Two candidates with distinct subjects keep every dedup
    /// outcome CleanAdd, so no judge call happens and the single shim response
    /// is deterministic.
    #[test]
    fn e2e_golden_path_extract_writes_facts_via_claude_cli_shim() {
        let (hex_tmp, _g) = crate::telemetry::test_support::isolate();
        let hex_dir = hex_tmp.path();

        // Route memory_extract through the claude-cli transport (no http key).
        std::fs::create_dir_all(hex_dir.join(".hex/config")).unwrap();
        std::fs::write(
            hex_dir.join(".hex/config/llm.toml"),
            "[use_cases.memory_extract]\ntransport = \"claude-cli\"\n\n[use_cases.memory_judge]\ntransport = \"claude-cli\"\n",
        )
        .unwrap();

        // Fake `claude`: drains stdin (the prompt arrives there), then emits a
        // valid --output-format json envelope whose result is the candidates
        // JSON. Distinct subjects => both classify as CleanAdd.
        let candidates = r#"[{"subject":"Ada Lovelace","predicate":"prefers","object":"tables over prose in status reports","importance":0.8},{"subject":"Project Analytical Engine","predicate":"prefers","object":"weekly written updates over meetings","importance":0.5}]"#;
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": candidates,
        })
        .to_string();
        let shim_dir = hex_dir.join("shim");
        std::fs::create_dir_all(&shim_dir).unwrap();
        let script = format!("#!/bin/sh\ncat > /dev/null\nprintf '%s' '{}'\n", envelope);
        let shim = shim_dir.join("claude");
        std::fs::write(&shim, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let old_path = std::env::var("PATH").ok();
        std::env::set_var(
            "PATH",
            match &old_path {
                Some(p) => format!("{}:{}", shim_dir.display(), p),
                None => shim_dir.display().to_string(),
            },
        );

        // A real transcript file under the isolated HEX_DIR.
        std::fs::create_dir_all(hex_dir.join("raw/transcripts")).unwrap();
        let transcript = hex_dir.join("raw/transcripts/2026-08-17.md");
        std::fs::write(
            &transcript,
            "# session\n\nAda Lovelace said she prefers tables over prose in status \
             reports. The Analytical Engine project runs on weekly written updates.\n",
        )
        .unwrap();
        let path_str = transcript.to_string_lossy().to_string();
        let file_len = std::fs::metadata(&transcript).unwrap().len() as i64;

        let mut conn = fixture_conn();
        let report = run_on_file(&mut conn, &path_str, 1).expect("golden path must succeed");

        // Restore PATH before asserting so a panic can't leak the shim.
        match old_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }

        assert_eq!(
            report.adds, 2,
            "both CleanAdd candidates must land: {report:?}"
        );
        let fact_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts WHERE source_ref = ?1 AND tombstone = 0",
                [&path_str],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            fact_count, 2,
            "facts rows must carry the transcript as source_ref"
        );
        let add_history: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fact_history WHERE op = 'ADD'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(add_history, 2, "each ADD must be recorded in fact_history");
        assert_eq!(
            watermark::last_offset(&conn, &path_str).unwrap(),
            file_len,
            "watermark must advance to the end of the processed slice"
        );
        assert_eq!(
            watermark::strikes(&conn, &path_str).unwrap(),
            0,
            "a clean run must leave zero strikes"
        );
    }
}
