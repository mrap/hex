use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

#[derive(Default, serde::Serialize)]
pub struct ConsolidateReport {
    pub ok: Vec<String>,
    pub failed: Vec<(String, String)>,
}

pub fn run(conn: &mut Connection) -> anyhow::Result<ConsolidateReport> {
    let mut r = ConsolidateReport::default();

    macro_rules! iso {
        ($name:expr, $expr:expr) => {
            match $expr {
                Ok(()) => r.ok.push($name.to_string()),
                Err(e) => {
                    eprintln!("consolidate op '{}' FAILED: {e}", $name);
                    r.failed.push(($name.to_string(), e.to_string()));
                }
            }
        };
    }

    iso!("orientation-snapshot", op_orientation_snapshot(conn));
    iso!("catchup-distill", op_catchup_distill(conn));
    iso!("fact-canonicalize", op_fact_canonicalize(conn));
    iso!("dedup", op_dedup(conn));
    iso!("contradiction-sweep", op_contradiction_sweep(conn));
    // PAUSED (Mike, 2026-06-11 — me/decisions/fact-prune-paused-until-access-counter):
    // prune tombstones on access_count=0 + age>60d, but NOTHING increments
    // access_count yet, so expiry was effectively universal for non-exempt
    // facts regardless of how often recall served them. Re-enable ONLY after
    // recall/search bump access_count/last_accessed on facts they serve
    // (FIX-013 follow-up). Deliberately not deleted: the re-enable is one line.
    // iso!("prune",             op_prune(conn));
    iso!("topic-rollup", op_topic_rollup(conn));

    // Record when consolidation last ran so `hex memory stats` can report it.
    // This is advisory bookkeeping: log loudly on failure (Rule S6) but do NOT
    // fail the run over a metadata hiccup.
    match stamp_last_consolidated(conn) {
        Ok(()) => r.ok.push("stamp-last-consolidated".to_string()),
        Err(e) => eprintln!("consolidate: WARN could not stamp last_consolidated: {e}"),
    }

    Ok(r)
}

/// Stamp the wall-clock time of this consolidation run into the `metadata`
/// key-value table under `last_consolidated`. `hex memory stats` reads this key.
/// Idempotent; creates the metadata table if a bare DB lacks it.
fn stamp_last_consolidated(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES ('last_consolidated', ?)",
        rusqlite::params![chrono::Local::now().to_rfc3339()],
    )?;
    Ok(())
}

fn op_orientation_snapshot(_conn: &mut Connection) -> anyhow::Result<()> {
    // Refresh standing snapshot: active project, open threads, recent-session arc.
    // FIRST so any later failure does not starve retrieval.
    Ok(())
}

