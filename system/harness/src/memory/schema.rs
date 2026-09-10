use rusqlite::{Connection, Result};

pub const PLAN2_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version    INTEGER PRIMARY KEY,
    applied_at TEXT
);

CREATE TABLE IF NOT EXISTS facts (
    id            TEXT PRIMARY KEY,
    subject       TEXT NOT NULL,
    predicate     TEXT NOT NULL,
    object        TEXT NOT NULL,
    importance    REAL NOT NULL DEFAULT 0.5,
    access_count  INTEGER NOT NULL DEFAULT 0,
    last_accessed TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    source_ref    TEXT,
    private       INTEGER NOT NULL DEFAULT 0,
    tombstone     INTEGER NOT NULL DEFAULT 0,
    embedding     BLOB,
    source_origin TEXT,
    effective_date TEXT,
    superseded_by TEXT,
    authority_status TEXT COLLATE BINARY NOT NULL DEFAULT 'unknown'
        CHECK (authority_status COLLATE BINARY IN ('current','historical','unknown'))
);
CREATE INDEX IF NOT EXISTS facts_subject_idx     ON facts(subject);
CREATE INDEX IF NOT EXISTS facts_predicate_idx   ON facts(predicate);
CREATE INDEX IF NOT EXISTS facts_tombstone_idx   ON facts(tombstone);
CREATE INDEX IF NOT EXISTS facts_dedup_idx       ON facts(subject, predicate);

CREATE TABLE IF NOT EXISTS fact_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    fact_id     TEXT NOT NULL,
    op          TEXT NOT NULL CHECK (op IN ('ADD','UPDATE','DELETE','FLAG')),
    prev_value  TEXT,
    new_value   TEXT,
    ts          TEXT NOT NULL,
    FOREIGN KEY (fact_id) REFERENCES facts(id)
);
CREATE INDEX IF NOT EXISTS fact_history_fact_idx ON fact_history(fact_id);

CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    date        TEXT NOT NULL,
    source_path TEXT NOT NULL UNIQUE,
    summary     TEXT,
    topic_id    TEXT
);

CREATE TABLE IF NOT EXISTS topics (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL UNIQUE,
    rollup_md         TEXT,
    last_consolidated TEXT
);

CREATE TABLE IF NOT EXISTS fact_topics (
    fact_id  TEXT NOT NULL,
    topic_id TEXT NOT NULL,
    PRIMARY KEY (fact_id, topic_id)
);

CREATE TABLE IF NOT EXISTS transcript_files (
    path                  TEXT PRIMARY KEY,
    last_offset           INTEGER NOT NULL DEFAULT 0,
    last_distilled_at     TEXT,
    consecutive_failures  INTEGER NOT NULL DEFAULT 0
);
"#;

pub const PLAN2_VEC_DDL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS facts_vec USING vec0(
    fact_id TEXT PRIMARY KEY,
    embedding FLOAT[768]
);
"#;

pub const PLAN2_FTS_DDL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    subject,
    predicate,
    object,
    content=facts,
    content_rowid=rowid,
    tokenize='porter unicode61'
);
CREATE TRIGGER IF NOT EXISTS facts_fts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, subject, predicate, object)
        VALUES (new.rowid, new.subject, new.predicate, new.object);
END;
CREATE TRIGGER IF NOT EXISTS facts_fts_ad AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, subject, predicate, object)
        VALUES('delete', old.rowid, old.subject, old.predicate, old.object);
END;
CREATE TRIGGER IF NOT EXISTS facts_fts_au AFTER UPDATE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, subject, predicate, object)
        VALUES('delete', old.rowid, old.subject, old.predicate, old.object);
    INSERT INTO facts_fts(rowid, subject, predicate, object)
        VALUES (new.rowid, new.subject, new.predicate, new.object);
END;
"#;

pub const MESSAGES_DDL: &str = "
CREATE TABLE IF NOT EXISTS messages (
    id          TEXT PRIMARY KEY,
    source      TEXT NOT NULL,
    kind        TEXT NOT NULL,
    body        TEXT,
    reply_to    TEXT,
    answer_json TEXT,
    prompt_json TEXT,
    resolved    INTEGER NOT NULL DEFAULT 0,
    ts          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_reply_to ON messages(reply_to);
";

pub fn apply_messages_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(MESSAGES_DDL)
}

/// Trend table for the recall eval — one row per `hex-eval-trend` cron run.
/// Columns mirror the eval's machine-readable summary so the trend is a
/// straight append: no scoring change, just a durable record of each night's
/// numbers. `baseline_present` is stored 0/1.
pub const EVAL_RUNS_DDL: &str = "
CREATE TABLE IF NOT EXISTS eval_runs (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    ts               TEXT NOT NULL,
    cases_total      INTEGER NOT NULL,
    facts_hits       INTEGER NOT NULL,
    anywhere_hits    INTEGER NOT NULL,
    regressions      INTEGER NOT NULL,
    baseline_present INTEGER NOT NULL,
    harness_version  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_eval_runs_ts ON eval_runs(ts);
";

/// Apply the `eval_runs` migration. A single `CREATE TABLE IF NOT EXISTS` DDL
/// batch is atomic and idempotent (same shape as `apply_messages_schema`), so
/// a partial or repeated apply can never leave a half-built table.
pub fn apply_eval_runs_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(EVAL_RUNS_DDL)
}

