//! v1 ContextAssembler — parallel retrieval moves merged by a simple
//! confidence score with a coverage floor. See
//! `me/decisions/context-assembly-parallel-moves-confidence-2026-06-04.md`.
//!
//! v1 is a KEYWORD-SHAPE assembler. M1's vector arm fires ONLY when the
//! caller supplies a pre-computed `query_vec` (semantic policy is a
//! caller decision, per spec Tj0b203yv). The assembler NEVER constructs an
//! `Embedder` itself — that would blow the UserPromptSubmit hook's latency
//! budget, since the hook is a fresh OS process per user message and the
//! 522 MB nomic model would load on every non-trivial recall (audit finding
//! 1, 2026-07-16). The hot path (`recall::recall`, `harness::submit`) MUST
//! pass `None`; offline CLI callers who want semantic search embed the query
//! themselves and pass `Some(&qv)`.

use rusqlite::Connection;
use std::collections::HashSet;

use super::recall::FactHit;
use super::recall_config::RecallConfig;
use super::search::{search_fts_public, SearchResult};

pub const MAX_CONTEXT_CHARS: usize = 10_000;

const TOP_K_PER_MOVE: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MoveId {
    M1ContentMatch,
    M2EntityFilter,
    M3PredicateQuery,
    M4TemporalSelect,
    /// Facts ranked by query relevance (BM25 over facts_fts, RRF-fused with
    /// the vector arm when a query_vec is supplied). The arm the 2026-08-18
    /// recall audit found missing: M2/M3/M4 order by importance/recency and
    /// never score fact content against the query, so the correct fact lost
    /// to generic high-importance facts on 14/17 golden-set questions.
    M5FactRelevance,
}

pub enum CandidateKind {
    Chunk(SearchResult),
    Fact(FactHit),
}

pub struct Candidate {
    pub kind: CandidateKind,
    pub move_id: MoveId,
    pub move_fired: bool,
    pub native_score: f64,
    pub rank_in_move: usize,
    pub confidence: f32,
    pub dedup_key: String,
}

pub struct MoveStats {
    pub move_id: MoveId,
    pub fired: bool,
    pub candidate_count: usize,
    pub top_native_scores: Vec<f64>,
}

pub struct AssembledContext {
    pub candidates: Vec<Candidate>,
    pub per_move_stats: Vec<MoveStats>,
}

// ───────────────────────────── cue detection ──────────────────────────────

/// Map query words to the stored predicate vocabulary. Returns the list of
/// canonical predicate names whose cues appear in the query.
fn predicate_cues(query: &str) -> Vec<&'static str> {
    let q = query.to_lowercase();
    let toks: HashSet<&str> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let mut out: Vec<&'static str> = Vec::new();
    let map: &[(&[&str], &str)] = &[
        (&["decide", "decided", "decision"], "decided"),
        (&["prefer", "prefers", "preference"], "prefers"),
        (&["dislike", "dislikes"], "dislikes"),
        // "blocker"/"blockers" are the NOUN phrasing ("who are the blockers")
        // the verb-only cues missed — `predicate_cues` does exact HashSet
        // membership (no stemming), so the nouns need explicit entries
        // (task Tkmz6c46q, diagnosis 2026-08-31 case c-13).
        (
            &["block", "blocked", "blocking", "blocker", "blockers"],
            "blocked-by",
        ),
        (&["responsible", "owner", "owns"], "responsible-for"),
        (&["plan", "plans", "planning"], "plans-to"),
        (&["focus", "focused", "focusing"], "current-focus"),
        (&["status"], "status"),
        (&["know", "knows", "knowing"], "knows"),
        (&["learned", "learning", "learn"], "learned-that"),
        (&["commit", "committed", "committing"], "committed-to"),
        (&["values"], "values"),
        (&["avoid", "avoids", "avoiding"], "avoids"),
        (&["work", "works", "working"], "works-on"),
        // "has" is the single most common predicate in production (427/1,863
        // facts on the 2026-08-18 audit snapshot) and was unreachable by any
        // query phrasing before this cue existed.
        (&["has", "have"], "has"),
    ];
    for (cues, pred) in map {
        if cues.iter().any(|c| toks.contains(c)) && !out.contains(pred) {
            out.push(*pred);
        }
    }
    out
}

fn is_temporal(query: &str) -> bool {
    let q = query.to_lowercase();
    let toks: HashSet<&str> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    // Strong, unambiguous temporal intent — always fires M4.
    if ["current", "latest", "today", "recent", "recently"]
        .iter()
        .any(|c| toks.contains(*c))
    {
        return true;
    }
    // "now" is a WEAK cue: it is filler in many non-temporal questions
    // ("what is mike building now") and, firing M4 on its own, floods the merge
    // with same-day facts that crowd out the real answer (task T8s8bq3th,
    // diagnosis 2026-08-31 case a-mike-building). Treat a lone `now` as
    // NON-temporal; only an explicit temporal phrase built around `now` fires M4.
    if toks.contains("now") {
        return q.contains("right now") || q.contains("as of now") || q.contains("just now");
    }
    false
}

/// Build the entity gazetteer from DISTINCT facts.subject. Returns a list of
/// (full_subject, lowercase_match_token) pairs — the token is the slug after
/// the colon (e.g. "alice" for "person:alice").
fn detect_entity_subjects(conn: &Connection, query: &str) -> Vec<String> {
    let q = query.to_lowercase();
    let toks: HashSet<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_string())
        .collect();
    if toks.is_empty() {
        return Vec::new();
    }
    let mut matched: Vec<String> = Vec::new();
    let mut stmt = match conn.prepare("SELECT DISTINCT subject FROM facts WHERE tombstone = 0") {
        Ok(s) => s,
        Err(_) => return matched,
    };
    let rows = match stmt.query_map([], |r| r.get::<_, String>(0)) {
        Ok(r) => r,
        Err(_) => return matched,
    };
    for subj in rows.filter_map(Result::ok) {
        let lower = subj.to_lowercase();
        let mut hit = false;
        if toks.contains(&lower) {
            // Whole-subject exact token (single-word subjects like "zwerk").
            hit = true;
        } else {
            // Strip an optional leading `type:` prefix (person:, project:) so
            // the type token never triggers a match, then split the remainder
            // on every word separator. This reaches hyphen-, underscore-,
            // slash-, and space-delimited AND multi-word subjects — none of
            // which is ever a single query token: fleet-coordinator,
            // "hex project", hex-v2-arch (task Tkmz6c46q, diagnosis 2026-08-31).
            // Broadening M2's match surface raises its firing rate; the
            // entity-intersection window fix (task T8s8bq3th) is what keeps the
            // wider match from flooding the merge.
            let slug = lower.splitn(2, ':').nth(1).unwrap_or(lower.as_str());
            for piece in slug.split([':', '-', '_', '/', ' ']) {
                if piece.len() >= 3 && toks.contains(piece) {
                    hit = true;
                    break;
                }
            }
        }
        if hit && !matched.contains(&subj) {
            matched.push(subj);
        }
    }
    matched
}

/// Distinctive query terms for the M2 relevance blend: lowercased, alphanumeric,
/// length >= 3, with the generic question/stop words dropped so they can't
/// inflate an object's relevance score.
fn query_terms(query: &str) -> HashSet<String> {
    query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| {
            t.len() >= 3
                && !matches!(
                    *t,
                    "the" | "and"
                        | "for"
                        | "are"
                        | "was"
                        | "who"
                        | "what"
                        | "how"
                        | "does"
                        | "did"
                        | "you"
                        | "this"
                        | "that"
                        | "about"
                )
        })
        .map(|t| t.to_string())
        .collect()
}

/// Count how many distinct query terms appear as tokens of `object`. This is
/// the query-relevance signal M2 blends into its previously importance-only
/// ordering (task Tkmz6c46q, diagnosis 2026-08-31 case b-brand-lead-restrictions).
fn object_relevance(object: &str, qterms: &HashSet<String>) -> usize {
    if qterms.is_empty() {
        return 0;
    }
    let obj_toks: HashSet<String> = object
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_string())
        .collect();
    qterms.iter().filter(|q| obj_toks.contains(*q)).count()
}