fn op_catchup_distill(conn: &mut Connection) -> anyhow::Result<()> {
    let paths: Vec<String> = conn
        .prepare(
            "SELECT path FROM transcript_files WHERE last_distilled_at IS NULL
             OR datetime(last_distilled_at) < datetime('now','-1 day')",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .collect();
    for p in paths {
        let _ = crate::memory::distill::run_on_file(conn, &p, 500);
    }
    Ok(())
}

fn op_dedup(_conn: &mut Connection) -> anyhow::Result<()> {
    // Not yet implemented: vector-cluster near-duplicate facts, feed to LLM judge for merge.
    eprintln!("consolidate op 'dedup': not yet implemented");
    Ok(())
}

// ---------------------------------------------------------------------------
// Conservative fact canonicalization (recall-fix task Tsfwg7d2v; diagnosis root
// cause 2: fact-store duplication and subject fragmentation).
//
// Case-variant and separator-variant subject spellings are folded onto ONE
// canonical grouping key, and within each (canonical-subject, predicate) group
// high-confidence near-duplicate facts collapse down to the single best fact
// (newest, most-complete wording) by TOMBSTONING the rest. Never DELETEs.
// ---------------------------------------------------------------------------

/// Overlap-coefficient threshold for treating two objects as near-duplicates
/// (|A ∩ B| / min(|A|, |B|) over token SETS). High and deliberately
/// conservative: only near-identical or strict-superset wordings of the SAME
/// claim clear it. See `objects_are_near_duplicates`.
const OBJECT_SIM_THRESHOLD: f64 = 0.8;

/// One live fact row loaded by the canonicalization pass.
struct CanonFact {
    id: String,
    subject: String,
    predicate: String,
    object: String,
    updated_at: String,
}

/// Canonical GROUPING key for a subject: lowercased, with runs of separator
/// characters (space, tab, underscore, hyphen) folded to a single hyphen and
/// leading/trailing separators trimmed. Pure case + separator variants collapse
/// onto one key (`Fleet Coordinator` == `fleet-coordinator`); a spelling that
/// adds or drops a whole token (`hex-fleet-coordinator`) stays DISTINCT — the
/// conservative choice, because an extra token cannot be told apart from a
/// genuinely more-specific entity with high confidence (spec STOP CONDITION on
/// classes that cannot be distinguished).
///
/// This is a GROUPING key ONLY. The stored `subject` column is NEVER rewritten:
/// lowercasing live subjects would destroy display/eval spellings (e.g. `Mike`
/// -> `mike`) and break subject-exact queries the tests pin. Collapsing across
/// spellings still consolidates the duplicates onto a single surviving row.
fn canonical_subject_key(subject: &str) -> String {
    let mut out = String::with_capacity(subject.len());
    let mut pending_sep = false;
    for ch in subject.trim().chars() {
        if ch == ' ' || ch == '\t' || ch == '_' || ch == '-' {
            pending_sep = true;
        } else {
            if pending_sep && !out.is_empty() {
                out.push('-');
            }
            pending_sep = false;
            out.extend(ch.to_lowercase());
        }
    }
    out
}

/// Tokenize an object into a set of lowercased alphanumeric tokens for
/// similarity scoring. Punctuation and whitespace are separators; empty tokens
/// are dropped.
fn object_token_set(object: &str) -> std::collections::BTreeSet<String> {
    object
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// TRUE iff `t` is an explicit negation/polarity token. When such a token is the
/// DIFFERENCE between two otherwise-overlapping objects, the objects may state
/// OPPOSITE claims, so they must not be collapsed. Deliberately limited to
/// unambiguous, standalone negations (plus common no-apostrophe contraction
/// spellings, since `object_token_set` splits on the apostrophe). Erring toward
/// keeping both facts is the conservative direction the spec demands.
fn is_polarity_token(t: &str) -> bool {
    matches!(
        t,
        "not" | "no"
            | "never"
            | "none"
            | "cannot"
            | "cant"
            | "dont"
            | "doesnt"
            | "didnt"
            | "wont"
            | "wouldnt"
            | "shouldnt"
            | "isnt"
            | "arent"
            | "wasnt"
            | "werent"
            | "without"
            | "neither"
            | "nor"
            | "nothing"
    )
}

/// TRUE iff two objects are high-confidence near-duplicates of the SAME claim.
///
/// Uses the overlap coefficient |A ∩ B| / min(|A|, |B|) over token sets. This is
/// superset-tolerant on purpose: a strict-superset wording — the same decision
/// re-stated more completely — scores 1.0 against its shorter form, which is
/// exactly the repeated-decision class the diagnosis calls out. It additionally
/// requires at least 2 shared substantive tokens, so a single common word can
/// never trigger a collapse. Conservative by construction: genuinely distinct
/// claims sharing a subject+predicate have little token overlap and never clear
/// the bar (pinned by `canonical_keeps_distinct_facts_sharing_subject_and_predicate`).
///
/// POLARITY GUARD: the raw overlap coefficient scores a strict superset 1.0 even
/// when the extra tokens flip the claim's meaning ("use X" vs "do not use X"),
/// which would wrongly tombstone a CONTRADICTING fact. So if the tokens that
/// DIFFER between the two objects (their symmetric difference) include an
/// explicit negation, the pair is never a duplicate — a token-overlap metric
/// cannot distinguish a claim from its negation with high confidence, and the
/// spec HARD CONSTRAINT / `conservative-collapse` verification require keeping
/// both. Pinned by `canonical_keeps_polarity_flipped_near_superset_facts`.
fn objects_are_near_duplicates(a: &str, b: &str) -> bool {
    let ta = object_token_set(a);
    let tb = object_token_set(b);
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    // Polarity guard (conservative). A negation present in one object but not the
    // other flips the claim; never collapse across it.
    if ta
        .symmetric_difference(&tb)
        .any(|t| is_polarity_token(t.as_str()))
    {
        return false;
    }
    let shared = ta.intersection(&tb).count();
    let min_len = ta.len().min(tb.len());
    let overlap = shared as f64 / min_len as f64;
    overlap >= OBJECT_SIM_THRESHOLD && shared >= 2
}

/// Conservative fact-canonicalization pass (diagnosis root cause 2). Folds
/// case-variant and separator-variant subject spellings onto one canonical
/// grouping key, then within each (canonical-subject, predicate) group collapses
/// HIGH-CONFIDENCE near-duplicate facts (same claim, near-identical or
/// strict-superset object wording) down to the single best fact — the newest,
/// most-complete wording — by TOMBSTONING the rest. Never DELETEs a row. Every
/// collapse is logged loudly (stderr + telemetry + a `fact_history` row) with
/// both fact ids AND both objects (Rule S6 / spec constraint).
///
/// Conservatism guarantees:
/// - Subjects that differ by more than case + separator (an added/removed token)
///   stay in SEPARATE groups and never merge.
/// - Within a group, a fact is absorbed only if it is directly near-duplicate
///   with the SURVIVING leader (best-first leader clustering — no transitive
///   chaining through a bridge fact).
/// - The stored `subject` column is never rewritten (see `canonical_subject_key`).
fn op_fact_canonicalize(conn: &mut Connection) -> anyhow::Result<()> {
    // Loud guard (S6): a live fact with a NULL id (legacy inserts omit the
    // TEXT PRIMARY KEY, which SQLite permits) cannot be canonicalized or
    // referenced by fact_history. Count and warn rather than silently skipping;
    // the `id IS NOT NULL` filter below keeps such rows out of the pass.
    let null_ids: i64 = conn.query_row(
        "SELECT COUNT(*) FROM facts WHERE tombstone = 0 AND id IS NULL",
        [],
        |r| r.get(0),
    )?;
    if null_ids > 0 {
        eprintln!(
            "consolidate fact-canonicalize: WARN {null_ids} live fact(s) have a NULL id and are \
             EXCLUDED from canonicalization (cannot be tombstoned or logged safely)"
        );
    }

    let facts: Vec<CanonFact> = {
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, updated_at
               FROM facts
              WHERE tombstone = 0 AND id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(CanonFact {
                id: r.get(0)?,
                subject: r.get(1)?,
                predicate: r.get(2)?,
                object: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })?;
        // Do NOT swallow a row-decode error (S6): surface it and abort the op.
        let mut v = Vec::new();
        for row in rows {
            v.push(row?);
        }
        v
    };

    // Group by (canonical subject key, case-folded predicate). BTreeMap keeps the
    // pass deterministic across runs.
    let mut groups: std::collections::BTreeMap<(String, String), Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, f) in facts.iter().enumerate() {
        let key = (canonical_subject_key(&f.subject), f.predicate.to_lowercase());
        groups.entry(key).or_default().push(i);
    }

    let mut collapsed = 0usize;
    for (_key, mut members) in groups {
        if members.len() < 2 {
            continue;
        }
        // Best-first: newest `updated_at`, then longest object (most complete),
        // then id for a stable tie-break. ISO date/datetime strings sort
        // lexicographically, so a plain string compare is correct here.
        members.sort_by(|&a, &b| {
            facts[b]
                .updated_at
                .cmp(&facts[a].updated_at)
                .then_with(|| facts[b].object.len().cmp(&facts[a].object.len()))
                .then_with(|| facts[a].id.cmp(&facts[b].id))
        });

        // Leader clustering: each remaining fact is absorbed by the FIRST
        // surviving leader it is directly near-duplicate with. A fact matching
        // no earlier leader becomes a leader itself (a distinct claim survives).
        let mut absorbed = vec![false; members.len()];
        for li in 0..members.len() {
            if absorbed[li] {
                continue;
            }
            let leader = members[li];
            for ci in (li + 1)..members.len() {
                if absorbed[ci] {
                    continue;
                }
                let cand = members[ci];
                if objects_are_near_duplicates(&facts[leader].object, &facts[cand].object) {
                    absorbed[ci] = true;
                    tombstone_duplicate_fact(conn, &facts[cand], &facts[leader])?;
                    collapsed += 1;
                }
            }
        }
    }

    if collapsed > 0 {
        eprintln!("consolidate fact-canonicalize: collapsed {collapsed} near-duplicate fact(s)");
    }
    Ok(())
}

/// Tombstone `loser` as a near-duplicate of `survivor`. TOMBSTONE only — the row
/// is NEVER DELETEd. Logs loudly with BOTH fact ids and BOTH objects (Rule S6 /
/// spec constraint): stderr, a telemetry event, and a `fact_history` row (op is
/// CHECK-constrained to ADD/UPDATE/DELETE/FLAG; an UPDATE row with a descriptive
/// `new_value` records the collapse and names its survivor). Idempotent: guarded
/// on `tombstone = 0` so a re-run never double-logs an already-collapsed fact.
fn tombstone_duplicate_fact(
    conn: &Connection,
    loser: &CanonFact,
    survivor: &CanonFact,
) -> anyhow::Result<()> {
    // Loud, greppable audit (Rule S6 / spec constraint): both ids AND both objects.
    eprintln!(
        "consolidate fact-canonicalize: COLLAPSE tombstoning fact id={} object={:?} as \
         near-duplicate of surviving fact id={} object={:?} [subject={:?} predicate={:?}]",
        loser.id, loser.object, survivor.id, survivor.object, survivor.subject, survivor.predicate
    );

    // Tombstone — NEVER delete. Guard on tombstone=0 keeps re-runs idempotent.
    let changed = conn.execute(
        "UPDATE facts SET tombstone = 1 WHERE id = ?1 AND tombstone = 0",
        rusqlite::params![loser.id],
    )?;
    if changed == 0 {
        // Row was already tombstoned or vanished between load and write — do not
        // silently pretend we collapsed it (S6).
        eprintln!(
            "consolidate fact-canonicalize: WARN fact id={} was not live at write time; no \
             tombstone applied",
            loser.id
        );
        return Ok(());
    }

    // Durable audit row. Names the survivor so the collapse is reconstructable
    // from the ledger.
    conn.execute(
        "INSERT INTO fact_history (fact_id, op, prev_value, new_value, ts)
         VALUES (?1, 'UPDATE', ?2, ?3, datetime('now'))",
        rusqlite::params![
            loser.id,
            loser.object,
            format!(
                "tombstoned as canonical near-duplicate of fact {} (object: {})",
                survivor.id, survivor.object
            ),
        ],
    )?;

    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "memory::consolidate".into(),
        event: "fact-canonicalize::collapse".into(),
        status: "ok".into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(format!(
            "loser={} survivor={} subject={:?} predicate={:?}",
            loser.id, survivor.id, survivor.subject, survivor.predicate
        )),
    });

    Ok(())
}

