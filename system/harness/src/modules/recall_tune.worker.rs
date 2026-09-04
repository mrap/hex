//! `hex-recall-tune` — weekly bounded recall-parameter auto-tuner (hill-climber
//! stage 1, spec Tzxmamhr8).
//!
//! Every Sunday 05:00 UTC this runs one bounded hill-climbing step over the
//! recall ranking parameters and, only when a candidate provably holds up on a
//! disjoint held-out slice, auto-lands it as the INSTANCE config
//! (`$HEX_DIR/.hex/config/recall.toml`). It NEVER commits to foundation develop
//! — the auto-land unit is a single archived, revertible file (design doc
//! 2026-08-19; deliberate deviation recorded in the spec).
//!
//! One run, in order:
//!   (i)   SKIP LOUDLY unless BOTH opt-in slices exist —
//!         `recall-cases-tuning.toml` and `recall-cases-heldout.toml`. Foundation
//!         ships to every instance; most never opt in, so absent slices are
//!         expected, never silent (a `recall_tune.skipped` event + stderr line).
//!   (ii)  Snapshot memory.db ONCE so every score in the run is frozen and
//!         comparable. Absent memory.db is LOUD (S6), not a benign skip.
//!   (viii)Revert check FIRST: re-score the live config on the fresh snapshot
//!         against the last win's recorded pre-change held-out score; on a
//!         regression, restore the archived `.prev` and log a reverted regret.
//!   (iii) Propose a bounded neighborhood of variants around the current config
//!         (hard cap `MAX_VARIANTS`). Proposal takes ONLY the current config —
//!         its signature has no cases path, so the held-out slice is
//!         structurally unable to influence it.
//!   (iv)  Score current + variants on the TUNING slice only; pick the best.
//!   (v)   Final gate on the HELD-OUT slice — the ONLY place a candidate is
//!         scored against held-out. The held-out path is carried in a
//!         `HeldoutPath` newtype that exposes no `Deref`/`AsRef`, so it cannot
//!         silently coerce into the tuning scorer.
//!   (vi)  Land iff held-out score >= current AND zero held-out regressions.
//!   (vii) On land: archive the previous `recall.toml` with a timestamped
//!         `.prev` suffix, atomically write the new one, append a `win_log` row.
//!         On reject: append a `regret_log` row.
//!
//! Every outcome (skip / revert / no-improvement / land / reject) emits
//! telemetry; every real failure returns `Err` (loud status=error row + stderr),
//! per SO S6. `win_log`/`regret_log` live in `memory.db` (migration in
//! `hex::memory::schema`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hex::memory::eval::CaseResult;
use hex::memory::provider::hex_root;
use hex::memory::recall_config::{config_path, RecallConfig};
use hex::memory::{db_path, eval, open_db, schema};
use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron — 05:00 UTC every Sunday (7-field: sec min hour dom mon dow year).
pub const CRON_WEEKLY_SUN_0500: &str = "0 0 5 * * SUN *";

/// Hard cap on variants scored in one run — the sweep is a bounded neighborhood
/// hill-climbing step, never an unbounded grid search. Raised from 12 to 24 for
/// the tuner-v2 widening (spec Thbgp5304): the sweep now also proposes two-knob
/// combinations, so the cap must admit the single-knob neighborhood (9) plus the
/// bounded two-knob cross-products (12) — a static 21 today, headroom to 24.
const MAX_VARIANTS: usize = 24;

/// Instance opt-in tuning slice.
fn tuning_cases_path(root: &Path) -> PathBuf {
    root.join(".hex/eval/recall-cases-tuning.toml")
}
/// Instance opt-in held-out slice (disjoint from tuning).
fn heldout_cases_path(root: &Path) -> PathBuf {
    root.join(".hex/eval/recall-cases-heldout.toml")
}

/// The held-out cases path, wrapped so the type system guarantees it reaches
/// ONLY gate-class code — never proposal generation or tuning scoring (spec
/// "heldout-isolated"). Deliberately exposes NO `Deref`/`AsRef<Path>`: the inner
/// path is reachable only through `path()`, which only the gate + revert
/// re-score call. A reviewer grepping `heldout` call sites sees exactly the two
/// deliberate gate-class uses, not a leak into the sweep.
struct HeldoutPath(PathBuf);
impl HeldoutPath {
    fn path(&self) -> &Path {
        &self.0
    }
}

/// The tuning cases path, wrapped symmetrically to [`HeldoutPath`]. The sweep
/// scorer ([`score_tuning`]) accepts ONLY this type, so a [`HeldoutPath`] is
/// type-incapable of reaching tuning scoring — held-out isolation on the
/// tuning side is enforced by the compiler, not by call-site discipline (spec
/// "heldout-isolated"). The two newtypes never coerce into one another.
struct TuningPath(PathBuf);
impl TuningPath {
    fn path(&self) -> &Path {
        &self.0
    }
}