// ─────────────────────────────────── moves ─────────────────────────────────

fn fact_select_sql(extra_where: &str, order: &str) -> String {
    format!(
        "SELECT subject, predicate, object, importance, private, created_at \
         FROM facts \
         WHERE tombstone = 0 {} \
         ORDER BY {} LIMIT ?",
        extra_where, order
    )
}

fn fact_from_row(r: &rusqlite::Row) -> rusqlite::Result<(FactHit, f64)> {
    let importance: f32 = r.get(3)?;
    Ok((
        FactHit {
            subject: r.get(0)?,
            predicate: r.get(1)?,
            object: r.get(2)?,
            importance,
            private: r.get::<_, i64>(4)? != 0,
        },
        importance as f64,
    ))
}

/// M1 — content match. ALWAYS fires. FTS5 chunks, plus vector KNN ONLY when
/// the caller supplies a pre-computed `query_vec` (semantic policy is
/// caller-decided per spec Tj0b203yv). Returns ordered candidates.
fn m1_content(
    conn: &Connection,
    query: &str,
    for_agent: bool,
    query_vec: Option<&[f32]>,
    cfg: &RecallConfig,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();

    let chunks = search_fts_public(conn, query, TOP_K_PER_MOVE * 3, None).unwrap_or_default();
    for (rank, c) in chunks
        .into_iter()
        .filter(|r| !(for_agent && r.private))
        .take(TOP_K_PER_MOVE)
        .enumerate()
    {
        let native = c.score;
        let dedup_key = format!("chunk:{}", c.rowid);
        let move_fired = true;
        let confidence = cfg.move_relevance.factor(move_fired) * (1.0 / (rank as f32 + 1.0));
        out.push(Candidate {
            kind: CandidateKind::Chunk(c),
            move_id: MoveId::M1ContentMatch,
            move_fired,
            native_score: native,
            rank_in_move: rank,
            confidence,
            dedup_key,
        });
    }

    // Vector arm — caller-decided embedder policy. `None` = FTS-only (the
    // UserPromptSubmit hot path per spec Tj0b203yv). `Some(qv)` = semantic
    // fusion. The assembler NEVER constructs an `Embedder` itself; the hook
    // process would otherwise cold-load a 522 MB model on every non-trivial
    // message.
    if let Some(qv) = query_vec {
        match super::vector::knn(conn, qv, TOP_K_PER_MOVE) {
            Ok(hits) => {
                for (i, (rowid, dist)) in hits.iter().enumerate() {
                    // Fetch the chunk row to build a SearchResult.
                    if let Ok(c) = conn.query_row(
                        "SELECT rowid, source_path, heading, chunk_index, content, private \
                         FROM chunks WHERE rowid = ?1",
                        [rowid],
                        |r| {
                            Ok(SearchResult {
                                rowid: r.get(0)?,
                                source_path: r.get(1)?,
                                heading: r.get(2)?,
                                chunk_index: r.get(3)?,
                                content: r.get(4)?,
                                private: r.get::<_, i64>(5)? != 0,
                                score: *dist,
                            })
                        },
                    ) {
                        if for_agent && c.private {
                            continue;
                        }
                        let dedup_key = format!("chunk:{}", c.rowid);
                        if out.iter().any(|x| x.dedup_key == dedup_key) {
                            continue;
                        }
                        let rank = i;
                        let confidence = 1.0 / (rank as f32 + 1.0);
                        out.push(Candidate {
                            kind: CandidateKind::Chunk(c),
                            move_id: MoveId::M1ContentMatch,
                            move_fired: true,
                            native_score: *dist,
                            rank_in_move: rank,
                            confidence,
                            dedup_key,
                        });
                    }
                }
            }
            Err(e) => {
                eprintln!("[assemble] M1 vector knn failed: {e}");
            }
        }
    }

    out
}