fn op_contradiction_sweep(_conn: &mut Connection) -> anyhow::Result<()> {
    // Not yet implemented: resolve fact_history.op='FLAG' rows via LLM judge.
    eprintln!("consolidate op 'contradiction-sweep': not yet implemented");
    Ok(())
}

// PAUSED — see the op registration above. Kept compiled (not deleted) so the
// re-enable diff is one line once the access counter ships.
#[allow(dead_code)]
fn op_prune(conn: &mut Connection) -> anyhow::Result<()> {
    // Tombstone-eligible: access_count=0 AND age>60 AND subject!='user' AND predicate!='decided'
    conn.execute(
        "UPDATE facts SET tombstone = 1
         WHERE tombstone = 0 AND access_count = 0
           AND subject != 'user' AND predicate != 'decided'
           AND julianday('now') - julianday(updated_at) > 60",
        [],
    )?;
    Ok(())
}

fn op_topic_rollup(_conn: &mut Connection) -> anyhow::Result<()> {
    // Not yet implemented: maintain topics/fact_topics rollup.
    Ok(())
}

/// One quick tick may not hold the consolidate lock indefinitely — the
/// nightly full run needs it (lock_wait_budget = 45m). 10 minutes processes
/// ~10-20 slices; the 15-min cron picks the remainder up next tick.
pub(crate) const BACKSTOP_BUDGET: std::time::Duration = std::time::Duration::from_secs(10 * 60);