/// Auto-tuner ledgers (hill-climber stage 1, spec Tzxmamhr8). `win_log` records
/// every landed parameter change; `regret_log` records every rejected candidate
/// AND every later auto-revert. Both tables carry the identical column set the
/// spec fixes — `id, ts, params_json, tuning_score, heldout_score, action,
/// reverted` — so the weekly digest reads them uniformly. `params_json` is the
/// free-form payload (the winning `RecallConfig`, the archived `.prev` path, and
/// the pre-change held-out score the revert check re-measures against).
pub const TUNE_LOG_DDL: &str = "
CREATE TABLE IF NOT EXISTS win_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    ts            TEXT NOT NULL,
    params_json   TEXT NOT NULL,
    tuning_score  INTEGER NOT NULL,
    heldout_score INTEGER NOT NULL,
    action        TEXT NOT NULL,
    reverted      INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS regret_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    ts            TEXT NOT NULL,
    params_json   TEXT NOT NULL,
    tuning_score  INTEGER NOT NULL,
    heldout_score INTEGER NOT NULL,
    action        TEXT NOT NULL,
    reverted      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_win_log_ts    ON win_log(ts);
CREATE INDEX IF NOT EXISTS idx_regret_log_ts ON regret_log(ts);
";

/// Apply the `win_log`/`regret_log` migration. One `CREATE TABLE IF NOT EXISTS`
/// batch — atomic and idempotent, same shape as `apply_eval_runs_schema`, so a
/// partial or repeated apply can never leave a half-built ledger.
pub fn apply_tune_log_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(TUNE_LOG_DDL)
}

const AUTHORITY_SCHEMA_VERSION: i64 = 5;
const AUTHORITY_COLUMNS: [(&str, &str); 4] = [
    ("source_origin", "TEXT"),
    ("effective_date", "TEXT"),
    ("superseded_by", "TEXT"),
    (
        "authority_status",
        "TEXT COLLATE BINARY NOT NULL DEFAULT 'unknown' CHECK (authority_status COLLATE BINARY IN ('current','historical','unknown'))",
    ),
];

#[derive(Debug)]
struct ColumnDefinition {
    name: String,
    declared_type: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key: bool,
}

fn authority_schema_error(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_SCHEMA),
        Some(format!("authority schema: {}", message.into())),
    )
}

fn authority_columns(conn: &Connection) -> Result<Vec<ColumnDefinition>> {
    let mut statement = conn.prepare("PRAGMA table_info(facts)")?;
    let columns = statement
        .query_map([], |row| {
            Ok(ColumnDefinition {
                name: row.get(1)?,
                declared_type: row.get(2)?,
                not_null: row.get::<_, i64>(3)? != 0,
                default_value: row.get(4)?,
                primary_key: row.get::<_, i64>(5)? != 0,
            })
        })?
        .collect();
    columns
}

fn validate_authority_schema(conn: &Connection) -> Result<bool> {
    let columns = authority_columns(conn)?;
    let mut complete = true;
    for (name, _) in AUTHORITY_COLUMNS {
        let Some(column) = columns.iter().find(|column| column.name == name) else {
            complete = false;
            continue;
        };
        // table_info cannot prove an existing CHECK constraint. Accept only
        // its observable shape here, then validate every stored status below.
        // Newly added authority_status columns use the declared CHECK above.
        let compatible = if name == "authority_status" {
            column.declared_type.eq_ignore_ascii_case("TEXT")
                && column.not_null
                && column.default_value.as_deref() == Some("'unknown'")
                && !column.primary_key
        } else {
            column.declared_type.eq_ignore_ascii_case("TEXT")
                && !column.not_null
                && column.default_value.is_none()
                && !column.primary_key
        };
        if !compatible {
            return Err(authority_schema_error(format!(
                "incompatible column `{name}`: {column:?}"
            )));
        }
    }
    if columns
        .iter()
        .any(|column| column.name == "authority_status")
    {
        let invalid: i64 = conn.query_row(
            "SELECT COUNT(*) FROM facts
             WHERE authority_status IS NULL
                OR authority_status COLLATE BINARY
                   NOT IN ('current','historical','unknown')",
            [],
            |row| row.get(0),
        )?;
        if invalid != 0 {
            return Err(authority_schema_error(format!(
                "authority_status contains {invalid} invalid values"
            )));
        }
    }
    Ok(complete)
}

fn authority_version_present(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_version WHERE version = ?1)",
        [AUTHORITY_SCHEMA_VERSION],
        |row| row.get(0),
    )
}