/// M2 — entity filter. Fires when at least one detected entity matches a
/// stored fact subject.
fn m2_entity(
    conn: &Connection,
    query: &str,
    for_agent: bool,
    subjects: &[String],
    cfg: &RecallConfig,
) -> (bool, Vec<Candidate>) {
    if subjects.is_empty() {
        return (false, Vec::new());
    }
    // Query relevance blended into M2's ordering (task Tkmz6c46q). M2 used to
    // order purely by importance, so a low-importance fact that actually
    // answers the query was buried below generic high-importance facts under
    // the same subject and never entered the per-subject top-K window
    // (diagnosis 2026-08-31 case b-brand-lead-restrictions). We now fetch a
    // WIDER importance-ordered window per subject, re-rank it by
    // (query-relevance, importance), and keep the top-K — so a relevant fact
    // that importance alone would drop is re-surfaced, without widening the
    // number of candidates the move ultimately contributes.
    let qterms = query_terms(query);
    let mut scored: Vec<(FactHit, f64, usize)> = Vec::new();
    for subj in subjects {
        let extra = if for_agent {
            " AND subject = ?1 AND private = 0"
        } else {
            " AND subject = ?1"
        };
        let sql = fact_select_sql(extra, "importance DESC, created_at DESC");
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let window: Vec<(FactHit, f64)> = match stmt
            .query_map(
                rusqlite::params![subj, (TOP_K_PER_MOVE * 3) as i64],
                fact_from_row,
            ) {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        };
        drop(stmt);
        let mut ranked: Vec<(FactHit, f64, usize)> = window
            .into_iter()
            .map(|(f, imp)| {
                let rel = object_relevance(&f.object, &qterms);
                (f, imp, rel)
            })
            .collect();
        // Relevance first, importance breaks ties. Stable sort preserves the
        // SQL importance/recency order within an equal (relevance, importance).
        ranked.sort_by(|a, b| {
            b.2.cmp(&a.2).then(
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        ranked.truncate(TOP_K_PER_MOVE);
        scored.extend(ranked);
    }
    // Same blended key across subjects for a stable overall rank ordering.
    scored.sort_by(|a, b| {
        b.2.cmp(&a.2).then(
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    let hits: Vec<(FactHit, f64)> = scored.into_iter().map(|(f, imp, _)| (f, imp)).collect();
    let cands = facts_to_candidates(hits, MoveId::M2EntityFilter, true, cfg);
    (true, cands)
}

/// M3 — predicate query. Fires when a cue maps to a known predicate.
///
/// `entity_subjects` are the M2-detected subjects (empty when the query names
/// no entity). When non-empty, the per-predicate window is INTERSECTED with
/// those subjects BEFORE the top-K cut, so a global per-predicate window can no
/// longer flood the merge with higher-importance facts from OTHER subjects that
/// happen to share the cued predicate (task T8s8bq3th, diagnosis 2026-08-31
/// root cause 3, cases hex-focus / a-hex-startup-skill). Empty subjects ⇒ the
/// previous global window, byte-identical.
fn m3_predicate(
    conn: &Connection,
    query: &str,
    for_agent: bool,
    entity_subjects: &[String],
    cfg: &RecallConfig,
) -> (bool, Vec<Candidate>) {
    use rusqlite::types::Value;
    let preds = predicate_cues(query);
    if preds.is_empty() {
        return (false, Vec::new());
    }
    let mut hits: Vec<(FactHit, f64)> = Vec::new();
    for pred in &preds {
        let mut sql = String::from(
            "SELECT subject, predicate, object, importance, private, created_at \
             FROM facts WHERE tombstone = 0 AND predicate = ?",
        );
        let mut params: Vec<Value> = vec![Value::Text((*pred).to_string())];
        if for_agent {
            sql.push_str(" AND private = 0");
        }
        if !entity_subjects.is_empty() {
            sql.push_str(" AND subject IN (");
            for (i, s) in entity_subjects.iter().enumerate() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push('?');
                params.push(Value::Text(s.clone()));
            }
            sql.push(')');
        }
        sql.push_str(" ORDER BY importance DESC, created_at DESC LIMIT ?");
        params.push(Value::Integer(TOP_K_PER_MOVE as i64));
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let collected: Vec<(FactHit, f64)> =
            match stmt.query_map(rusqlite::params_from_iter(params), |r| fact_from_row(r)) {
                Ok(rows) => rows.filter_map(Result::ok).collect(),
                Err(_) => Vec::new(),
            };
        drop(stmt);
        hits.extend(collected);
    }
    hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let cands = facts_to_candidates(hits, MoveId::M3PredicateQuery, true, cfg);
    (true, cands)
}

/// M4 — temporal select (FACTS ONLY; chunks have no timestamp column).
///
/// `entity_subjects` are the M2-detected subjects. When non-empty the recency
/// window is INTERSECTED with them BEFORE the top-K cut, so a temporal query
/// that also names an entity ("what did tara decide recently") returns that
/// entity's recent facts rather than a global same-day grab-bag that floods the
/// merge (task T8s8bq3th). Empty subjects ⇒ the previous global window,
/// byte-identical.
fn m4_temporal(
    conn: &Connection,
    query: &str,
    for_agent: bool,
    entity_subjects: &[String],
    cfg: &RecallConfig,
) -> (bool, Vec<Candidate>) {
    use rusqlite::types::Value;
    if !is_temporal(query) {
        return (false, Vec::new());
    }
    let mut sql = String::from(
        "SELECT subject, predicate, object, importance, private, created_at \
         FROM facts WHERE tombstone = 0",
    );
    let mut params: Vec<Value> = Vec::new();
    if for_agent {
        sql.push_str(" AND private = 0");
    }
    if !entity_subjects.is_empty() {
        sql.push_str(" AND subject IN (");
        for (i, s) in entity_subjects.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            sql.push('?');
            params.push(Value::Text(s.clone()));
        }
        sql.push(')');
    }
    sql.push_str(" ORDER BY created_at DESC, importance DESC LIMIT ?");
    params.push(Value::Integer(TOP_K_PER_MOVE as i64));
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return (true, Vec::new()),
    };
    let hits: Vec<(FactHit, f64)> = stmt
        .query_map(rusqlite::params_from_iter(params), |r| fact_from_row(r))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    let cands = facts_to_candidates(hits, MoveId::M4TemporalSelect, true, cfg);
    (true, cands)
}

/// M5 — fact relevance. Facts ranked against the query by the fused
/// retrieval `facts_recall` implements (dual-weighted BM25 over the widened
/// facts_fts + slug arm + optional vector KNN, RRF-fused; importance only
/// breaks ties). Fires when the query yields at least one relevance-ranked
/// fact. Relevance order is the candidate order; native_score carries the
/// RRF score — the signal that actually determined the rank — for the
/// recall-log calibration seam. Privacy filters in SQL before truncation
/// (for_agent = exclude_private).
fn m5_fact_relevance(
    conn: &Connection,
    query: &str,
    for_agent: bool,
    query_vec: Option<&[f32]>,
    cfg: &RecallConfig,
) -> (bool, Vec<Candidate>) {
    let hits: Vec<(FactHit, f64)> = match super::recall::facts_recall_with_config(
        conn,
        query,
        TOP_K_PER_MOVE,
        query_vec,
        for_agent,
        cfg,
    ) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[assemble] M5 fact relevance failed: {e}");
            return (false, Vec::new());
        }
    };
    if hits.is_empty() {
        return (false, Vec::new());
    }
    let cands = facts_to_candidates(hits, MoveId::M5FactRelevance, true, cfg);
    (true, cands)
}

/// Stable per-object fingerprint for the fact dedup key. `FactHit` carries no
/// fact ULID (only subject/predicate/object), so the object IS the identity
/// signal. Hashed verbatim — deliberately case-sensitive — so distinct facts
/// sharing subject+predicate get distinct keys; object-case near-duplicates are
/// the canonicalization pass's job (task Tsfwg7d2v), not this key. `DefaultHasher`
/// is fine: dedup keys are only ever compared within a single `assemble()` call,
/// so cross-run hash stability is not load-bearing.
fn fact_object_fingerprint(object: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    object.hash(&mut h);
    h.finish()
}

fn facts_to_candidates(
    hits: Vec<(FactHit, f64)>,
    move_id: MoveId,
    fired: bool,
    cfg: &RecallConfig,
) -> Vec<Candidate> {
    let mr = cfg.move_relevance.factor(fired);
    hits.into_iter()
        .enumerate()
        .map(|(rank, (f, native))| {
            // Object-aware, case-insensitive fact dedup key (task T28958xxp,
            // diagnosis 2026-08-31). The old key `fact:{subject}|{predicate}`
            // was object-blind and case-sensitive: because the merge shares ONE
            // seen-set across the floor and round-robin loops, at most one fact
            // per (subject,predicate) pair could ever enter assembled context —
            // evicting the other 245-of-1600 groups that hold 2+ facts. Folding
            // subject+predicate case collapses true case-variant duplicates,
            // while the per-object fingerprint keeps genuinely distinct facts
            // (different object) apart so they no longer evict each other.
            // NOTE: the object is fingerprinted verbatim (NOT case-folded) —
            // object-similarity collapsing is the deliberate, logged job of the
            // canonicalization pass (task Tsfwg7d2v), not this key.
            let dedup_key = format!(
                "fact:{}|{}|{:016x}",
                f.subject.to_lowercase(),
                f.predicate.to_lowercase(),
                fact_object_fingerprint(&f.object),
            );
            let confidence = mr * (1.0 / (rank as f32 + 1.0));
            Candidate {
                kind: CandidateKind::Fact(f),
                move_id,
                move_fired: fired,
                native_score: native,
                rank_in_move: rank,
                confidence,
                dedup_key,
            }
        })
        .collect()
}

// ───────────────────────────────── merge ───────────────────────────────────

fn cand_chars(c: &Candidate) -> usize {
    match &c.kind {
        CandidateKind::Chunk(s) => {
            let snip = s.content.chars().take(600).count();
            snip + s.source_path.len() + s.heading.len() + 16
        }
        CandidateKind::Fact(f) => f.subject.len() + f.predicate.len() + f.object.len() + 8,
    }
}

fn move_stats(move_id: MoveId, fired: bool, cands: &[Candidate]) -> MoveStats {
    let top_native_scores: Vec<f64> = cands.iter().take(3).map(|c| c.native_score).collect();
    MoveStats {
        move_id,
        fired,
        candidate_count: cands.len(),
        top_native_scores,
    }
}

/// Public entry. Runs the four moves, merges with floor + per-move quota
/// round-robin by confidence DESC, dedups, and truncates to the char budget.
///
/// `query_vec` is the caller-decided embedder policy (spec Tj0b203yv):
/// - `None` → FTS-only. The UserPromptSubmit hook path (`recall::recall`) and
///   the worker submit path MUST pass `None` so no `Embedder` is constructed
///   in a fresh OS process.
/// - `Some(qv)` → semantic fusion via `vector::knn`. Offline CLI callers that
///   want semantic M1 embed the query themselves and pass the vector.
///
/// The assembler NEVER constructs an `Embedder`.
pub fn assemble(
    conn: &Connection,
    query: &str,
    for_agent: bool,
    budget: usize,
    query_vec: Option<&[f32]>,
) -> AssembledContext {
    assemble_with_chunk_cap(conn, query, for_agent, budget, query_vec, usize::MAX)
}

/// `assemble` with a cap on how many chunk candidates the merge admits.
/// Callers that render only the first N chunks (recall renders 2) MUST pass
/// their render cap: un-rendered chunks otherwise consume the char budget
/// (~600 each) and silently crowd out facts — measured on the 2026-08-18
/// golden set as the difference between a relevant fact landing in context
/// or not. Chunks past the cap are skipped without charging the budget.
///
/// Uses the compiled-default recall parameters. Callers that must apply an
/// instance-tuned config (the hot recall path) or score a variant (the eval
/// sweep) use [`assemble_with_config`].
pub fn assemble_with_chunk_cap(
    conn: &Connection,
    query: &str,
    for_agent: bool,
    budget: usize,
    query_vec: Option<&[f32]>,
    max_chunks: usize,
) -> AssembledContext {
    assemble_with_config(
        conn,
        query,
        for_agent,
        budget,
        query_vec,
        // Offline semantic callers pass one vector meaning "both arms on";
        // forward it to the facts arm too so their behavior is unchanged.
        query_vec,
        max_chunks,
        &RecallConfig::default(),
    )
}

/// [`assemble_with_chunk_cap`] with an explicit recall config. This is the
/// single site the recall ranking parameters (RRF constant, bm25 arm weights,
/// M5 relevance-move multipliers) enter the assembler. `&RecallConfig::default()`
/// reproduces the previous hardcoded behavior exactly.
///
/// `query_vec` drives the CHUNK-side vector arm (M1); `facts_query_vec` drives
/// the FACTS-side KNN arm (M5, `facts_recall`) independently. Keeping them
/// separate lets the hot recall path turn on ONLY the facts arm
/// (`query_vec = None`, `facts_query_vec = Some(qv)`) without lighting the
/// chunk arm — so chunk results stay byte-identical and the task-3 facts A/B
/// isolates the one arm it means to measure (spec Sdnap37he, task Ttrmaca6q;
/// exclusion: do not regress existing arms). Offline semantic callers pass the
/// same vector to both (see [`assemble_with_chunk_cap`]).
pub fn assemble_with_config(
    conn: &Connection,
    query: &str,
    for_agent: bool,
    budget: usize,
    query_vec: Option<&[f32]>,
    facts_query_vec: Option<&[f32]>,
    max_chunks: usize,
    cfg: &RecallConfig,
) -> AssembledContext {
    let budget = if budget == 0 {
        MAX_CONTEXT_CHARS
    } else {
        budget
    };

    // ── run the moves (sequential — local SQLite, the cost is dominated by
    // FTS5/index lookups; "parallel" in spec scope is logical, not threaded).
    let m1_c = m1_content(conn, query, for_agent, query_vec, cfg);
    let (m5_f, m5_c) = m5_fact_relevance(conn, query, for_agent, facts_query_vec, cfg);
    // Detect entity subjects ONCE and thread them to M2/M3/M4. M3 and M4
    // intersect their fetch with these subjects BEFORE the top-K window (task
    // T8s8bq3th): a global per-predicate / per-day window otherwise lets
    // higher-importance or same-day facts from OTHER subjects flood out the
    // fact the query's named entity actually holds. Empty ⇒ every move keeps
    // its prior global behavior, so no-entity queries stay byte-identical.
    let entity_subjects = detect_entity_subjects(conn, query);
    let (m2_f, m2_c) = m2_entity(conn, query, for_agent, &entity_subjects, cfg);
    let (m3_f, m3_c) = m3_predicate(conn, query, for_agent, &entity_subjects, cfg);
    let (m4_f, m4_c) = m4_temporal(conn, query, for_agent, &entity_subjects, cfg);

    let per_move_stats = vec![
        move_stats(MoveId::M1ContentMatch, true, &m1_c),
        move_stats(MoveId::M5FactRelevance, m5_f, &m5_c),
        move_stats(MoveId::M2EntityFilter, m2_f, &m2_c),
        move_stats(MoveId::M3PredicateQuery, m3_f, &m3_c),
        move_stats(MoveId::M4TemporalSelect, m4_f, &m4_c),
    ];

    // ── merge: FLOOR — M1 top-1 first, then each fired non-M1 move's top-1.
    // M5 sits directly after M1 so the relevance-ranked fact wins any
    // dedup-key collision against importance-ranked M2/M3/M4 picks, and ties
    // in the round-robin (stable sort) resolve in relevance's favor.
    let mut merged: Vec<Candidate> = Vec::new();
    let mut chars = 0usize;
    let mut seen: HashSet<String> = HashSet::new();
    let mut queues: Vec<(MoveId, std::vec::IntoIter<Candidate>)> = vec![
        (MoveId::M1ContentMatch, m1_c.into_iter()),
        (MoveId::M5FactRelevance, m5_c.into_iter()),
        (MoveId::M2EntityFilter, m2_c.into_iter()),
        (MoveId::M3PredicateQuery, m3_c.into_iter()),
        (MoveId::M4TemporalSelect, m4_c.into_iter()),
    ];

    let mut chunks_taken = 0usize;

    // Floor: take the first available from each queue, M1 first.
    for (move_id, queue) in &mut queues {
        // Skip non-fired moves on the floor — they get the 0.3 demotion and
        // do not warrant a guaranteed slot. M1 always fires.
        let fired = match move_id {
            MoveId::M1ContentMatch => true,
            MoveId::M5FactRelevance => m5_f,
            MoveId::M2EntityFilter => m2_f,
            MoveId::M3PredicateQuery => m3_f,
            MoveId::M4TemporalSelect => m4_f,
        };
        if !fired {
            continue;
        }
        if let Some(cand) = queue.next() {
            let is_chunk = matches!(cand.kind, CandidateKind::Chunk(_));
            if is_chunk && chunks_taken >= max_chunks {
                continue;
            }
            let cost = cand_chars(&cand);
            if seen.insert(cand.dedup_key.clone()) {
                if is_chunk {
                    chunks_taken += 1;
                }
                if chars + cost > budget {
                    // Floor over-budget — still push so the facet coverage
                    // contract is honored, then stop (no further candidates
                    // are considered, so `chars` needs no update).
                    merged.push(cand);
                    return AssembledContext {
                        candidates: merged,
                        per_move_stats,
                    };
                }
                merged.push(cand);
                chars += cost;
            }
        }
    }

    // ── per-move QUOTA round-robin by confidence: at each round, each fired
    //    move offers its next-best candidate; we keep them sorted by
    //    confidence DESC across the round so highest-confidence wins ties.
    loop {
        // Gather one candidate from each non-empty queue.
        let mut round: Vec<Candidate> = Vec::new();
        for q in queues.iter_mut() {
            if let Some(c) = q.1.next() {
                round.push(c);
            }
        }
        if round.is_empty() {
            break;
        }
        round.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for cand in round {
            let is_chunk = matches!(cand.kind, CandidateKind::Chunk(_));
            if is_chunk && chunks_taken >= max_chunks {
                continue;
            }
            if !seen.insert(cand.dedup_key.clone()) {
                continue;
            }
            if is_chunk {
                chunks_taken += 1;
            }
            let cost = cand_chars(&cand);
            if chars + cost > budget {
                return AssembledContext {
                    candidates: merged,
                    per_move_stats,
                };
            }
            merged.push(cand);
            chars = chars.saturating_add(cost);
        }
    }
    let _ = chars;

    AssembledContext {
        candidates: merged,
        per_move_stats,
    }
}

/// Render assembled candidates into the worker-facing context block. This is the
/// layer above which `submit()` prepends the reply "pin".
///
/// NOTE: `Candidate` has NO `content` field and NO `Default` derive (verified
/// 2026-06-05). Text lives inside `CandidateKind`.
pub fn render_candidates(ctx: &AssembledContext) -> String {
    ctx.candidates
        .iter()
        .map(|c| match &c.kind {
            CandidateKind::Chunk(s) => s.content.clone(),
            CandidateKind::Fact(f) => format!("{} {} {}", f.subject, f.predicate, f.object),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fresh_db() -> Connection {
        crate::memory::vector::register_sqlite_vec();
        let c = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&c).unwrap();
        crate::memory::schema::apply_plan2(&c).unwrap();
        // Production form: chunks IS the FTS5 vtable (see search.rs setup_db
        // and index.rs:379). search_fts_public queries `chunks MATCH ?` so
        // the column layout must match.
        c.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
                file_id UNINDEXED,
                source_path UNINDEXED,
                heading,
                chunk_index UNINDEXED,
                content,
                private UNINDEXED,
                tokenize='unicode61'
            );",
        )
        .unwrap();
        c
    }

    fn insert_chunk(c: &Connection, path: &str, content: &str, private: bool) {
        c.execute(
            "INSERT INTO chunks(file_id,source_path,heading,chunk_index,content,private)
             VALUES ('1',?1,'h','0',?2,?3)",
            rusqlite::params![path, content, private as i32],
        )
        .unwrap();
    }

    fn insert_fact(
        c: &Connection,
        id: &str,
        subject: &str,
        predicate: &str,
        object: &str,
        private: bool,
    ) {
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
             VALUES (?1,?2,?3,?4,0.8,'2026-06-04','2026-06-04',?5)",
            rusqlite::params![id, subject, predicate, object, private as i32],
        )
        .unwrap();
    }

    /// Floor: M1's top-1 is placed first, and each OTHER fired move
    /// contributes its top-1 before any further fill.
    #[test]
    fn floor_places_m1_top1_first_and_each_fired_move_top1() {
        let c = fresh_db();
        insert_chunk(&c, "docs/schema.md", "schema decision memory layer", false);
        // Predicate cue ("decided") should fire M3.
        insert_fact(&c, "f1", "project:hex", "decided", "use sqlite-vec", false);
        // Entity in gazetteer should fire M2.
        insert_fact(&c, "f2", "person:alice", "prefers", "rust", false);

        let r = assemble(
            &c,
            "what did alice decide about the schema",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );

        assert!(!r.candidates.is_empty(), "assembler returned no candidates");
        // First candidate must come from M1 (the floor).
        assert_eq!(
            r.candidates[0].move_id,
            MoveId::M1ContentMatch,
            "M1 top-1 must be placed first as the floor"
        );
        // Each fired non-M1 move's coverage must reach the merge. Since M5
        // (fact relevance) runs first and shares dedup keys, the guarantee is
        // that the move's facts are PRESENT — not that the move gets the
        // attribution (M5 legitimately claims the same facts first).
        let objects: Vec<&str> = r
            .candidates
            .iter()
            .filter_map(|c| match &c.kind {
                CandidateKind::Fact(f) => Some(f.object.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            objects.iter().any(|o| o.contains("use sqlite-vec")),
            "M3-covered fact missing from merge"
        );
        assert!(
            objects.iter().any(|o| o.contains("rust")),
            "M2-covered fact missing from merge"
        );
    }

    /// The 2026-08-18 recall bug, as a regression test: a fact whose content
    /// answers the query but whose subject is a minority variant and whose
    /// importance is LOW must beat generic high-importance facts under the
    /// majority subject. Only a query-relevance arm (M5) can surface it —
    /// M2 matches subject "Mike" and sorts by importance, burying it.
    #[test]
    fn m5_fact_relevance_surfaces_low_importance_specific_fact() {
        let c = fresh_db();
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
             VALUES ('t1','Michael Rapadas','knows',
                     'Justin Frankel, who referred him to an employment lawyer',
                     0.5,'2026-06-04','2026-06-04',0)",
            [],
        )
        .unwrap();
        for i in 0..6 {
            c.execute(
                "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
                 VALUES (?1,'Mike','works-on',?2,0.9,'2026-06-04','2026-06-04',0)",
                rusqlite::params![format!("g{i}"), format!("generic project number {i}")],
            )
            .unwrap();
        }

        let r = assemble(
            &c,
            "Who referred Mike to an employment lawyer?",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );

        let m5_fired = r
            .per_move_stats
            .iter()
            .any(|s| s.move_id == MoveId::M5FactRelevance && s.fired);
        assert!(m5_fired, "M5 must fire on a content-matching query");
        let target_in = r.candidates.iter().any(|c| match &c.kind {
            CandidateKind::Fact(f) => f.object.contains("Justin Frankel"),
            _ => false,
        });
        assert!(
            target_in,
            "relevance-ranked fact must be injected despite low importance"
        );
    }

    /// Subject-only match: the widened facts_fts must make entity-name
    /// queries rankable when the name never appears in any object text.
    #[test]
    fn m5_finds_fact_via_subject_token() {
        let c = fresh_db();
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
             VALUES ('z1','Zwerk','is','an open-source model-agnostic agent platform',
                     0.5,'2026-06-04','2026-06-04',0)",
            [],
        )
        .unwrap();

        let r = assemble(
            &c,
            "Tell me about Zwerk please",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );
        let hit = r.candidates.iter().any(|c| match &c.kind {
            CandidateKind::Fact(f) => f.subject == "Zwerk",
            _ => false,
        });
        assert!(
            hit,
            "subject token must be searchable via widened facts_fts"
        );
    }

    /// "has"/"have" cue must reach the most common production predicate.
    #[test]
    fn predicate_cue_has_fires_m3() {
        let c = fresh_db();
        insert_fact(
            &c,
            "h1",
            "boi",
            "has",
            "a retention policy pruning old events",
            false,
        );
        let r = assemble(
            &c,
            "what retention does boi have",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );
        let m3_fired = r
            .per_move_stats
            .iter()
            .any(|s| s.move_id == MoveId::M3PredicateQuery && s.fired);
        assert!(m3_fired, "'have' must cue the 'has' predicate");
    }

    /// RED (task Tkmz6c46q — matching batch, diagnosis 2026-08-31 cause 3
    /// "camelCase predicates (knowsAbout) index as one token"): a fact whose
    /// predicate is camelCase is invisible to a query naming the split words.
    /// The facts FTS tokenizer (`porter unicode61`, schema.rs:83) indexes
    /// `knowsAbout` as the single token `knowsabout`, so neither `know` nor
    /// `about` matches it. Subject and object deliberately share NO token with
    /// the query, and the `know` cue maps to the DISTINCT predicate `knows`
    /// (M3 does exact `predicate = 'knows'`, assemble.rs:331, which never
    /// equals `knowsAbout`), so the ONLY path to this fact is an index-side
    /// camelCase split feeding M5's facts_fts arm.
    ///
    /// After the fix (split camelCase predicates into word tokens for FTS
    /// matching, with index-side normalization), the fact MUST appear in the
    /// assembled candidates. This is the `camelcase-reachable` verification.
    #[test]
    fn assemble_camelcase_predicate_reachable_by_split_words() {
        let c = fresh_db();
        insert_fact(
            &c,
            "kc1",
            "person:dana",
            "knowsAbout",
            "distributed consensus protocols",
            false,
        );

        // Query names the split words `know` and `about`; it contains nothing
        // that matches the subject slug `dana` or the object text, so retrieval
        // can only come from splitting the camelCase predicate index-side.
        let r = assemble(
            &c,
            "what do you know about this",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );
        let hit = r.candidates.iter().any(|cand| match &cand.kind {
            CandidateKind::Fact(f) => f.predicate == "knowsAbout",
            _ => false,
        });
        assert!(
            hit,
            "fact with camelCase predicate `knowsAbout` must be retrievable by a \
             query containing the split words `know`/`about` (needs index-side \
             camelCase token split feeding facts_fts)"
        );

        // Control (green now AND after the fix): a query naming NEITHER split
        // word must not surface the fact — pins that the split words are what
        // surface it, not some unrelated FTS widening.
        let ctrl = assemble(
            &c,
            "what is the weather forecast",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );
        let ctrl_hit = ctrl.candidates.iter().any(|cand| match &cand.kind {
            CandidateKind::Fact(f) => f.predicate == "knowsAbout",
            _ => false,
        });
        assert!(
            !ctrl_hit,
            "control: fact must NOT surface for a query naming neither split word"
        );
    }

    /// RED (task Tkmz6c46q — matching batch): the `blocked-by` predicate cue
    /// list carries only the verbs `block`/`blocked`/`blocking`, so the noun
    /// phrasing "who are the blockers" never cues M3 toward `blocked-by` facts.
    /// `predicate_cues` does exact `HashSet` membership (no stemming), so
    /// `blockers` is not caught by the existing verbs.
    ///
    /// After the fix (add `blocker`/`blockers` to the cue list), the cue must
    /// resolve to `blocked-by`.
    #[test]
    fn predicate_cue_blockers_maps_to_blocked_by() {
        let preds = predicate_cues("who are the blockers on hex");
        assert!(
            preds.contains(&"blocked-by"),
            "`blockers` must cue the `blocked-by` predicate; got {preds:?}"
        );
    }

    /// Entity detection must reach hyphen-delimited and multi-word subjects
    /// (task Tkmz6c46q, diagnosis 2026-08-31 cases fleet-coordinator,
    /// hex-v2-arch, "hex project"). Pre-fix, only the after-colon slug was
    /// split, so a subject like `fleet-coordinator` (no colon) or `hex project`
    /// (a space) was matchable only as ONE whole query token, which never
    /// occurs.
    #[test]
    fn entity_detection_reaches_hyphen_and_multiword_subjects() {
        let c = fresh_db();
        insert_fact(&c, "e1", "fleet-coordinator", "has", "three arms", false);
        insert_fact(&c, "e2", "hex project", "has", "a recall eval", false);
        insert_fact(&c, "e3", "hex-v2-arch", "has", "a merge stage", false);
        insert_fact(&c, "e4", "person:brand-lead", "avoids", "public pricing", false);

        let m = detect_entity_subjects(&c, "who owns the fleet coordinator rewrite");
        assert!(
            m.iter().any(|s| s == "fleet-coordinator"),
            "hyphen subject unreachable: {m:?}"
        );

        let m = detect_entity_subjects(&c, "what does the hex project track");
        assert!(
            m.iter().any(|s| s == "hex project"),
            "multi-word (space) subject unreachable: {m:?}"
        );

        let m = detect_entity_subjects(&c, "describe the arch of hex-v2-arch design");
        assert!(
            m.iter().any(|s| s == "hex-v2-arch"),
            "hyphen+version subject unreachable via a word piece: {m:?}"
        );

        let m = detect_entity_subjects(&c, "what restrictions bind the brand lead");
        assert!(
            m.iter().any(|s| s == "person:brand-lead"),
            "hyphen slug after a type prefix unreachable: {m:?}"
        );

        // The bare type prefix must NOT match every subject of that type.
        let m = detect_entity_subjects(&c, "who is this person anyway");
        assert!(
            !m.iter().any(|s| s == "person:brand-lead"),
            "the `person:` type prefix must not trigger an entity match: {m:?}"
        );
    }

    /// M2 must blend query relevance into its previously importance-only order
    /// (task Tkmz6c46q, diagnosis 2026-08-31 case b-brand-lead-restrictions):
    /// a low-importance fact that answers the query, ranked below the top-K by
    /// importance alone, must be re-surfaced. Tested directly on `m2_entity` so
    /// no other move can mask the effect.
    #[test]
    fn m2_blends_query_relevance_over_importance() {
        let c = fresh_db();
        // Seven generic HIGH-importance facts under the subject — enough to push
        // the relevant fact past the top-K window on importance alone.
        for i in 0..7 {
            c.execute(
                "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
                 VALUES (?1,'person:brand-lead','has',?2,0.9,'2026-06-04','2026-06-04',0)",
                rusqlite::params![format!("bl{i}"), format!("quarterly scheduling note number {i}")],
            )
            .unwrap();
        }
        // The query-relevant fact: LOW importance, but its object carries the
        // query term "restrictions".
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
             VALUES ('blx','person:brand-lead','avoids','discussing pricing restrictions with the press',0.3,'2026-06-04','2026-06-04',0)",
            [],
        )
        .unwrap();

        let cfg = RecallConfig::default();
        let query = "what restrictions bind the brand lead";
        let subjects = detect_entity_subjects(&c, query);
        let (fired, cands) = m2_entity(&c, query, false, &subjects, &cfg);
        assert!(fired, "M2 must fire on the brand-lead entity");
        let objs: Vec<&str> = cands
            .iter()
            .filter_map(|cand| match &cand.kind {
                CandidateKind::Fact(f) => Some(f.object.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            objs.iter().any(|o| o.contains("restrictions")),
            "relevance-blended M2 must surface the query-relevant low-importance \
             fact that importance-only ordering buries; got {objs:?}"
        );
        // The relevant fact must WIN the ordering, not merely appear.
        assert!(
            matches!(&cands[0].kind, CandidateKind::Fact(f) if f.object.contains("restrictions")),
            "the query-relevant fact must rank first under the blend; got {objs:?}"
        );
    }

    /// The facts tokenizer must keep 2-char alphanumeric tokens that carry a
    /// digit (v2), routed end-to-end through `assemble` (task Tkmz6c46q,
    /// diagnosis 2026-08-31 case c-14). "v2" is the ONLY query term shared with
    /// the fact, so pre-fix (sub-3-char tokens dropped) the fact is unreachable.
    #[test]
    fn assemble_two_char_versioned_token_reaches_fact() {
        let c = fresh_db();
        insert_fact(
            &c,
            "v1",
            "project:hex",
            "uses",
            "the v2 arch pipeline",
            false,
        );

        let r = assemble(&c, "what is the v2 design", false, MAX_CONTEXT_CHARS, None);
        let hit = r.candidates.iter().any(|cand| match &cand.kind {
            CandidateKind::Fact(f) => f.object.contains("v2 arch"),
            _ => false,
        });
        assert!(
            hit,
            "fact sharing only the 2-char token `v2` with the query must be \
             retrievable now that the facts tokenizer keeps digit-bearing 2-char tokens"
        );
    }

    /// Privacy: for_agent=true MUST exclude facts marked private from every
    /// facts move (M2/M3/M4).
    #[test]
    fn privacy_excludes_private_facts_when_for_agent() {
        let c = fresh_db();
        // Predicate cue "decided" → M3 will fire on this private fact.
        insert_fact(&c, "p1", "me/secret", "decided", "fire bob", true);
        insert_fact(&c, "p2", "project:hex", "decided", "use sqlite-vec", false);

        let r = assemble(
            &c,
            "what did we decide recently",
            true,
            MAX_CONTEXT_CHARS,
            None,
        );

        for cand in &r.candidates {
            if let CandidateKind::Fact(f) = &cand.kind {
                assert!(
                    !f.private,
                    "private fact {} leaked into for_agent=true result",
                    f.subject
                );
                assert_ne!(f.subject, "me/secret", "private subject leaked");
            }
        }
    }

    /// Per-move quota: M1 having a long candidate list MUST NOT crowd out a
    /// fired fact move's top candidate.
    #[test]
    fn per_move_quota_protects_fired_fact_moves_from_m1_domination() {
        let c = fresh_db();
        // Stuff M1 with many matching chunks.
        for i in 0..20 {
            insert_chunk(
                &c,
                &format!("docs/d{i}.md"),
                "schema decision memory layer schema decision",
                false,
            );
        }
        // One fact under a predicate cue.
        insert_fact(
            &c,
            "f1",
            "project:hex",
            "decided",
            "adopt the parallel-moves assembler",
            false,
        );

        let r = assemble(
            &c,
            "what did we decide about the schema",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );

        let m3_fired = r
            .per_move_stats
            .iter()
            .any(|s| s.move_id == MoveId::M3PredicateQuery && s.fired);
        assert!(m3_fired, "M3 should fire on the 'decide' cue");

        // The fact must survive M1's domination regardless of which fact move
        // (M5 relevance or M3 predicate) carries it into the merge.
        let fact_kept = r.candidates.iter().any(|c| match &c.kind {
            CandidateKind::Fact(f) => f.object.contains("parallel-moves assembler"),
            _ => false,
        });
        assert!(
            fact_kept,
            "the fact was crowded out by M1's long list — per-move quota missing"
        );
    }

    /// Merge contract — confidence formula AND char budget truncation.
    /// confidence = move_relevance(1.0 fired) * 1/(rank+1)
    /// budget truncation must cut the merge before exceeding the char budget.
    #[test]
    fn confidence_formula_and_budget_truncation() {
        let c = fresh_db();
        // Populate several chunks (each ~30 chars of content + path) so M1
        // alone would exceed a small budget if truncation were absent.
        for i in 0..10 {
            insert_chunk(
                &c,
                &format!("docs/m{i}.md"),
                "schema schema schema schema schema schema schema schema",
                false,
            );
        }

        // 1) confidence formula at rank 0 for a fired move must equal 1.0.
        let full = assemble(&c, "schema", false, MAX_CONTEXT_CHARS, None);
        let m1_top = full
            .candidates
            .iter()
            .find(|x| x.move_id == MoveId::M1ContentMatch)
            .expect("M1 should produce at least one candidate");
        assert_eq!(m1_top.rank_in_move, 0, "M1 top should be rank 0");
        assert!(m1_top.move_fired, "M1 always fires");
        assert!(
            (m1_top.confidence - 1.0).abs() < 1e-6,
            "rank-0 fired confidence must be 1.0, got {}",
            m1_top.confidence
        );
        // native_score must be carried separately (BM25 is negative in FTS5)
        // — i.e. it should NOT equal the confidence value.
        assert!(
            (m1_top.native_score as f32 - m1_top.confidence).abs() > 1e-6
                || m1_top.native_score == 0.0,
            "native_score must be carried separately from confidence"
        );

        // 2) Budget truncation: a tiny budget must force the merged result
        //    to stay at or under a small bound. (Floor candidate is allowed
        //    to push slightly over per the facet-coverage contract, so we
        //    assert the merge stopped well short of the unbounded length.)
        let tiny = assemble(&c, "schema", false, 100, None);
        assert!(
            tiny.candidates.len() < full.candidates.len(),
            "tiny budget ({} cands) did not truncate vs full ({} cands)",
            tiny.candidates.len(),
            full.candidates.len()
        );
    }

    /// Embedder-down: assemble() must not panic on a DB with no vector data
    /// / no available embedder, and must still return FTS+facts results.
    #[test]
    fn embedder_down_returns_results_without_panic() {
        let c = fresh_db();
        insert_chunk(&c, "docs/a.md", "memory schema assembler", false);
        insert_fact(&c, "f1", "project:hex", "decided", "ship it", false);

        // Should NOT panic even though no embedder is wired up here.
        let r = assemble(
            &c,
            "what did we decide about the memory schema",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );

        assert!(
            !r.candidates.is_empty(),
            "assemble returned no candidates even though FTS+facts are populated"
        );
    }

    #[test]
    fn render_candidates_joins_content() {
        let mk = |txt: &str| Candidate {
            kind: CandidateKind::Chunk(SearchResult {
                rowid: 0,
                source_path: "p".into(),
                heading: "h".into(),
                chunk_index: "0".into(),
                content: txt.into(),
                private: false,
                score: 0.0,
            }),
            move_id: MoveId::M1ContentMatch,
            move_fired: true,
            native_score: 0.0,
            rank_in_move: 0,
            confidence: 1.0,
            dedup_key: txt.into(),
        };
        let ctx = AssembledContext {
            candidates: vec![mk("alpha"), mk("beta")],
            per_move_stats: vec![],
        };
        let s = render_candidates(&ctx);
        assert!(s.contains("alpha") && s.contains("beta"));
    }

    // ── Task T28958xxp: object-aware, case-insensitive fact dedup key ────────
    // RED TESTS (write_red_tests phase). Pre-fix the fact dedup key is
    // `fact:{subject}|{predicate}` (line ~425): object-blind and case-sensitive.
    // The single shared seen-set (line ~566) spans BOTH the floor loop and the
    // round-robin loop, so at most ONE fact per (subject,predicate) pair can
    // enter assembled context. These pin the fix and MUST fail until it lands.

    /// EVICTION (verification `eviction-fixed`): two facts sharing subject and
    /// predicate but with DIFFERENT objects must BOTH appear in assembled
    /// output. Pre-fix only one survives because the object-blind key collides
    /// and the shared seen-set drops the second. This is the diagnosis's
    /// 246-of-1600 collision class (Mike+decided x152, Mike+works-on x115).
    #[test]
    fn dedup_two_facts_same_pair_different_objects_both_render() {
        let c = fresh_db();
        // Same subject + predicate, distinct objects. "decide" cues M3.
        insert_fact(
            &c,
            "d1",
            "project:hex",
            "decided",
            "use sqlite-vec for the vector store",
            false,
        );
        insert_fact(
            &c,
            "d2",
            "project:hex",
            "decided",
            "adopt the parallel-moves assembler",
            false,
        );

        let r = assemble(
            &c,
            "what did project hex decide about sqlite-vec and the assembler",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );

        // Retrieval precondition: M3 must have fetched BOTH facts as candidates,
        // so a failure below is a dedup eviction — NOT a retrieval shortfall.
        let m3 = r
            .per_move_stats
            .iter()
            .find(|s| s.move_id == MoveId::M3PredicateQuery)
            .expect("M3 stats present");
        assert!(m3.fired, "M3 must fire on the 'decide' cue");
        assert_eq!(
            m3.candidate_count, 2,
            "retrieval precondition: both same-pair facts must be fetched by M3"
        );

        // The real assertion: both distinct objects must survive the merge.
        let objects: Vec<&str> = r
            .candidates
            .iter()
            .filter_map(|c| match &c.kind {
                CandidateKind::Fact(f) => Some(f.object.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            objects.iter().any(|o| o.contains("sqlite-vec")),
            "first same-pair fact missing from merge"
        );
        assert!(
            objects.iter().any(|o| o.contains("parallel-moves assembler")),
            "second same-pair fact was evicted by the object-blind dedup key"
        );

        // And prove it in the ACTUAL rendered assembled output (the char budget
        // is applied while candidates are built — line ~632/679 — so this string
        // is the real context an agent would see, matching the `eviction-fixed`
        // verification's "assembled output" wording).
        let rendered = render_candidates(&r);
        assert!(
            rendered.contains("sqlite-vec") && rendered.contains("parallel-moves assembler"),
            "both distinct same-pair facts must appear in the rendered assembled output; got:\n{rendered}"
        );
    }

    /// CASE-VARIANT COLLAPSE: two facts that differ ONLY by case in subject and
    /// predicate (identical object) are TRUE duplicates and must collapse — i.e.
    /// share ONE dedup key so the seen-set drops the second. Pre-fix the key is
    /// case-sensitive (`Mike|works-on` != `mike|WORKS-ON`) so both leak.
    /// Asserts key EQUALITY only — never the key format (object-hash vs ULID is
    /// deliberately the implementer's choice).
    #[test]
    fn dedup_case_variant_true_duplicates_collapse() {
        let cfg = RecallConfig::default();
        let hits = vec![
            (
                crate::memory::recall::FactHit {
                    subject: "Mike".into(),
                    predicate: "works-on".into(),
                    object: "the fleet coordinator rewrite".into(),
                    importance: 0.8,
                    private: false,
                },
                0.8_f64,
            ),
            (
                crate::memory::recall::FactHit {
                    subject: "mike".into(),
                    predicate: "WORKS-ON".into(),
                    object: "the fleet coordinator rewrite".into(),
                    importance: 0.8,
                    private: false,
                },
                0.8_f64,
            ),
        ];
        let cands = facts_to_candidates(hits, MoveId::M3PredicateQuery, true, &cfg);
        assert_eq!(cands.len(), 2, "sanity: two candidates built");
        assert_eq!(
            cands[0].dedup_key, cands[1].dedup_key,
            "case-variant true duplicates must share a dedup key so they collapse"
        );
    }

    /// DISTINCTNESS (the other half of the key semantics): two facts with the
    /// SAME subject and predicate but DIFFERENT objects must NOT collapse — their
    /// dedup keys must differ. Pre-fix the object-blind key makes them equal, so
    /// `assert_ne!` fails now. Without this, an implementer could satisfy the
    /// collapse test by case-folding alone and leave the eviction bug intact.
    #[test]
    fn dedup_distinct_objects_keep_separate_keys() {
        let cfg = RecallConfig::default();
        let hits = vec![
            (
                crate::memory::recall::FactHit {
                    subject: "project:hex".into(),
                    predicate: "decided".into(),
                    object: "use sqlite-vec".into(),
                    importance: 0.8,
                    private: false,
                },
                0.8_f64,
            ),
            (
                crate::memory::recall::FactHit {
                    subject: "project:hex".into(),
                    predicate: "decided".into(),
                    object: "adopt the parallel-moves assembler".into(),
                    importance: 0.8,
                    private: false,
                },
                0.8_f64,
            ),
        ];
        let cands = facts_to_candidates(hits, MoveId::M3PredicateQuery, true, &cfg);
        assert_ne!(
            cands[0].dedup_key, cands[1].dedup_key,
            "distinct objects under one subject+predicate must keep separate keys"
        );
    }

    /// RED TEST (task T8s8bq3th — entity-scoped M3/M4 windows).
    ///
    /// When M2 detects an entity, M3's predicate fetch must be INTERSECTED with
    /// that entity's subject before the top-K window is applied — not run as a
    /// single GLOBAL per-predicate window. Pre-fix, `m3_predicate` runs
    /// `WHERE predicate = ?1 ORDER BY importance DESC LIMIT 6` with NO subject
    /// scoping, so when many other subjects hold higher-importance facts under
    /// the queried predicate, M3's window fills with THEIR facts and floods them
    /// into the assembled context — crowding the one entity the query actually
    /// named. The fix scopes M3 (and M4) to the M2-detected subjects.
    ///
    /// FIXTURE-CHOICE NOTE (fixture corrected during execute — see recall-fix
    /// package doc, T8s8bq3th "Deviation: red-test fixture correction"):
    /// the discriminator needs a predicate whose M3 CUE word does NOT also
    /// FTS-match the predicate, so the foreign facts reach the assembled output
    /// through M3's GLOBAL window ONLY (not through M5's relevance arm). The
    /// original `prefers` fixture failed this: `facts_fts` uses a `porter`
    /// tokenizer (schema.rs:83), so the cue word `preference` stems to `prefer`
    /// — identical to the stem of the indexed predicate `prefers` — and M5's
    /// FTS arm surfaced EVERY `prefers` fact via the predicate column, flooding
    /// foreign subjects regardless of M3. We use `blocked-by` cued by `blocker`
    /// instead: `blocker` is an EXACT `predicate_cues` entry (fires M3) but
    /// porter does NOT stem it to `blocked`/`block` (verified: `MATCH 'blocker'`
    /// returns nothing against predicate `blocked-by`), so M5 cannot reach the
    /// foreign facts and M3's global window is their only pre-fix path.
    ///
    /// Discriminator (why this is red pre-fix and green post-fix):
    /// the 10 non-target subjects each hold a HIGHER-importance `blocked-by`
    /// fact whose subject/object carry neither query term (`tara`, `blocker`),
    /// so M5 (fact relevance) and M2 (per-subject) never surface them — they can
    /// reach the assembled output through ONE path only, M3's global window.
    ///   * Pre-fix: M3's global top-6 by importance is 6 of those non-target
    ///     `blocked-by` facts (importance 0.90 > the target's 0.05), so
    ///     `blocked-by` facts from foreign subjects appear in the context. RED.
    ///   * Post-fix: M3 intersects with the detected subject `person:tara`, so
    ///     the ONLY `blocked-by` fact admitted is tara's — the target subject's
    ///     fact wins over the higher-importance facts from other subjects. GREEN.
    #[test]
    fn recall_entity_scoped_window_beats_flooding() {
        let c = fresh_db();
        // Innocuous M1 chunk — must NOT mention the entity or predicate.
        insert_chunk(&c, "docs/notes.md", "general workspace scheduling notes", false);

        // Target: tara's `blocked-by` fact. Deliberately LOW importance so a
        // global importance-ranked M3 window would never pick it over the
        // foreign subjects below — only entity scoping can.
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
             VALUES ('target','person:tara','blocked-by','the zzmarker editing style',
                     0.05,'2026-06-04','2026-06-04',0)",
            [],
        )
        .unwrap();

        // 10 OTHER subjects, each with a HIGHER-importance `blocked-by` fact
        // whose subject and object carry neither `tara` nor `blocker`. They
        // match no move but M3, so pre-fix M3's GLOBAL window floods them into
        // the context and post-fix entity scoping removes them entirely.
        // 10 > K=6 so at least six foreign `blocked-by` facts fill the pre-fix
        // window.
        for i in 0..10 {
            c.execute(
                "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
                 VALUES (?1,?2,'blocked-by',?3,0.90,'2026-06-04','2026-06-04',0)",
                rusqlite::params![
                    format!("other-{i}"),
                    format!("person:subject{i}"),
                    format!("the lightweight variant {i} choice")
                ],
            )
            .unwrap();
        }

        let r = assemble(&c, "who is tara's blocker", false, MAX_CONTEXT_CHARS, None);

        // Sanity: the two moves this test depends on must actually have fired,
        // so a red result means "window not entity-scoped", not "cue/entity missed".
        let m2_fired = r
            .per_move_stats
            .iter()
            .any(|s| s.move_id == MoveId::M2EntityFilter && s.fired);
        let m3_fired = r
            .per_move_stats
            .iter()
            .any(|s| s.move_id == MoveId::M3PredicateQuery && s.fired);
        assert!(m2_fired, "M2 must detect the entity `tara` for this test to be meaningful");
        assert!(m3_fired, "M3 must fire on the `blocker` cue for this test to be meaningful");

        // Subjects of every `blocked-by` fact that reached the assembled context.
        let blocked_by_subjects: Vec<String> = r
            .candidates
            .iter()
            .filter_map(|c| match &c.kind {
                CandidateKind::Fact(f) if f.predicate == "blocked-by" => Some(f.subject.clone()),
                _ => None,
            })
            .collect();

        // The target entity's fact must be present at all.
        assert!(
            blocked_by_subjects.iter().any(|s| s == "person:tara"),
            "the detected entity's `blocked-by` fact must reach the assembled context; \
             `blocked-by` subjects were: {:?}",
            blocked_by_subjects
        );

        // DISCRIMINATOR: with M2 entities present, M3's window must be scoped to
        // those subjects, so NO foreign subject's `blocked-by` fact may reach the
        // context. Pre-fix this fails — the global M3 window floods the
        // `person:subject-N` facts in over the entity the query named.
        assert!(
            blocked_by_subjects.iter().all(|s| s == "person:tara"),
            "entity-scoped M3 must not admit `blocked-by` facts from non-target subjects; \
             the target subject's fact must win over higher-importance facts from other \
             subjects sharing that predicate. Got `blocked-by` subjects: {:?}",
            blocked_by_subjects
        );
    }

    /// M4 must NOT fire on a lone generic `now` in an otherwise non-temporal
    /// question — that flooded the merge with same-day facts (task T8s8bq3th,
    /// diagnosis 2026-08-31 case a-mike-building). Strong temporal cues still
    /// fire; an explicit `right now` / `as of now` / `just now` phrase still
    /// fires (the `now` is genuinely temporal there).
    #[test]
    fn recall_m4_gate_ignores_lone_now() {
        // Lone `now` — filler, must NOT fire M4.
        assert!(
            !is_temporal("what is mike building now"),
            "a lone `now` in a non-temporal question must not fire M4"
        );
        assert!(
            !is_temporal("what does tara prefer now"),
            "a lone `now` must not fire M4"
        );
        // Strong cues still fire.
        assert!(
            is_temporal("what is the latest decision"),
            "strong cue `latest` must still fire M4"
        );
        assert!(
            is_temporal("what did we decide recently"),
            "strong cue `recently` must still fire M4"
        );
        assert!(
            is_temporal("what is the current focus"),
            "strong cue `current` must still fire M4"
        );
        // Explicit temporal phrase around `now` still fires.
        assert!(
            is_temporal("what is mike working on right now"),
            "an explicit `right now` phrase is genuinely temporal and must fire M4"
        );
    }

    /// End-to-end: a non-temporal question whose only temporal-looking word is a
    /// lone `now` must leave M4 unfired in the assembled pipeline, so same-day
    /// facts do not flood the merge (task T8s8bq3th).
    #[test]
    fn recall_lone_now_leaves_m4_unfired_in_assemble() {
        let c = fresh_db();
        insert_fact(&c, "n1", "project:hex", "has", "a recall eval", false);
        let r = assemble(&c, "what is happening now", false, MAX_CONTEXT_CHARS, None);
        let m4_fired = r
            .per_move_stats
            .iter()
            .any(|s| s.move_id == MoveId::M4TemporalSelect && s.fired);
        assert!(
            !m4_fired,
            "a lone `now` must not fire M4 in the assembled pipeline"
        );
    }
}