/// Generate a bounded neighborhood of parameter variants around `current`.
///
/// Takes ONLY the current config — no cases path of any kind — so neither the
/// tuning nor (critically) the held-out slice can influence which variants are
/// proposed. This signature is the structural half of held-out isolation.
///
/// Tuner-v2 widening (spec Thbgp5304): the sweep proposes BOTH single-knob steps
/// (the original neighborhood) AND two-knob combinations across the three tunable
/// axes, so a fix that needs two parameters to move together is now reachable.
/// Every perturbation is computed relative to the CURRENT (base) value, so a
/// two-knob variant is an independent two-axis step, not a compounding one. The
/// enumeration is static (9 single-knob + 12 two-knob = 21) and stays under the
/// hard `MAX_VARIANTS` cap.
fn propose_variants(current: &RecallConfig) -> Vec<RecallConfig> {
    let mut out = Vec::new();

    // --- Single-knob neighborhood (unchanged from tuner-v1) ---
    // RRF fusion constant neighborhood (multiplicative, kept >= 1.0).
    for factor in [0.5_f64, 0.75, 1.5, 2.0] {
        let mut v = current.clone();
        v.rrf_k = (current.rrf_k * factor).max(1.0);
        out.push(v);
    }
    // M5 demotion strength: the not-fired relevance multiplier, in [0, 1].
    for delta in [-0.1_f32, 0.1, 0.2] {
        let mut v = current.clone();
        v.move_relevance.unfired = (current.move_relevance.unfired + delta).clamp(0.0, 1.0);
        out.push(v);
    }
    // Content-arm object emphasis (bm25 object-column weight), kept >= 0.
    for factor in [0.5_f64, 1.5] {
        let mut v = current.clone();
        v.arm_weights.content[2] = (current.arm_weights.content[2] * factor).max(0.0);
        out.push(v);
    }

    // --- Two-knob combinations (tuner-v2) ---
    // Bounded representative subsets per axis keep the three cross-products small
    // (2 x 2 each = 12 total) so single- plus two-knob stays under MAX_VARIANTS.
    let rrf_pair = [0.5_f64, 1.5];
    let unfired_pair = [-0.1_f32, 0.1];
    let content_pair = [0.5_f64, 1.5];

    // rrf_k x move_relevance.unfired
    for factor in rrf_pair {
        for delta in unfired_pair {
            let mut v = current.clone();
            v.rrf_k = (current.rrf_k * factor).max(1.0);
            v.move_relevance.unfired = (current.move_relevance.unfired + delta).clamp(0.0, 1.0);
            out.push(v);
        }
    }
    // rrf_k x arm_weights.content[2]
    for factor in rrf_pair {
        for cfactor in content_pair {
            let mut v = current.clone();
            v.rrf_k = (current.rrf_k * factor).max(1.0);
            v.arm_weights.content[2] = (current.arm_weights.content[2] * cfactor).max(0.0);
            out.push(v);
        }
    }
    // move_relevance.unfired x arm_weights.content[2]
    for delta in unfired_pair {
        for cfactor in content_pair {
            let mut v = current.clone();
            v.move_relevance.unfired = (current.move_relevance.unfired + delta).clamp(0.0, 1.0);
            v.arm_weights.content[2] = (current.arm_weights.content[2] * cfactor).max(0.0);
            out.push(v);
        }
    }

    out.truncate(MAX_VARIANTS);
    out
}

/// Score one config on the TUNING slice against the frozen snapshot; returns
/// the facts-hits scalar the sweep maximizes. Accepts a [`TuningPath`] ONLY —
/// a [`HeldoutPath`] cannot be passed here, so tuning scoring is
/// type-incapable of touching held-out data. Loud on failure (propagated as
/// `Err`).
fn score_tuning(snap_root: &Path, tuning: &TuningPath, cfg: &RecallConfig) -> Result<usize> {
    let results = eval::score_cases_with_config(snap_root, tuning.path(), cfg).map_err(|e| {
        anyhow::anyhow!(
            "recall-tune: tuning scoring {} failed: {e}",
            tuning.path().display()
        )
    })?;
    Ok(eval::facts_hits(&results))
}

/// Score one config on the HELD-OUT slice against the frozen snapshot,
/// returning the full per-case result map so the gate can read both facts-hits
/// and regressions. Accepts a [`HeldoutPath`] ONLY, and is called from exactly
/// the two gate-class sites — the revert re-score and the final land gate. This
/// is the sole reader of held-out data in the whole worker.
fn score_heldout(
    snap_root: &Path,
    heldout: &HeldoutPath,
    cfg: &RecallConfig,
) -> Result<BTreeMap<String, CaseResult>> {
    eval::score_cases_with_config(snap_root, heldout.path(), cfg).map_err(|e| {
        anyhow::anyhow!(
            "recall-tune: held-out scoring {} failed: {e}",
            heldout.path().display()
        )
    })
}

/// Copy the live memory.db into a fresh tempdir so every score in the run reads
/// identical, frozen data. Absent live DB is LOUD (S6): an instance that shipped
/// both opt-in slices but has no memory.db is inconsistent, not a benign skip.
/// The `-wal` sidecar is copied best-effort so committed-but-uncheckpointed
/// writes are included; `-shm` is rebuilt by SQLite on open.
/// Loud guard for the phantom-DB trap: `open_db` (`Connection::open`) CREATES
/// the file when absent. An absent memory.db on an opted-in instance is an
/// inconsistent install — a hard error, never a benign skip.
fn ensure_db_exists(dbp: &Path) -> Result<()> {
    if dbp.exists() {
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "recall-tune: memory.db absent at {} — case files present but no memory store",
        dbp.display()
    ))
}