pub(crate) fn backstop_over_budget(start: std::time::Instant) -> bool {
    start.elapsed() >= BACKSTOP_BUDGET
}

/// Phase A transcript-delta backstop.
///
/// Scans `raw/transcripts/*.md`, registers any not-yet-known file in
/// `transcript_files` (reusing `memory::distill::watermark` — do NOT reinvent),
/// then runs the existing distill pipeline on the delta from that watermark
/// forward to capture corrections/decisions the live agent missed. Tolerates
/// gaps gracefully: not-yet-parsed transcripts, missing LLM key, sub-threshold
/// spans, parse failures — all are swallowed so the run continues. Idempotent:
/// a second invocation with no new content is a no-op (no duplicated row, no
/// regressed watermark).
pub fn op_transcript_backstop(conn: &mut Connection, hex_dir: &Path) -> anyhow::Result<()> {
    let dir = hex_dir.join("raw").join("transcripts");
    if !dir.exists() {
        return Ok(());
    }

    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    entries.sort();

    let loop_start = std::time::Instant::now();
    for (i, p) in entries.iter().enumerate() {
        if backstop_over_budget(loop_start) {
            let remaining = entries.len() - i;
            let msg = format!(
                "backstop budget ({:?}) reached — {remaining} file(s) deferred to next tick",
                BACKSTOP_BUDGET
            );
            println!("{msg}");
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "memory::consolidate".into(),
                event: "backstop::budget-stop".into(),
                status: "ok".into(),
                duration_ms: Some(loop_start.elapsed().as_millis() as i64),
                exit_code: None,
                detail: Some(msg),
            });
            break;
        }
        let path_str = match p.to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        // Register the file in transcript_files if absent. Reuses the
        // watermark primitive so there's exactly one writer to that table.
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM transcript_files WHERE path=?1",
                rusqlite::params![path_str.as_str()],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            crate::memory::distill::watermark::advance_offset(conn, &path_str, 0)?;
        }

        // Distill the delta. Errors (LLM unavailable, parse failure, etc.) are
        // tolerated so the backstop never crashes on partial state. The
        // watermark advances only when extraction succeeds end-to-end.
        if let Err(e) = crate::memory::distill::run_on_file(conn, &path_str, 0) {
            eprintln!("transcript-backstop: distill deferred for {path_str}: {e}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory;

    #[test]
    fn backstop_budget_constant_is_ten_minutes() {
        assert_eq!(BACKSTOP_BUDGET, std::time::Duration::from_secs(10 * 60));
        let fresh = std::time::Instant::now();
        assert!(!backstop_over_budget(fresh));
    }

    /// Regression: a consolidate run must stamp `metadata.last_consolidated`
    /// so `hex memory stats` stops reporting "never" after a real run.
    #[test]
    fn consolidate_stamps_last_consolidated_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("memory.db");
        // open_db applies the Plan 2 schema (facts, transcript_files, …) the
        // consolidate ops touch.
        let mut conn = memory::open_db(&db).unwrap();

        let before: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key='last_consolidated'",
                [],
                |r| r.get(0),
            )
            .ok();
        assert!(
            before.is_none(),
            "fresh DB should have no last_consolidated"
        );

        let _ = run(&mut conn).unwrap();

        let after: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key='last_consolidated'",
                [],
                |r| r.get(0),
            )
            .ok();
        assert!(
            after.is_some(),
            "consolidate must stamp last_consolidated into metadata"
        );
    }

    /// Pin the prune pause (Mike, 2026-06-11): until recall/search increment
    /// access_count, consolidation must NOT tombstone old facts — and the op
    /// must not appear in the run report.
    #[test]
    fn prune_is_paused_old_facts_survive() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("memory.db");
        let mut conn = memory::open_db(&db).unwrap();

        conn.execute(
            "INSERT INTO facts (subject, predicate, object, importance, access_count,
                                created_at, updated_at, tombstone)
             VALUES ('project:old', 'status', 'ancient but served daily', 0.7, 0,
                     datetime('now','-70 days'), datetime('now','-70 days'), 0)",
            [],
        )
        .unwrap();

        let report = run(&mut conn).unwrap();
        assert!(
            !report.ok.iter().any(|n| n == "prune")
                && !report.failed.iter().any(|(n, _)| n == "prune"),
            "prune op must be absent from the run report while paused"
        );

        let tombstone: i64 = conn
            .query_row(
                "SELECT tombstone FROM facts WHERE subject='project:old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            tombstone, 0,
            "a 70-day-old non-exempt fact must survive consolidation while prune is paused"
        );
    }

    // ---------------------------------------------------------------------
    // Task Tsfwg7d2v (recall-fix): conservative fact-canonicalization pass.
    //
    // RED tests. They exercise the STABLE consolidation entry point
    // `consolidate::run`, so they compile against the current tree and fail on
    // assertions (the canonicalization pass is not wired in yet) rather than on
    // a guessed function name. Named with "canonical" so the declared
    // verification `cargo test --release canonical` sweeps them up; today that
    // filter is GREEN only because of three unrelated pre-existing tests
    // (github_canonical_url_is_not_flagged, ledger_canonical_json_is_stable,
    // maintain_sweeps_orphan_vectors_and_canonicalizes_transcript_files).
    // ---------------------------------------------------------------------

    /// Insert one live fact with explicit identity and timestamps so the
    /// canonicalization pass has real rows to fold. `updated_at` doubles as
    /// `created_at` (recency signal); importance is held constant across a
    /// group so "newest complete wording" is the only tie-break in play.
    fn insert_canon_fact(
        conn: &Connection,
        id: &str,
        subject: &str,
        predicate: &str,
        object: &str,
        updated_at: &str,
    ) {
        conn.execute(
            "INSERT INTO facts (id, subject, predicate, object, importance,
                                created_at, updated_at, tombstone)
             VALUES (?1, ?2, ?3, ?4, 0.8, ?5, ?5, 0)",
            rusqlite::params![id, subject, predicate, object, updated_at],
        )
        .unwrap();
    }

    /// RED — fleet-coordinator three-spelling case (diagnosis root cause 2).
    /// The two UNAMBIGUOUS case+separator variants (`fleet-coordinator` /
    /// `Fleet Coordinator`, identical object) must canonicalize onto one
    /// subject and collapse to a single live row — proving subject
    /// canonicalization happened, since the collapse rule is keyed on
    /// canonical-subject + predicate + object similarity.
    ///
    /// The third spelling `hex-fleet-coordinator` carries an EXTRA token; whether
    /// to merge it is a conservative judgment the implement phase owns (spec STOP
    /// CONDITION: "cannot distinguish near-duplicates from genuinely distinct
    /// facts with high confidence for some class — stop and report the class").
    /// So this test only requires that its row SURVIVES — never that it collapses.
    #[test]
    fn canonical_folds_fleet_coordinator_case_and_separator_spellings() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("memory.db");
        let mut conn = memory::open_db(&db).unwrap();

        insert_canon_fact(
            &conn,
            "fc-1",
            "fleet-coordinator",
            "status",
            "coordinates the fleet of workers",
            "2026-08-30",
        );
        insert_canon_fact(
            &conn,
            "fc-2",
            "Fleet Coordinator",
            "status",
            "coordinates the fleet of workers",
            "2026-08-20",
        );
        insert_canon_fact(
            &conn,
            "fc-3",
            "hex-fleet-coordinator",
            "status",
            "coordinates workers across the hex fleet",
            "2026-08-10",
        );

        let _ = run(&mut conn).unwrap();

        // Tombstone, never DELETE: all three rows still present.
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 3, "canonicalization must tombstone, never DELETE rows");

        // The two case+separator variants must fold to ONE live row. Because the
        // collapse rule keys on canonical-subject + predicate + object
        // similarity, folding these two (different raw subjects) REQUIRES the
        // subject to have been canonicalized first.
        let live_variants: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts
                 WHERE id IN ('fc-1','fc-2') AND tombstone = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            live_variants, 1,
            "`fleet-coordinator` and `Fleet Coordinator` are pure case+separator \
             variants of one subject and must canonicalize + collapse to a single \
             live fact"
        );

        // The extra-token spelling's row survives regardless of the merge call.
        let fc3_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts WHERE id = 'fc-3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            fc3_rows, 1,
            "hex-fleet-coordinator row must never be deleted, whatever the \
             conservative merge decision"
        );
    }

    /// RED — repeated-decision case (diagnosis root cause 2: one decision
    /// re-extracted 6+ times as near-duplicate 0.9-importance facts). Several
    /// near-identical facts under the same subject+predicate must collapse to
    /// the single BEST wording (newest + most-complete), tombstoning the rest.
    /// `dec-best` is BOTH the newest `updated_at` AND a strict superset of the
    /// others' object text, so "best" is unambiguous — no tie-break the
    /// contract leaves unspecified.
    #[test]
    fn canonical_collapses_repeated_decision_near_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("memory.db");
        let mut conn = memory::open_db(&db).unwrap();

        insert_canon_fact(&conn, "dec-1", "Mike", "decided", "reply in GDD style", "2026-08-28");
        insert_canon_fact(&conn, "dec-2", "Mike", "decided", "reply in GDD style", "2026-08-29");
        insert_canon_fact(&conn, "dec-3", "Mike", "decided", "reply in GDD style", "2026-08-30");
        insert_canon_fact(
            &conn,
            "dec-best",
            "Mike",
            "decided",
            "reply in GDD style for all design replies",
            "2026-08-31",
        );

        let _ = run(&mut conn).unwrap();

        // Tombstone, never DELETE: every row still present.
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts WHERE subject='Mike' AND predicate='decided'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 4, "collapse must tombstone, never DELETE rows");

        // Exactly one live row survives the near-duplicate group.
        let live: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts
                 WHERE subject='Mike' AND predicate='decided' AND tombstone = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            live, 1,
            "near-duplicate re-extractions of one decision must collapse to a \
             single live fact"
        );

        // …and the survivor is the newest, most-complete wording.
        let survivor: String = conn
            .query_row(
                "SELECT id FROM facts
                 WHERE subject='Mike' AND predicate='decided' AND tombstone = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            survivor, "dec-best",
            "the newest, most-complete fact must be the one kept"
        );
    }

    /// GUARD (negative) — conservatism. Two facts sharing subject+predicate but
    /// stating genuinely DIFFERENT objects (low similarity) must NOT be
    /// collapsed: both survive live. This test is GREEN today and MUST stay
    /// green after the fix — it pins that the pass folds only high-confidence
    /// near-duplicates, never distinct claims (spec verification
    /// `conservative-collapse`).
    #[test]
    fn canonical_keeps_distinct_facts_sharing_subject_and_predicate() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("memory.db");
        let mut conn = memory::open_db(&db).unwrap();

        insert_canon_fact(
            &conn,
            "wo-1",
            "Mike",
            "works-on",
            "hex, a persistent self-improving AI agent system",
            "2026-08-18",
        );
        insert_canon_fact(
            &conn,
            "wo-2",
            "Mike",
            "works-on",
            "the world-thesis essay on economic growth",
            "2026-08-23",
        );

        let _ = run(&mut conn).unwrap();

        let live: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts
                 WHERE subject='Mike' AND predicate='works-on' AND tombstone = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            live, 2,
            "distinct facts sharing subject+predicate must both survive — \
             conservative canonicalization never collapses different claims"
        );
    }

    /// GUARD (negative) — polarity/contradiction class. A claim and its NEGATION
    /// share a subject+predicate, and one object is a strict superset of the
    /// other that differs ONLY by a negation word ("use Postgres" vs "do not use
    /// Postgres"). A naive overlap coefficient with a `min(|A|,|B|)` denominator
    /// scores such a pair 1.0 and would collapse them — TOMBSTONING a
    /// contradicting fact. That is exactly the dangerous class the spec HARD
    /// CONSTRAINT ("high confidence") and the `conservative-collapse`
    /// verification forbid: both rows must survive live.
    #[test]
    fn canonical_keeps_polarity_flipped_near_superset_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("memory.db");
        let mut conn = memory::open_db(&db).unwrap();

        insert_canon_fact(&conn, "pol-1", "Mike", "decided", "use Postgres", "2026-08-20");
        insert_canon_fact(
            &conn,
            "pol-2",
            "Mike",
            "decided",
            "do not use Postgres",
            "2026-08-25",
        );

        let _ = run(&mut conn).unwrap();

        let live: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts
                 WHERE subject='Mike' AND predicate='decided' AND tombstone = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            live, 2,
            "a claim and its negation must never collapse — token overlap alone \
             is not high-confidence evidence of a duplicate when the differing \
             tokens flip polarity"
        );
    }
}