fn apply_authority_schema_with_step<F>(conn: &Connection, after_add: &mut F) -> Result<()>
where
    F: FnMut(usize) -> Result<()>,
{
    if validate_authority_schema(conn)? && authority_version_present(conn)? {
        return Ok(());
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        if validate_authority_schema(conn)? && authority_version_present(conn)? {
            conn.execute_batch("COMMIT")?;
            return Ok(());
        }

        let mut added = 0;
        for (name, declaration) in AUTHORITY_COLUMNS {
            let present = authority_columns(conn)?
                .iter()
                .any(|column| column.name == name);
            if present {
                continue;
            }
            conn.execute_batch(&format!(
                "ALTER TABLE facts ADD COLUMN {name} {declaration}"
            ))?;
            added += 1;
            after_add(added)?;
        }

        if !validate_authority_schema(conn)? {
            return Err(authority_schema_error(
                "required columns remain missing after migration",
            ));
        }
        conn.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES (?1, datetime('now'))
             ON CONFLICT(version) DO NOTHING",
            [AUTHORITY_SCHEMA_VERSION],
        )?;
        conn.execute_batch("COMMIT")?;
        Ok(())
    })();

    if let Err(primary) = migration {
        if let Err(rollback) = conn.execute_batch("ROLLBACK") {
            eprintln!("[schema] authority migration rollback warning: {rollback}");
        }
        return Err(primary);
    }
    Ok(())
}

pub fn apply_authority_schema(conn: &Connection) -> Result<()> {
    apply_authority_schema_with_step(conn, &mut |_| Ok(()))
}

pub fn apply_plan2(conn: &Connection) -> Result<()> {
    conn.execute_batch(PLAN2_DDL)?;
    apply_authority_schema(conn)?;
    // Backfill: older DBs created before consecutive_failures was added still
    // need the column. ALTER TABLE in SQLite errors if the column already
    // exists, so we ignore that one specific error.
    if let Err(e) = conn.execute(
        "ALTER TABLE transcript_files ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0",
        [],
    ) {
        let msg = e.to_string();
        if !msg.contains("duplicate column") {
            // Loud — but tolerated, as the column may already be present in a
            // fresh schema.
            eprintln!("[schema] transcript_files.consecutive_failures backfill: {e}");
        }
    }
    conn.execute_batch(PLAN2_VEC_DDL)?;
    if facts_fts_needs_widening(conn)? {
        // One IMMEDIATE transaction: the write lock is taken up front, the
        // widening need is RE-checked under that lock (two fresh-process
        // openers race this path — the loser must see the winner's finished
        // table and no-op, not drop it again), and drop+recreate+rebuild
        // commit atomically so no crash can leave the index dropped or
        // empty. On any error the guard rolls back to the old, still-
        // searchable table and the next open retries.
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let migrate = || -> Result<bool> {
            if !facts_fts_needs_widening(conn)? {
                return Ok(false);
            }
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS facts_fts_ai;
                 DROP TRIGGER IF EXISTS facts_fts_ad;
                 DROP TRIGGER IF EXISTS facts_fts_au;
                 DROP TABLE IF EXISTS facts_fts;",
            )?;
            conn.execute_batch(PLAN2_FTS_DDL)?;
            // External-content fts5: repopulate the 3-column index from facts.
            conn.execute("INSERT INTO facts_fts(facts_fts) VALUES('rebuild')", [])?;
            Ok(true)
        };
        match migrate() {
            Ok(did) => {
                conn.execute_batch("COMMIT")?;
                if did {
                    eprintln!("[schema] facts_fts widened to subject+predicate+object and rebuilt");
                }
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
    } else {
        conn.execute_batch(PLAN2_FTS_DDL)?;
    }
    conn.execute(
        "INSERT INTO schema_version (version, applied_at) VALUES (4, datetime('now'))
         ON CONFLICT(version) DO NOTHING",
        [],
    )?;
    Ok(())
}

/// Pre-2026-08 instances carry an object-only facts_fts, which makes any query
/// naming a subject or predicate structurally invisible to relevance ranking.
/// True when that shape is present and the widening migration must run.
fn facts_fts_needs_widening(conn: &Connection) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='facts_fts'",
        [],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Ok(false);
    }
    let has_subject: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('facts_fts') WHERE name='subject'",
        [],
        |r| r.get(0),
    )?;
    Ok(has_subject == 0)
}