fn snapshot_db(live_root: &Path) -> Result<(tempfile::TempDir, PathBuf)> {
    let src = db_path(live_root);
    if !src.exists() {
        return Err(anyhow::anyhow!(
            "recall-tune: memory.db absent at {} — cannot snapshot",
            src.display()
        ));
    }
    let tmp = tempfile::tempdir()
        .map_err(|e| anyhow::anyhow!("recall-tune: cannot create snapshot tempdir: {e}"))?;
    let dst = db_path(tmp.path());
    std::fs::create_dir_all(dst.parent().unwrap())
        .map_err(|e| anyhow::anyhow!("recall-tune: snapshot mkdir failed: {e}"))?;
    std::fs::copy(&src, &dst)
        .map_err(|e| anyhow::anyhow!("recall-tune: snapshot copy {} failed: {e}", src.display()))?;
    let src_wal = PathBuf::from(format!("{}-wal", src.display()));
    if src_wal.exists() {
        let dst_wal = PathBuf::from(format!("{}-wal", dst.display()));
        // Best-effort — a missing WAL tail only costs the snapshot the most
        // recent uncheckpointed rows, never correctness of relative scoring —
        // but the miss itself is said out loud (S6).
        if let Err(e) = std::fs::copy(&src_wal, &dst_wal) {
            eprintln!(
                "[recall-tune] WARN: WAL sidecar copy failed ({e}) — snapshot misses uncheckpointed rows"
            );
        }
    }
    let snap_root = tmp.path().to_path_buf();
    Ok((tmp, snap_root))
}

/// Append one ledger row. `table` is an internal literal (`"win_log"` /
/// `"regret_log"`), never user input, so the interpolation carries no injection
/// risk. `action` ∈ {"land","reject","reverted"}.
fn insert_ledger(
    conn: &rusqlite::Connection,
    table: &str,
    params_json: &str,
    tuning_score: i64,
    heldout_score: i64,
    action: &str,
    reverted: bool,
) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO {table}
                 (ts, params_json, tuning_score, heldout_score, action, reverted)
             VALUES (datetime('now'), ?1, ?2, ?3, ?4, ?5)"
        ),
        rusqlite::params![params_json, tuning_score, heldout_score, action, i64::from(reverted)],
    )?;
    Ok(())
}

/// Archive the current `recall.toml` with a timestamped `.prev` suffix, then
/// ATOMICALLY write `new_cfg`. Archive-then-rename ordering means a crash
/// mid-land leaves either the old config or the new one, never a partial. Returns
/// the archive path (always Some: a first-ever land archives the compiled
/// defaults) so the `win_log` row can point the later revert check at exactly
/// the file to restore.
fn land_config(root: &Path, new_cfg: &RecallConfig, stamp: &str) -> Result<Option<PathBuf>> {
    let live = config_path(root);
    let dir = live
        .parent()
        .ok_or_else(|| anyhow::anyhow!("recall-tune: config path has no parent"))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| anyhow::anyhow!("recall-tune: config mkdir failed: {e}"))?;

    // Archive the previous config FIRST, so the new-config rename below is the
    // last mutation and cannot orphan an un-archived predecessor. A first-ever
    // land (no live config) archives the COMPILED DEFAULTS instead: restoring
    // that archive is behaviorally identical to the pre-land state, so the
    // revert path exists from day one — and the first landing is exactly the
    // one with the least evidence behind it.
    let a = dir.join(format!("recall.toml.{stamp}.prev"));
    if live.exists() {
        std::fs::copy(&live, &a)
            .map_err(|e| anyhow::anyhow!("recall-tune: archive .prev failed: {e}"))?;
    } else {
        let body = toml::to_string(&RecallConfig::default())
            .map_err(|e| anyhow::anyhow!("recall-tune: serialize default .prev failed: {e}"))?;
        std::fs::write(&a, body)
            .map_err(|e| anyhow::anyhow!("recall-tune: write default .prev failed: {e}"))?;
    }
    let archive = Some(a);

    let body = toml::to_string(new_cfg)
        .map_err(|e| anyhow::anyhow!("recall-tune: serialize recall.toml failed: {e}"))?;
    let tmp = live.with_extension("toml.tmp");
    std::fs::write(&tmp, body)
        .map_err(|e| anyhow::anyhow!("recall-tune: write tmp config failed: {e}"))?;
    std::fs::rename(&tmp, &live)
        .map_err(|e| anyhow::anyhow!("recall-tune: rename config into place failed: {e}"))?;
    Ok(archive)
}

/// The most recent still-active landed win, read back from `win_log`.
struct LastWin {
    id: i64,
    /// The pre-change (prior config) held-out score, from `params_json` — the
    /// baseline the revert check re-measures the live config against.
    prev_heldout_score: i64,
    /// The archived previous config to restore on revert.
    prev_archive: PathBuf,
}

/// Read the newest un-reverted `action='land'` win that carries BOTH a
/// restorable archive and a recorded pre-change score. `None` when there is
/// nothing to check (first run, or the last win had no prior config to fall
/// back to).
fn last_active_win(conn: &rusqlite::Connection) -> Result<Option<LastWin>> {
    let row = conn.query_row(
        "SELECT id, params_json FROM win_log
             WHERE action = 'land' AND reverted = 0
             ORDER BY id DESC LIMIT 1",
        [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
    );
    let (id, pj) = match row {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!("recall-tune: win_log read failed: {e}")),
    };
    let v: serde_json::Value = serde_json::from_str(&pj)
        .map_err(|e| anyhow::anyhow!("recall-tune: win_log params_json parse failed: {e}"))?;
    let prev_archive = match v.get("prev_archive").and_then(|x| x.as_str()) {
        Some(s) => PathBuf::from(s),
        // Only legacy/malformed rows lack prev_archive (land_config now always
        // records one) → nothing revertible for such a row.
        None => return Ok(None),
    };
    let prev_heldout_score = v
        .get("prev_heldout_score")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    Ok(Some(LastWin {
        id,
        prev_heldout_score,
        prev_archive,
    }))
}

/// Whether a regression on fresh data warrants reverting the last win: the live
/// (landed) config now scores BELOW the pre-change baseline it was landed to
/// beat. Comparing against the PRE-change score (not the candidate's own landed
/// score) is deliberate — it distinguishes a genuine parameter regression from
/// ordinary score drift as facts accrue between runs.
fn should_revert(prev_heldout_score: i64, live_heldout_score: i64) -> bool {
    live_heldout_score < prev_heldout_score
}

/// The land decision — the ZERO-REGRESSION accept rule. A candidate lands iff
/// its held-out score does not drop AND it regresses NOT ONE held-out case.
///
/// This is a verbatim extraction of the previously-inline decision; the
/// tuner-v2 widening (spec Thbgp5304) enlarges only the CANDIDATE space
/// (`propose_variants`), never this gate — the spec exclusion pins the accept
/// rule as "observability only". Extracted as a named function so a unit test
/// can pin it byte-identical against accidental loosening (e.g. admitting a
/// score-neutral trade that costs one held-out case).
fn should_land(best_heldout: usize, current_heldout: usize, heldout_regressions: usize) -> bool {
    best_heldout >= current_heldout && heldout_regressions == 0
}

/// Build the observable `regret_log` payload for a REJECTED candidate, naming
/// the specific held-out cases it would have GAINED and LOST (spec Thbgp5304).
///
/// A vetoed candidate is almost always a trade — it wins some held-out cases and
/// loses others — and the zero-regression gate vetoes any trade. Recording bare
/// counts hides WHICH cases were traded, so a human reading the ledger cannot
/// tell a near-miss (one incidental loss) from a bad candidate (many). Derives
/// `lost` from the regressions and `gained` from the new-passes of
/// `eval::compare(best, current)` — same call, same orientation as the gate's
/// own regression count — and embeds the rejected `cfg` for provenance. The
/// caller emits these lists loudly (stderr + telemetry), per S6.
fn veto_record(
    cfg: &RecallConfig,
    best: &BTreeMap<String, CaseResult>,
    current: &BTreeMap<String, CaseResult>,
) -> serde_json::Value {
    let (lost, gained) = eval::compare(best, current);
    let reason = if lost.is_empty() {
        "heldout_not_better"
    } else {
        "heldout_regressions"
    };
    serde_json::json!({
        "config": cfg,
        "reason": reason,
        "heldout_regressions": lost.len(),
        "lost_cases": lost,
        "gained_cases": gained,
    })
}

/// Restore the archived `.prev` config over the live `recall.toml`, mark the win
/// reverted, and append a `reverted` `regret_log` row. Mechanical half of the
/// revert — the caller supplies the freshly measured `live_heldout_score`, so
/// this is unit-testable without the real recall path.
fn perform_revert(
    conn: &rusqlite::Connection,
    root: &Path,
    win: &LastWin,
    live_heldout_score: i64,
) -> Result<()> {
    let live = config_path(root);
    if !win.prev_archive.exists() {
        return Err(anyhow::anyhow!(
            "recall-tune: cannot revert — archived config {} is gone",
            win.prev_archive.display()
        ));
    }
    if let Some(dir) = live.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| anyhow::anyhow!("recall-tune: revert mkdir failed: {e}"))?;
    }
    // Atomic restore: copy archive to a tmp, then rename over the live file.
    let tmp = live.with_extension("toml.revert-tmp");
    std::fs::copy(&win.prev_archive, &tmp)
        .map_err(|e| anyhow::anyhow!("recall-tune: revert copy failed: {e}"))?;
    std::fs::rename(&tmp, &live)
        .map_err(|e| anyhow::anyhow!("recall-tune: revert rename failed: {e}"))?;

    conn.execute(
        "UPDATE win_log SET reverted = 1 WHERE id = ?1",
        rusqlite::params![win.id],
    )
    .map_err(|e| anyhow::anyhow!("recall-tune: mark win reverted failed: {e}"))?;

    let payload = serde_json::json!({
        "prev_archive": win.prev_archive.display().to_string(),
        "prev_heldout_score": win.prev_heldout_score,
        "reverted_win_id": win.id,
    })
    .to_string();
    insert_ledger(
        conn,
        "regret_log",
        &payload,
        0,
        live_heldout_score,
        "reverted",
        true,
    )
    .map_err(|e| anyhow::anyhow!("recall-tune: regret_log(reverted) insert failed: {e}"))?;
    Ok(())
}