/// Create the minimal Plan 1 schema baseline needed by tests that exercise Plan 2.
pub fn apply_plan1_baseline_for_test(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version    INTEGER PRIMARY KEY,
            applied_at TEXT
        );",
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO schema_version VALUES (3, datetime('now'))",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type HistoryState = (i64, String, String, Option<String>, Option<String>, String);

    #[test]
    fn migration_creates_all_plan2_tables() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        for expected in &[
            "facts",
            "fact_history",
            "sessions",
            "topics",
            "fact_topics",
            "transcript_files",
        ] {
            assert!(
                tables.contains(&expected.to_string()),
                "missing table: {expected}"
            );
        }
    }

    #[test]
    fn apply_plan2_idempotent_without_preexisting_schema_version() {
        // Simulates a production DB that came from Plan 1 without a schema_version table.
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        // No schema_version table created — bare DB, like a real Plan 1 production DB.
        apply_plan2(&conn).unwrap();
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            version, 5,
            "schema_version should record version=5 after apply_plan2"
        );
    }

    /// A pre-widening DB (object-only facts_fts + old triggers) must be
    /// migrated in place: subject tokens become searchable, existing rows are
    /// re-indexed, and the recreated triggers keep new inserts in sync.
    #[test]
    fn facts_fts_widening_migrates_object_only_index() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        conn.execute_batch(PLAN2_DDL).unwrap();
        // Old shape: object-only external-content fts + object-only triggers.
        conn.execute_batch(
            "CREATE VIRTUAL TABLE facts_fts USING fts5(
                object, content=facts, content_rowid=rowid,
                tokenize='porter unicode61'
            );
            CREATE TRIGGER facts_fts_ai AFTER INSERT ON facts BEGIN
                INSERT INTO facts_fts(rowid, object) VALUES (new.rowid, new.object);
            END;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,created_at,updated_at)
             VALUES ('f1','Asterfall','is','an agent platform','2026-01-01','2026-01-01')",
            [],
        )
        .unwrap();
        // Pre-migration: subject token invisible.
        let pre: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'asterfall'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pre, 0, "old index should not match subject tokens");

        apply_plan2(&conn).unwrap();

        let post: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'asterfall'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            post, 1,
            "widened index must match subject tokens after rebuild"
        );

        // Recreated trigger keeps new inserts searchable by subject.
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,created_at,updated_at)
             VALUES ('f2','Mossvale','is','a game','2026-01-01','2026-01-01')",
            [],
        )
        .unwrap();
        let trig: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'mossvale'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(trig, 1, "post-migration insert trigger must index subject");

        // Idempotent: second apply must not drop/rebuild again or error.
        apply_plan2(&conn).unwrap();
    }

    #[test]
    fn tombstone_requires_zero_access_and_age_over_threshold() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap();

        let col_check: Vec<String> = conn
            .prepare("PRAGMA table_info(facts)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(col_check.contains(&"access_count".to_string()));
        assert!(col_check.contains(&"tombstone".to_string()));
    }

    // ---------------------------------------------------------------------
    // Authority + validity metadata (authority checkpoint).
    //
    // Audit finding 1 ("Memory returns obsolete operational instructions"): the
    // facts schema carries recording timestamps and a source reference, but NO
    // first-class validity interval or authority level, so a superseded
    // historical instruction is retrieved and injected as a current fact. This
    // task makes memory authority explicit by adding four ADDITIVE, COMPATIBLE
    // columns to `facts` (carried through retrieval by the recall task
    // and consumed by the evaluation task):
    //   * source_origin    — provenance / authority origin of the source.
    //   * effective_date   — when the fact became effective. NEVER fabricated
    //                        from updated_at; missing metadata stays unknown.
    //   * superseded_by    — id of the fact that supersedes this one
    //                        (validity / supersession); NULL = not superseded.
    //   * authority_status — 'current' | 'historical' | 'unknown'. Missing
    //                        metadata stays 'unknown', NEVER 'current'.
    //
    // These tests are RED until the migration lands. They exercise only the
    // existing public API (apply_plan2 / open_db / raw SQL / PRAGMA), so a
    // missing column surfaces as a clean runtime assertion failure ("no such
    // column: authority_status") rather than a build break that would mask the
    // rest of the memory suite. Fixtures use synthetic identities only (Asterfall /
    // Mossvale / 2026-01-01) — no personal data or instance-specific dates.
    //
    // NOTE FOR EXECUTE: the portable explicit source registry (config-file
    // loader, no DB required — cf. recall_config.rs) has no compile-safe red in
    // this phase; its behavior is owned by the implementation phase. Required
    // invariants: an absent registry => unknown / fail-closed (never
    // "newest = current"); it resolves with NO memory.db present; a private or
    // unknown source is excluded when for_agent. If the migration bumps
    // schema_version past 4, the existing `version == 4` assertion in
    // `apply_plan2_idempotent_without_preexisting_schema_version` must be
    // updated as a VISIBLE, reviewed contract change (not a silent edit / not a
    // weakening).

    /// The four authority/validity columns MUST be present on `facts` after the
    /// idempotent Plan-2 migration.
    #[test]
    fn authority_columns_present_after_migration() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap();

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(facts)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        for expected in &[
            "source_origin",
            "effective_date",
            "superseded_by",
            "authority_status",
        ] {
            assert!(
                cols.contains(&expected.to_string()),
                "facts must gain authority column `{expected}` (present: {cols:?})"
            );
        }
    }

    /// Migrating a PRE-CHANGE database (no authority columns) must be additive:
    /// every original fact, its history, its FTS rowid, its vector, source_ref,
    /// private and tombstone flags survive, AND the new authority metadata
    /// defaults to UNKNOWN — never fabricated from updated_at, never 'current'.
    #[test]
    fn authority_migration_preserves_legacy_data_and_defaults_unknown() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();

        // Hand-write the LEGACY (pre-authority) facts + fact_history shape so
        // this stays a genuine migrate-from-old-data fixture: reusing PLAN2_DDL
        // would pick up the new columns once they land and defeat the test.
        conn.execute_batch(
            "CREATE TABLE facts (
                id            TEXT PRIMARY KEY,
                subject       TEXT NOT NULL,
                predicate     TEXT NOT NULL,
                object        TEXT NOT NULL,
                importance    REAL NOT NULL DEFAULT 0.5,
                access_count  INTEGER NOT NULL DEFAULT 0,
                last_accessed TEXT,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                source_ref    TEXT,
                private       INTEGER NOT NULL DEFAULT 0,
                tombstone     INTEGER NOT NULL DEFAULT 0,
                embedding     BLOB
            );
            CREATE TABLE fact_history (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                fact_id    TEXT NOT NULL,
                op         TEXT NOT NULL CHECK (op IN ('ADD','UPDATE','DELETE','FLAG')),
                prev_value TEXT,
                new_value  TEXT,
                ts         TEXT NOT NULL,
                FOREIGN KEY (fact_id) REFERENCES facts(id)
            );",
        )
        .unwrap();
        conn.execute_batch(PLAN2_VEC_DDL).unwrap();
        conn.execute_batch(PLAN2_FTS_DDL).unwrap();

        // Seed one PRIVATE legacy fact with a source_ref and a real updated_at.
        conn.execute(
            "INSERT INTO facts
               (id,subject,predicate,object,importance,created_at,updated_at,source_ref,private,tombstone)
             VALUES
               ('f_legacy','Asterfall','deploy-command','run asterfallctl ship',0.7,
                '2026-01-01','2026-02-02','sources/asterfall-runbook.md',1,0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fact_history (fact_id,op,new_value,ts)
             VALUES ('f_legacy','ADD','run asterfallctl ship','2026-01-01')",
            [],
        )
        .unwrap();
        // A fact vector so a bad table-rebuild (which repoints/loses it) is caught.
        crate::memory::vector::insert_fact_vec(&conn, "f_legacy", &[0.0f32; 768]).unwrap();

        // Record the pre-migration rowid: facts_fts is external-content
        // (content_rowid=rowid), so a drop-and-recreate of `facts` would
        // silently repoint the index. An additive ALTER keeps the rowid stable.
        let rowid_pre: i64 = conn
            .query_row("SELECT rowid FROM facts WHERE id='f_legacy'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let fts_pre: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'asterfall'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            fts_pre, 1,
            "legacy fixture should be FTS-searchable by subject"
        );

        // Run the migration.
        apply_plan2(&conn).unwrap();

        // --- Original data preserved verbatim ---
        let (subject, object, source_ref, private, tombstone): (String, String, String, i64, i64) =
            conn.query_row(
                "SELECT subject,object,source_ref,private,tombstone FROM facts WHERE id='f_legacy'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(subject, "Asterfall");
        assert_eq!(object, "run asterfallctl ship");
        assert_eq!(
            source_ref, "sources/asterfall-runbook.md",
            "source_ref must survive migration"
        );
        assert_eq!(private, 1, "private flag must survive migration");
        assert_eq!(tombstone, 0, "tombstone flag must survive migration");

        let hist: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fact_history WHERE fact_id='f_legacy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hist, 1, "fact_history rows must survive migration");

        let rowid_post: i64 = conn
            .query_row("SELECT rowid FROM facts WHERE id='f_legacy'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            rowid_post, rowid_pre,
            "rowid must be stable (external-content FTS join key)"
        );
        let fts_post: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'asterfall'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            fts_post, 1,
            "FTS must still match the subject token after migration"
        );

        let vec_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_vec WHERE fact_id='f_legacy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(vec_rows, 1, "fact vector must survive migration");

        // --- New authority metadata defaults to UNKNOWN (never fabricated) ---
        let (status, effective, origin, superseded): (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT authority_status,effective_date,source_origin,superseded_by
                   FROM facts WHERE id='f_legacy'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            status, "unknown",
            "a migrated legacy fact must be UNKNOWN authority, never 'current'"
        );
        assert!(
            effective.is_none(),
            "effective_date must stay NULL — never fabricated from updated_at (got {effective:?})"
        );
        assert!(
            origin.is_none(),
            "source_origin must stay unknown/NULL for a legacy fact"
        );
        assert!(superseded.is_none(), "a lone legacy fact is not superseded");
    }

    /// Supersession / conflict: two facts sharing subject+predicate can be
    /// marked so the superseded one is HISTORICAL (retained, still queryable)
    /// while the replacement is CURRENT — the schema must represent this without
    /// deleting history.
    #[test]
    fn superseded_fact_is_historical_not_deleted() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap();

        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('f_old','Asterfall','deploy-command','run old-deploy.sh',0.5,'2026-01-01','2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('f_new','Asterfall','deploy-command','run asterfallctl ship',0.5,'2026-03-01','2026-03-01')",
            [],
        )
        .unwrap();

        // Mark the new fact current; mark the old fact historical and superseded.
        conn.execute(
            "UPDATE facts SET authority_status='current' WHERE id='f_new'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE facts SET authority_status='historical', superseded_by='f_new' WHERE id='f_old'",
            [],
        )
        .unwrap();

        // The current instruction is the new one only.
        let current: Vec<String> = conn
            .prepare(
                "SELECT id FROM facts
                   WHERE subject='Asterfall' AND predicate='deploy-command'
                     AND authority_status='current'",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            current,
            vec!["f_new".to_string()],
            "only the replacement is current"
        );

        // History is retained and queryable, not deleted.
        let historical: (String, String) = conn
            .query_row(
                "SELECT id, superseded_by FROM facts WHERE authority_status='historical'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            historical.0, "f_old",
            "the superseded fact stays queryable as history"
        );
        assert_eq!(
            historical.1, "f_new",
            "the supersession link points to the replacement"
        );

        // Both rows still exist — supersession must not delete history.
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts WHERE subject='Asterfall' AND predicate='deploy-command'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            total, 2,
            "supersession retains both the current and historical rows"
        );
    }

    /// "Compatible": the existing `INSERT INTO facts (id,subject,predicate,
    /// object,...)` call sites across recall.rs / consolidate.rs / vector.rs
    /// name NO authority columns. After migration they must keep working and
    /// produce UNKNOWN authority — a bare write is never silently promoted to
    /// 'current'.
    #[test]
    fn legacy_insert_path_defaults_to_unknown_authority() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap();

        // Exact legacy column list used by the existing write paths.
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('f_bare','Mossvale','is','a game',0.5,'2026-01-01','2026-01-01')",
            [],
        )
        .unwrap();

        let (status, effective, origin): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT authority_status,effective_date,source_origin FROM facts WHERE id='f_bare'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            status, "unknown",
            "a bare legacy write must default to UNKNOWN authority"
        );
        assert!(
            effective.is_none(),
            "a bare legacy write must not fabricate an effective_date"
        );
        assert!(
            origin.is_none(),
            "a bare legacy write has unknown source_origin"
        );
    }

    /// The authority migration must also run through the real `open_db` entry
    /// point on an on-disk database. `open_db` swallows migration errors with a
    /// warning (mod.rs), so a column check here catches a migration that fails
    /// silently on a real file rather than only in-memory.
    #[test]
    fn open_db_applies_authority_migration_on_disk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = crate::memory::db_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = crate::memory::open_db(&path).unwrap();

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(facts)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        for expected in &[
            "source_origin",
            "effective_date",
            "superseded_by",
            "authority_status",
        ] {
            assert!(
                cols.contains(&expected.to_string()),
                "open_db must apply the authority migration on disk; missing `{expected}` (present: {cols:?})"
            );
        }
    }

    /// Privacy through the authority migration. A PRIVATE legacy fact must keep
    /// `private=1` across the additive migration AND must NOT be silently
    /// promoted to 'current' authority: a private, provenance-unknown fact stays
    /// UNKNOWN (fail-closed), never surfaced as a current instruction. This is
    /// the compile-safe, schema-level privacy red for the "preserve private
    /// filtering / fail closed for private/unknown sources" contract. The
    /// registry-level `for_agent` fail-closed resolution (reading authority
    /// directly with NO memory.db, excluding private/unknown sources) has no
    /// compile-safe red in this phase and is owned by the execute phase.
    #[test]
    fn private_fact_survives_authority_migration_and_is_not_promoted_to_current() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();

        // Hand-written LEGACY (pre-authority) shape — no authority columns — so
        // this stays a genuine migrate-from-old-data privacy fixture.
        conn.execute_batch(
            "CREATE TABLE facts (
                id            TEXT PRIMARY KEY,
                subject       TEXT NOT NULL,
                predicate     TEXT NOT NULL,
                object        TEXT NOT NULL,
                importance    REAL NOT NULL DEFAULT 0.5,
                access_count  INTEGER NOT NULL DEFAULT 0,
                last_accessed TEXT,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                source_ref    TEXT,
                private       INTEGER NOT NULL DEFAULT 0,
                tombstone     INTEGER NOT NULL DEFAULT 0,
                embedding     BLOB
            );
            CREATE TABLE fact_history (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                fact_id    TEXT NOT NULL,
                op         TEXT NOT NULL CHECK (op IN ('ADD','UPDATE','DELETE','FLAG')),
                prev_value TEXT,
                new_value  TEXT,
                ts         TEXT NOT NULL,
                FOREIGN KEY (fact_id) REFERENCES facts(id)
            );",
        )
        .unwrap();
        conn.execute_batch(PLAN2_VEC_DDL).unwrap();
        conn.execute_batch(PLAN2_FTS_DDL).unwrap();

        // A private legacy fact with a real updated_at (synthetic identity, no
        // credential value): private=1, provenance unrecorded.
        conn.execute(
            "INSERT INTO facts
               (id,subject,predicate,object,importance,created_at,updated_at,source_ref,private,tombstone)
             VALUES
               ('f_priv','Asterfall','internal-runbook','internal-only deploy step',0.5,
                '2026-01-01','2026-02-02','sources/asterfall-private-notes.md',1,0)",
            [],
        )
        .unwrap();

        apply_plan2(&conn).unwrap();

        let (private, status): (i64, String) = conn
            .query_row(
                "SELECT private,authority_status FROM facts WHERE id='f_priv'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            private, 1,
            "the private flag must survive the additive authority migration"
        );
        assert_eq!(
            status, "unknown",
            "a private, provenance-unknown legacy fact must stay UNKNOWN, never promoted to 'current'"
        );
    }

    #[allow(dead_code)] // Derived equality reads every field in this all-state oracle.
    #[derive(Debug, PartialEq)]
    struct FactState {
        rowid: i64,
        id: String,
        subject: String,
        predicate: String,
        object: String,
        importance: f64,
        access_count: i64,
        last_accessed: Option<String>,
        created_at: String,
        updated_at: String,
        source_ref: Option<String>,
        private: i64,
        tombstone: i64,
        embedding: Option<Vec<u8>>,
        source_origin: Option<String>,
        effective_date: Option<String>,
        superseded_by: Option<String>,
        authority_status: String,
    }

    #[allow(dead_code)] // Derived equality reads fields not asserted separately.
    #[derive(Debug, PartialEq)]
    struct AuthorityState {
        facts: Vec<FactState>,
        history: Vec<HistoryState>,
        fts: Vec<(i64, String, String, String)>,
        fts_matches: (i64, i64),
        vectors: Vec<(String, Vec<u8>)>,
        versions: Vec<i64>,
    }

    #[allow(dead_code)] // Derived equality reads every field in this all-state oracle.
    #[derive(Debug, PartialEq)]
    struct LegacyFactState {
        rowid: i64,
        id: String,
        subject: String,
        predicate: String,
        object: String,
        importance: f64,
        access_count: i64,
        last_accessed: Option<String>,
        created_at: String,
        updated_at: String,
        source_ref: Option<String>,
        private: i64,
        tombstone: i64,
        embedding: Option<Vec<u8>>,
    }

    #[allow(dead_code)] // Derived equality reads fields not asserted separately.
    #[derive(Debug, PartialEq)]
    struct LegacyState {
        facts: Vec<LegacyFactState>,
        history: Vec<HistoryState>,
        fts: Vec<(i64, String, String, String)>,
        fts_matches: (i64, i64),
        vectors: Vec<(String, Vec<u8>)>,
        versions: Vec<i64>,
    }

    fn snapshot_legacy_state(conn: &Connection) -> LegacyState {
        let facts = conn
            .prepare(
                "SELECT rowid,id,subject,predicate,object,importance,access_count,last_accessed,
                        created_at,updated_at,source_ref,private,tombstone,embedding
                   FROM facts ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok(LegacyFactState {
                    rowid: row.get(0)?,
                    id: row.get(1)?,
                    subject: row.get(2)?,
                    predicate: row.get(3)?,
                    object: row.get(4)?,
                    importance: row.get(5)?,
                    access_count: row.get(6)?,
                    last_accessed: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                    source_ref: row.get(10)?,
                    private: row.get(11)?,
                    tombstone: row.get(12)?,
                    embedding: row.get(13)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let history = conn
            .prepare("SELECT id,fact_id,op,prev_value,new_value,ts FROM fact_history ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let fts = conn
            .prepare("SELECT rowid,subject,predicate,object FROM facts_fts ORDER BY rowid")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let fts_matches = (
            conn.query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'asterfall'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            conn.query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'mossvale'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        );
        let vectors = conn
            .prepare("SELECT fact_id,embedding FROM facts_vec ORDER BY fact_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let versions = conn
            .prepare("SELECT version FROM schema_version ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        LegacyState {
            facts,
            history,
            fts,
            fts_matches,
            vectors,
            versions,
        }
    }

    fn snapshot_authority_state(conn: &Connection) -> AuthorityState {
        let facts = conn
            .prepare(
                "SELECT rowid,id,subject,predicate,object,importance,access_count,last_accessed,
                        created_at,updated_at,source_ref,private,tombstone,embedding,
                        source_origin,effective_date,superseded_by,authority_status
                   FROM facts ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok(FactState {
                    rowid: row.get(0)?,
                    id: row.get(1)?,
                    subject: row.get(2)?,
                    predicate: row.get(3)?,
                    object: row.get(4)?,
                    importance: row.get(5)?,
                    access_count: row.get(6)?,
                    last_accessed: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                    source_ref: row.get(10)?,
                    private: row.get(11)?,
                    tombstone: row.get(12)?,
                    embedding: row.get(13)?,
                    source_origin: row.get(14)?,
                    effective_date: row.get(15)?,
                    superseded_by: row.get(16)?,
                    authority_status: row.get(17)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let history = conn
            .prepare("SELECT id,fact_id,op,prev_value,new_value,ts FROM fact_history ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let fts = conn
            .prepare("SELECT rowid,subject,predicate,object FROM facts_fts ORDER BY rowid")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let fts_matches = (
            conn.query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'asterfall'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            conn.query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'mossvale'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        );
        let vectors = conn
            .prepare("SELECT fact_id,embedding FROM facts_vec ORDER BY fact_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let versions = conn
            .prepare("SELECT version FROM schema_version ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        AuthorityState {
            facts,
            history,
            fts,
            fts_matches,
            vectors,
            versions,
        }
    }

    fn create_legacy_authority_fixture(conn: &Connection) {
        apply_plan1_baseline_for_test(conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE facts (
                id TEXT PRIMARY KEY, subject TEXT NOT NULL, predicate TEXT NOT NULL,
                object TEXT NOT NULL, importance REAL NOT NULL DEFAULT 0.5,
                access_count INTEGER NOT NULL DEFAULT 0, last_accessed TEXT,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL, source_ref TEXT,
                private INTEGER NOT NULL DEFAULT 0, tombstone INTEGER NOT NULL DEFAULT 0,
                embedding BLOB
             );
             CREATE TABLE fact_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT, fact_id TEXT NOT NULL,
                op TEXT NOT NULL CHECK (op IN ('ADD','UPDATE','DELETE','FLAG')),
                prev_value TEXT, new_value TEXT, ts TEXT NOT NULL,
                FOREIGN KEY (fact_id) REFERENCES facts(id)
             );",
        )
        .unwrap();
        conn.execute_batch(PLAN2_VEC_DDL).unwrap();
        conn.execute_batch(PLAN2_FTS_DDL).unwrap();
        conn.execute_batch(
            "INSERT INTO facts
                (id,subject,predicate,object,importance,access_count,last_accessed,
                 created_at,updated_at,source_ref,private,tombstone,embedding)
             VALUES
                ('f_live','Asterfall','deploy-command','run asterfallctl ship',0.8,3,
                 '2026-02-03','2026-01-01','2026-02-02','sources/runbook.md',1,0,X'0102'),
                ('f_old','Mossvale','deploy-command','run old-deploy.sh',0.4,0,NULL,
                 '2025-01-01','2025-02-02','sources/history.md',0,1,X'0304');
             INSERT INTO fact_history (fact_id,op,prev_value,new_value,ts) VALUES
                ('f_live','ADD',NULL,'run asterfallctl ship','2026-01-01'),
                ('f_old','FLAG','run old-deploy.sh','tombstoned','2026-03-01');",
        )
        .unwrap();
        crate::memory::vector::insert_fact_vec(conn, "f_live", &[0.0; 768]).unwrap();
        crate::memory::vector::insert_fact_vec(conn, "f_old", &[1.0; 768]).unwrap();
    }

    #[test]
    fn authority_migration_repeat_preserves_all_state() {
        crate::memory::vector::register_sqlite_vec();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("memory.db");
        let conn = Connection::open(&path).unwrap();
        create_legacy_authority_fixture(&conn);
        apply_plan2(&conn).unwrap();
        conn.execute_batch(
            "UPDATE facts SET
                source_origin='portable/runbook', effective_date='2026-02-01',
                authority_status='current'
             WHERE id='f_live';
             UPDATE facts SET
                source_origin='portable/history', effective_date='2025-01-01',
                superseded_by='f_live', authority_status='historical'
             WHERE id='f_old';",
        )
        .unwrap();
        let first = snapshot_authority_state(&conn);
        drop(conn);

        let conn = Connection::open(&path).unwrap();
        apply_plan2(&conn).unwrap();
        let repeated = snapshot_authority_state(&conn);
        assert_eq!(
            repeated, first,
            "repeat migration must preserve every state value"
        );
        assert_eq!(
            repeated
                .versions
                .iter()
                .filter(|&&version| version == 5)
                .count(),
            1,
            "schema version 5 must be recorded once"
        );
        assert_eq!(repeated.fts_matches, (1, 1));
    }

    #[test]
    fn authority_migration_failure_rolls_back_and_returns_original_error() {
        use std::cell::Cell;

        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        create_legacy_authority_fixture(&conn);
        let before = snapshot_legacy_state(&conn);
        let injected = Cell::new(false);
        let marker = "authority-test-injected-after-first-add";
        let error = apply_authority_schema_with_step(&conn, &mut |added| {
            assert_eq!(added, 1);
            injected.set(true);
            Err(rusqlite::Error::InvalidParameterName(marker.to_string()))
        })
        .unwrap_err();
        assert!(
            injected.get(),
            "test failure must occur after one column add"
        );
        match error {
            rusqlite::Error::InvalidParameterName(actual) => assert_eq!(actual, marker),
            other => panic!("migration must return the injected original error: {other:?}"),
        }
        assert!(
            conn.is_autocommit(),
            "failed migration must leave no transaction open"
        );
        let authority_columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('facts')
                 WHERE name IN ('source_origin','effective_date','superseded_by','authority_status')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(authority_columns, 0, "all authority DDL must roll back");
        let version: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_version WHERE version=5",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 0, "failed migration must not record version 5");
        let after = snapshot_legacy_state(&conn);
        assert_eq!(
            after, before,
            "rollback must preserve every legacy state value"
        );
    }

    #[test]
    fn authority_migration_rejects_case_variant_under_nocase_collation() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (
                version INTEGER PRIMARY KEY,
                applied_at TEXT
             );
             CREATE TABLE facts (
                id TEXT PRIMARY KEY,
                source_origin TEXT,
                effective_date TEXT,
                superseded_by TEXT,
                authority_status TEXT COLLATE NOCASE NOT NULL DEFAULT 'unknown'
             );
             INSERT INTO facts (id,authority_status) VALUES ('f_case','CURRENT');",
        )
        .unwrap();

        let error = apply_authority_schema(&conn).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("authority_status contains 1 invalid values"),
            "case variant must return the authority-specific invalid-value error: {error}"
        );
        let stored: String = conn
            .query_row(
                "SELECT authority_status FROM facts WHERE id='f_case'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "CURRENT", "rejection must not rewrite the value");
        let version: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_version WHERE version=5",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 0, "rejection must not record version 5");

        conn.execute(
            "UPDATE facts SET authority_status='current' WHERE id='f_case'",
            [],
        )
        .unwrap();
        apply_authority_schema(&conn).unwrap();
        let accepted: (String, i64) = conn
            .query_row(
                "SELECT authority_status,
                        (SELECT COUNT(*) FROM schema_version WHERE version=5)
                   FROM facts WHERE id='f_case'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            accepted,
            ("current".to_string(), 1),
            "exact lowercase status must remain compatible and record version 5"
        );
    }
}