fn run_recall_tune(_e: Event, ctx: Ctx) -> Result<()> {
    let root = hex_root();
    let tuning = tuning_cases_path(&root);
    let heldout_raw = heldout_cases_path(&root);

    // (i) SKIP LOUDLY unless BOTH opt-in slices exist — the instance opts in by
    // splitting its suite into a tuning slice and a disjoint held-out slice.
    if !tuning.exists() || !heldout_raw.exists() {
        eprintln!(
            "[recall-tune] SKIP: opt-in slices not both present ({} / {})",
            tuning.display(),
            heldout_raw.display()
        );
        ctx.emit(
            "recall_tune.skipped",
            serde_json::json!({
                "reason": "cases_absent",
                "tuning_path": tuning.display().to_string(),
                "heldout_path": heldout_raw.display().to_string(),
                "tuning_present": tuning.exists(),
                "heldout_present": heldout_raw.exists(),
            }),
        )?;
        return Ok(());
    }
    // Wrap both paths immediately in their newtypes. From here the held-out
    // path reaches only gate-class code (`score_heldout`), and the tuning path
    // reaches only the sweep (`score_tuning`) — neither can be substituted for
    // the other, by type.
    let heldout = HeldoutPath(heldout_raw);
    let tuning = TuningPath(tuning);

    // Open the ledger DB and ensure win_log/regret_log exist (atomic migration).
    // Existence is checked BEFORE open_db: `Connection::open` CREATES the file
    // when absent, so an instance with case files but no memory store would be
    // scored against a phantom empty DB and read as "nothing to improve"
    // forever (S6). Same guard as climber_digest.
    let dbp = db_path(&root);
    ensure_db_exists(&dbp)?;
    let conn = open_db(&dbp)
        .map_err(|e| anyhow::anyhow!("recall-tune: open memory.db {} failed: {e}", dbp.display()))?;
    schema::apply_tune_log_schema(&conn)
        .map_err(|e| anyhow::anyhow!("recall-tune: win_log/regret_log migration failed: {e}"))?;

    // (ii) Snapshot ONCE — every score below reads this frozen copy.
    let (snap, snap_root) = snapshot_db(&root)?;

    // (viii) Revert check for the PREVIOUS run's landed change, on this fresh snapshot.
    if let Some(win) = last_active_win(&conn)? {
        let live_cfg = RecallConfig::load(&root);
        // Gate-class held-out use #1: re-score the live config to check it still
        // beats the pre-change baseline it was landed to beat.
        let live_heldout = eval::facts_hits(&score_heldout(&snap_root, &heldout, &live_cfg)?);
        if should_revert(win.prev_heldout_score, live_heldout as i64) {
            perform_revert(&conn, &root, &win, live_heldout as i64)?;
            eprintln!(
                "[recall-tune] REVERTED win {}: live held-out {} < pre-change {}; restored {}",
                win.id,
                live_heldout,
                win.prev_heldout_score,
                win.prev_archive.display()
            );
            ctx.emit(
                "recall_tune.reverted",
                serde_json::json!({
                    "win_id": win.id,
                    "live_heldout_score": live_heldout,
                    "prev_heldout_score": win.prev_heldout_score,
                    "restored": win.prev_archive.display().to_string(),
                }),
            )?;
        }
    }

    // The live config AFTER any revert is the sweep's starting point.
    let current = RecallConfig::load(&root);

    // (iii) Bounded neighborhood — proposal never sees any cases path.
    let variants = propose_variants(&current);

    // (iv) Score current + each variant on the TUNING slice; pick the best.
    let current_tuning = score_tuning(&snap_root, &tuning, &current)?;
    let mut best = current.clone();
    let mut best_tuning = current_tuning;
    for v in &variants {
        let s = score_tuning(&snap_root, &tuning, v)?;
        if s > best_tuning {
            best_tuning = s;
            best = v.clone();
        }
    }

    // No variant beats current on tuning → nothing to gate. Loud no-op.
    if best_tuning <= current_tuning {
        drop(snap);
        eprintln!(
            "[recall-tune] no improvement: best tuning {best_tuning} <= current {current_tuning}"
        );
        ctx.emit(
            "recall_tune.no_improvement",
            serde_json::json!({
                "current_tuning_score": current_tuning,
                "best_tuning_score": best_tuning,
                "variants": variants.len(),
            }),
        )?;
        return Ok(());
    }

    // (v) Final gate on the HELD-OUT slice — gate-class held-out use #2, the ONLY
    // place a candidate is scored against held-out.
    let current_heldout_results = score_heldout(&snap_root, &heldout, &current)?;
    let best_heldout_results = score_heldout(&snap_root, &heldout, &best)?;
    let current_heldout = eval::facts_hits(&current_heldout_results);
    let best_heldout = eval::facts_hits(&best_heldout_results);
    // Held-out trade, named: `lost` = cases the current config hit that the
    // candidate misses (the regressions the gate vetoes on); `gained` = cases
    // the candidate newly hits. Same call/orientation the reject record reuses.
    let (heldout_lost, heldout_gained) =
        eval::compare(&best_heldout_results, &current_heldout_results);
    let heldout_regressions = heldout_lost.len();
    drop(snap);

    // (vi) Land iff the candidate does not lose on held-out AND regresses nothing
    // — the zero-regression accept rule, unchanged by the tuner-v2 widening.
    let land = should_land(best_heldout, current_heldout, heldout_regressions);
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();

    if land {
        // (vii) Archive .prev, atomically write the new config, append win_log.
        let archive = land_config(&root, &best, &stamp)?;
        let payload = serde_json::json!({
            "config": best,
            "prev_archive": archive.as_ref().map(|p| p.display().to_string()),
            // Pre-change baseline the NEXT run's revert check re-measures against.
            "prev_heldout_score": current_heldout,
        })
        .to_string();
        insert_ledger(
            &conn,
            "win_log",
            &payload,
            best_tuning as i64,
            best_heldout as i64,
            "land",
            false,
        )
        .map_err(|e| anyhow::anyhow!("recall-tune: win_log insert failed: {e}"))?;
        eprintln!(
            "[recall-tune] LANDED: tuning {current_tuning}->{best_tuning} held-out \
             {current_heldout}->{best_heldout}; archived {archive:?}"
        );
        ctx.emit(
            "recall_tune.landed",
            serde_json::json!({
                "current_tuning_score": current_tuning,
                "best_tuning_score": best_tuning,
                "current_heldout_score": current_heldout,
                "best_heldout_score": best_heldout,
                "heldout_regressions": heldout_regressions,
                "archive": archive.as_ref().map(|p| p.display().to_string()),
            }),
        )?;
    } else {
        // Reject → observable regret_log row: name the specific held-out cases
        // the candidate would have GAINED and LOST so a vetoed trade is legible
        // in the ledger and on stderr, not a bare regression count (spec
        // Thbgp5304 veto-observability; S6).
        let record = veto_record(&best, &best_heldout_results, &current_heldout_results);
        let reason = record["reason"]
            .as_str()
            .unwrap_or("heldout_regressions")
            .to_string();
        insert_ledger(
            &conn,
            "regret_log",
            &record.to_string(),
            best_tuning as i64,
            best_heldout as i64,
            "reject",
            false,
        )
        .map_err(|e| anyhow::anyhow!("recall-tune: regret_log(reject) insert failed: {e}"))?;
        eprintln!(
            "[recall-tune] REJECTED ({reason}): held-out {best_heldout} vs current \
             {current_heldout}, regressions {heldout_regressions}; \
             gained {heldout_gained:?} lost {heldout_lost:?}"
        );
        ctx.emit(
            "recall_tune.rejected",
            serde_json::json!({
                "reason": reason,
                "current_heldout_score": current_heldout,
                "best_heldout_score": best_heldout,
                "heldout_regressions": heldout_regressions,
                "gained_cases": heldout_gained,
                "lost_cases": heldout_lost,
            }),
        )?;
    }
    Ok(())
}

/// Build the `hex-recall-tune` worker.
pub fn worker() -> Worker {
    Worker::new("hex-recall-tune").on_cron_named("weekly", CRON_WEEKLY_SUN_0500, run_recall_tune)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The phantom-DB guard: absent memory.db is a hard error and the guard
    /// itself must not create the file (that is the whole point).
    #[test]
    fn ensure_db_exists_is_loud_when_absent_and_never_creates() {
        let tmp = tempfile::tempdir().unwrap();
        let dbp = tmp.path().join(".hex").join("memory.db");
        let err = ensure_db_exists(&dbp).unwrap_err().to_string();
        assert!(err.contains("memory.db absent"), "err: {err}");
        assert!(!dbp.exists());
        std::fs::create_dir_all(dbp.parent().unwrap()).unwrap();
        std::fs::write(&dbp, b"x").unwrap();
        assert!(ensure_db_exists(&dbp).is_ok());
    }

    /// A first-ever land (no live recall.toml) must still be revertible: the
    /// archive it records contains the compiled defaults, byte-identical to
    /// what land_config itself would serialize for them.
    #[test]
    fn first_land_archives_compiled_defaults_for_revert() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = land_config(tmp.path(), &nondefault_config(), "20260819T000000Z")
            .unwrap()
            .expect("first land must produce a revertible archive");
        let archived = std::fs::read_to_string(&archive).unwrap();
        assert_eq!(archived, toml::to_string(&RecallConfig::default()).unwrap());
        let live = std::fs::read_to_string(config_path(tmp.path())).unwrap();
        assert_eq!(live, toml::to_string(&nondefault_config()).unwrap());
    }

    fn nondefault_config() -> RecallConfig {
        let mut c = RecallConfig::default();
        c.rrf_k = 42.0;
        c.arm_weights.content = [3.0, 0.5, 4.0];
        c.arm_weights.entity = [5.0, 2.0, 0.5];
        c.move_relevance.fired = 0.8;
        c.move_relevance.unfired = 0.15;
        c
    }

    /// Held-out isolation (spec "heldout-isolated"), asserted BEHAVIORALLY, not
    /// by comment. The held-out file is made to NOT EXIST, then proposal
    /// generation and tuning scoring of the current config plus every variant
    /// are run. `score_cases_with_config` returns `CasesAbsent`/`Other` the
    /// instant it reads a missing cases file, so if either proposal generation
    /// or the tuning sweep had touched the held-out slice, these calls would
    /// fail — their success is the proof they read only tuning. The structural
    /// half is the type system: `propose_variants` takes no path at all, and
    /// `score_tuning` accepts only a `TuningPath`, so a `HeldoutPath` cannot be
    /// passed to either even by a future edit.
    #[test]
    fn proposal_and_tuning_never_read_heldout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // A real tuning fixture with one case — the ONLY cases file the sweep
        // is allowed to read.
        let tuning_file = root.join("recall-cases-tuning.toml");
        std::fs::write(
            &tuning_file,
            "[[cases]]\nid = \"c1\"\nquery = \"anything at all\"\nexpect = \"x\"\n",
        )
        .unwrap();
        let tuning = TuningPath(tuning_file);

        // The held-out path deliberately points at a file that does NOT exist:
        // any read of it is a loud `CasesAbsent` error.
        let heldout = HeldoutPath(root.join("recall-cases-heldout-ABSENT.toml"));
        assert!(
            !heldout.path().exists(),
            "the held-out fixture must be absent for this assertion to bite"
        );

        // Proposal generation takes only the config — no path can leak in.
        let base = RecallConfig::default();
        let variants = propose_variants(&base);
        assert!(!variants.is_empty(), "sweep must produce at least one variant");
        assert!(
            variants.len() <= MAX_VARIANTS,
            "variant count must respect the hard cap ({MAX_VARIANTS})"
        );
        // Every variant is a genuine neighbor (differs from the base somewhere).
        for v in &variants {
            let differs = v.rrf_k != base.rrf_k
                || v.move_relevance.unfired != base.move_relevance.unfired
                || v.arm_weights.content != base.arm_weights.content;
            assert!(differs, "each variant must perturb at least one parameter");
        }

        // Tuning scoring of the current config and every variant must SUCCEED
        // with the held-out file absent — proving the sweep read only tuning.
        score_tuning(root, &tuning, &base)
            .expect("tuning scoring of current must not read the (absent) held-out slice");
        for v in &variants {
            score_tuning(root, &tuning, v)
                .expect("variant tuning scoring must not read the (absent) held-out slice");
        }

        // Live tripwire: the absent held-out fixture MUST error when actually
        // read. This converts the sweep's success above from proof-by-comment
        // into proof-by-assertion — if `score_tuning` had silently touched the
        // held-out slice, it would have hit the same `CasesAbsent` error this
        // gate-class read does, and the loop above would have failed.
        assert!(
            score_heldout(root, &heldout, &base).is_err(),
            "the absent held-out fixture must be a live tripwire, else the sweep's success proves nothing"
        );
    }

    /// The win_log/regret_log migration creates both ledgers atomically, is
    /// idempotent, has the exact fixed column set, and a row round-trips.
    #[test]
    fn tune_log_migration_creates_ledgers_and_row_roundtrips() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::apply_tune_log_schema(&conn).unwrap();
        schema::apply_tune_log_schema(&conn).unwrap(); // idempotent

        for table in ["win_log", "regret_log"] {
            let cols: Vec<String> = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(std::result::Result::ok)
                .collect();
            for expected in &[
                "id",
                "ts",
                "params_json",
                "tuning_score",
                "heldout_score",
                "action",
                "reverted",
            ] {
                assert!(
                    cols.contains(&expected.to_string()),
                    "{table} missing column {expected}"
                );
            }
            assert_eq!(cols.len(), 7, "{table} must carry exactly the spec columns");
        }

        insert_ledger(&conn, "win_log", "{\"k\":1}", 5, 7, "land", false).unwrap();
        let (pj, ts, hs, act, rev): (String, i64, i64, String, i64) = conn
            .query_row(
                "SELECT params_json, tuning_score, heldout_score, action, reverted FROM win_log",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!((pj.as_str(), ts, hs, act.as_str(), rev), ("{\"k\":1}", 5, 7, "land", 0));
    }

    /// A landed config written to `recall.toml` must round-trip through the hot
    /// recall path's loader. Serialization drift would "land" a config that
    /// `RecallConfig::load` silently rejects (falling back to defaults) — the
    /// exact silent no-op this spec exists to kill. Also exercises the archive:
    /// a second land copies the prior config to a timestamped `.prev`.
    #[test]
    fn landed_config_roundtrips_through_loader_and_archives_prev() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // First land: no prior config → the compiled defaults are archived so
        // the revert path exists from day one; loader sees the exact values.
        let cfg = nondefault_config();
        let archive = land_config(root, &cfg, "20260819T050000Z").unwrap();
        let first_arch = archive.expect("first land archives the compiled defaults");
        assert_eq!(
            std::fs::read_to_string(&first_arch).unwrap(),
            toml::to_string(&RecallConfig::default()).unwrap(),
            "first-land archive must hold the compiled defaults"
        );
        let loaded = RecallConfig::load(root);
        assert_eq!(loaded.rrf_k, cfg.rrf_k);
        assert_eq!(loaded.arm_weights.content, cfg.arm_weights.content);
        assert_eq!(loaded.arm_weights.entity, cfg.arm_weights.entity);
        assert_eq!(loaded.move_relevance.fired, cfg.move_relevance.fired);
        assert_eq!(loaded.move_relevance.unfired, cfg.move_relevance.unfired);

        // Second land: the previous config is archived to a timestamped .prev.
        let mut cfg2 = RecallConfig::default();
        cfg2.rrf_k = 12.0;
        let archive2 = land_config(root, &cfg2, "20260826T050000Z").unwrap();
        let arch = archive2.expect("second land must archive the predecessor");
        assert!(arch.exists(), "archived .prev must exist on disk");
        assert!(
            arch.to_string_lossy().ends_with(".prev"),
            "archive must carry the .prev suffix"
        );
        // The archive holds the FIRST config; the live file now holds the second.
        let archived_body = std::fs::read_to_string(&arch).unwrap();
        let archived: RecallConfig = toml::from_str(&archived_body).unwrap();
        assert_eq!(archived.rrf_k, 42.0, "archive must preserve the prior config");
        assert_eq!(RecallConfig::load(root).rrf_k, 12.0, "live config is the new one");
    }

    /// should_revert compares the live score to the PRE-change baseline.
    #[test]
    fn should_revert_fires_only_below_pre_change_baseline() {
        assert!(should_revert(7, 6), "live below pre-change baseline → revert");
        assert!(!should_revert(7, 7), "equal → hold");
        assert!(!should_revert(7, 8), "above → hold");
    }

    /// A regression on the run after a landed change restores the archived .prev
    /// config AND records a `reverted` regret_log row (spec "revert-path").
    #[test]
    fn regression_after_land_restores_prev_and_logs_reverted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let cfg_dir = root.join(".hex/config");
        std::fs::create_dir_all(&cfg_dir).unwrap();

        // The archived PREV config (default: rrf_k = 60) that revert restores.
        let prev_archive = cfg_dir.join("recall.toml.20260819T050000Z.prev");
        std::fs::write(&prev_archive, toml::to_string(&RecallConfig::default()).unwrap()).unwrap();

        // The LIVE (landed) config: a distinct rrf_k so a restore is observable.
        let mut landed = RecallConfig::default();
        landed.rrf_k = 12.0;
        std::fs::write(config_path(root), toml::to_string(&landed).unwrap()).unwrap();

        // A win_log row recording the land: candidate held-out 8, pre-change 7.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::apply_tune_log_schema(&conn).unwrap();
        let payload = serde_json::json!({
            "config": landed,
            "prev_archive": prev_archive.display().to_string(),
            "prev_heldout_score": 7,
        })
        .to_string();
        insert_ledger(&conn, "win_log", &payload, 9, 8, "land", false).unwrap();

        // Start of the NEXT run: the live config now scores 6 on held-out —
        // below the pre-change baseline of 7 → regression.
        let win = last_active_win(&conn).unwrap().expect("an active win to check");
        assert_eq!(win.prev_heldout_score, 7);
        assert_eq!(win.prev_archive, prev_archive);
        let live_heldout = 6_i64;
        assert!(should_revert(win.prev_heldout_score, live_heldout));
        perform_revert(&conn, root, &win, live_heldout).unwrap();

        // The live config was restored to the archived predecessor (rrf_k = 60).
        assert_eq!(
            RecallConfig::load(root).rrf_k,
            60.0,
            "live recall.toml must be restored from .prev"
        );
        // The win is marked reverted so it is not re-checked next run.
        let reverted: i64 = conn
            .query_row("SELECT reverted FROM win_log WHERE id = ?1", [win.id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(reverted, 1, "win_log row must be marked reverted");
        // A reverted regret_log row was appended.
        let (act, rev, hs): (String, i64, i64) = conn
            .query_row(
                "SELECT action, reverted, heldout_score FROM regret_log",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((act.as_str(), rev, hs), ("reverted", 1, 6));
    }

    /// Tuner-v2 widening (spec Thbgp5304): the sweep now proposes MORE than the
    /// old nine single-knob variants, includes at least one two-knob combination,
    /// and still respects the raised hard cap. The exact count is pinned so a
    /// future enumeration change that would silently exceed the cap (and be
    /// truncated) is caught loudly instead.
    #[test]
    fn propose_variants_widens_to_two_knob_combinations_within_cap() {
        let base = RecallConfig::default();
        let variants = propose_variants(&base);

        // Widened past the old single-step-only space (was 9 single-knob variants).
        assert!(
            variants.len() > 9,
            "widened sweep must exceed the old 9 single-knob variants, got {}",
            variants.len()
        );
        // Cap raised to 24; the static enumeration is exactly 21 (9 single + 12 two-knob).
        assert_eq!(MAX_VARIANTS, 24, "cap must be 24 for the two-knob widening");
        assert!(
            variants.len() <= MAX_VARIANTS,
            "variant count must respect the hard cap ({MAX_VARIANTS}), got {}",
            variants.len()
        );
        assert_eq!(
            variants.len(),
            21,
            "enumeration is static at 21; if this grows past MAX_VARIANTS the truncate would silently drop variants"
        );

        // At least one variant must perturb TWO or more knobs at once — the whole
        // point of the widening.
        let two_knob = variants
            .iter()
            .filter(|v| {
                let d_rrf = u8::from(v.rrf_k != base.rrf_k);
                let d_unfired = u8::from(v.move_relevance.unfired != base.move_relevance.unfired);
                let d_content = u8::from(v.arm_weights.content != base.arm_weights.content);
                d_rrf + d_unfired + d_content >= 2
            })
            .count();
        assert!(
            two_knob >= 1,
            "at least one variant must move two or more knobs at once"
        );
    }

    /// The zero-regression accept rule, pinned byte-identical (spec Thbgp5304
    /// keeps the gate unchanged — observability only). The discriminating case
    /// `(8, 7, 1)` — held-out score UP but one case regressed — must still be
    /// vetoed; a loosened rule that accepted score-up trades would flip it.
    #[test]
    fn should_land_holds_the_zero_regression_accept_rule() {
        assert!(should_land(8, 7, 0), "clean improvement, zero regressions → land");
        assert!(
            should_land(7, 7, 0),
            "score-neutral with zero regressions → land"
        );
        assert!(
            !should_land(8, 7, 1),
            "any held-out regression vetoes the trade, even with the aggregate score up"
        );
        assert!(
            !should_land(6, 7, 0),
            "a held-out score drop vetoes even with zero regressions"
        );
    }

    /// Veto observability (spec Thbgp5304): a rejected candidate's regret record
    /// NAMES the held-out cases it gained and lost, not just a count. The fixture
    /// is an asymmetric trade — one case gained, one lost — so a gained/lost
    /// inversion is caught.
    #[test]
    fn veto_record_names_gained_and_lost_cases() {
        let hit = CaseResult {
            facts: true,
            anywhere: true,
        };
        let miss = CaseResult {
            facts: false,
            anywhere: false,
        };

        // current (baseline) held-out results.
        let mut current = BTreeMap::new();
        current.insert("keep".to_string(), hit); // both configs hold this
        current.insert("c-5".to_string(), hit); // current holds, candidate loses → LOST
        current.insert("hex-focus".to_string(), miss); // current misses, candidate gains → GAINED

        // candidate (best) held-out results: trades c-5 (lost) for hex-focus (gained).
        let mut best = BTreeMap::new();
        best.insert("keep".to_string(), hit);
        best.insert("c-5".to_string(), miss);
        best.insert("hex-focus".to_string(), hit);

        let rec = veto_record(&RecallConfig::default(), &best, &current);

        let lost: Vec<String> = rec["lost_cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let gained: Vec<String> = rec["gained_cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        assert_eq!(lost, vec!["c-5".to_string()], "the lost held-out case must be named");
        assert_eq!(
            gained,
            vec!["hex-focus".to_string()],
            "the gained held-out case must be named"
        );
        assert_eq!(
            rec["heldout_regressions"].as_u64().unwrap(),
            1,
            "regression count must equal the number of lost cases"
        );
        assert_eq!(
            rec["reason"].as_str().unwrap(),
            "heldout_regressions",
            "a trade with a loss is vetoed for heldout_regressions"
        );
        assert!(
            rec.get("config").is_some(),
            "the rejected config must be embedded for provenance"
        );
    }
}
