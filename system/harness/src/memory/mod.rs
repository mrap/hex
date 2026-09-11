pub mod assemble;
pub mod claude_cli;
pub mod consolidate;
pub mod distill;
pub mod embed;
pub mod embed_client;
pub mod eval;
pub mod index;
pub mod maintain;
pub mod maintain_facts;
pub mod parse_transcripts;
pub mod predicates;
pub mod provider;
pub mod recall;
pub mod recall_config;
pub mod recent;
pub mod rrf;
pub mod schema;
pub mod search;
pub mod stats;
pub mod vector;

use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub fn db_path(hex_root: &Path) -> PathBuf {
    hex_root.join(".hex/memory.db")
}

/// Open the memory DB with sqlite-vec registered. ALL memory code must open
/// connections through this — `Connection::open` directly would miss vec0.
/// Also ensures the Plan 2 schema (facts, fact_history, sessions, topics,
/// transcript_files, facts_vec, facts_fts) is applied — DDL is idempotent.
pub fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    vector::register_sqlite_vec();
    let conn = Connection::open(path)?;
    // Be friendly under concurrent writers (quick + long cron tick, etc.):
    // wait up to 5s for a competing writer to release before erroring.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // Best-effort migration — log but don't fail if a DDL piece errors
    // (e.g. older sqlite-vec without FLOAT[768]); the facts CLI commands will
    // surface a clearer error.
    if let Err(e) = schema::apply_plan2(&conn) {
        eprintln!("[memory] Plan 2 schema migration warning: {e}");
    }
    // Authority is a strict postcondition. Other historical migrations remain
    // best effort, but callers must never receive a connection whose authority
    // fields are absent or known to be incompatible.
    schema::apply_authority_schema(&conn)?;
    if let Err(e) = schema::apply_messages_schema(&conn) {
        eprintln!("[memory] messages schema migration warning: {e}");
    }
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    type HistoryState = (i64, String, String, Option<String>, Option<String>, String);

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
    }

    fn legacy_state(conn: &Connection) -> LegacyState {
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
            .collect::<rusqlite::Result<Vec<_>>>()
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
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let fts = conn
            .prepare("SELECT rowid,subject,predicate,object FROM facts_fts ORDER BY rowid")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
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
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        LegacyState {
            facts,
            history,
            fts,
            fts_matches,
            vectors,
        }
    }

    fn create_legacy_file(path: &Path) -> LegacyState {
        vector::register_sqlite_vec();
        let conn = Connection::open(path).unwrap();
        schema::apply_plan1_baseline_for_test(&conn).unwrap();
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
        conn.execute_batch(schema::PLAN2_VEC_DDL).unwrap();
        conn.execute_batch(schema::PLAN2_FTS_DDL).unwrap();
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
        vector::insert_fact_vec(&conn, "f_live", &[0.0; 768]).unwrap();
        vector::insert_fact_vec(&conn, "f_old", &[1.0; 768]).unwrap();
        legacy_state(&conn)
    }

    #[test]
    fn open_db_migrates_legacy_authority_on_disk_and_preserves_all_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("memory.db");
        let before = create_legacy_file(&path);
        let conn = open_db(&path).unwrap();
        let after = legacy_state(&conn);
        assert_eq!(
            after, before,
            "open_db migration must preserve all legacy state"
        );
        assert_eq!(after.fts_matches, (1, 1));
        let authority = conn
            .prepare(
                "SELECT id,source_origin,effective_date,superseded_by,authority_status
                   FROM facts ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            authority,
            vec![
                (
                    "f_live".to_string(),
                    None,
                    None,
                    None,
                    "unknown".to_string()
                ),
                ("f_old".to_string(), None, None, None, "unknown".to_string()),
            ],
            "legacy authority must remain unknown with no inferred metadata"
        );
    }

    #[test]
    fn open_db_returns_persistent_authority_schema_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("memory.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE facts (
                id TEXT PRIMARY KEY, subject TEXT NOT NULL, predicate TEXT NOT NULL,
                object TEXT NOT NULL, importance REAL NOT NULL DEFAULT 0.5,
                access_count INTEGER NOT NULL DEFAULT 0, last_accessed TEXT,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL, source_ref TEXT,
                private INTEGER NOT NULL DEFAULT 0, tombstone INTEGER NOT NULL DEFAULT 0,
                embedding BLOB, authority_status INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
        drop(conn);

        let error = match open_db(&path) {
            Ok(_) => panic!("incompatible authority schema must fail"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains("authority schema"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("authority_status"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn open_db_keeps_unrelated_message_migration_best_effort() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("memory.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE messages (id TEXT PRIMARY KEY);")
            .unwrap();
        drop(conn);

        let conn = open_db(&path).expect("message migration failure must remain best effort");
        let authority_columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('facts')
                 WHERE name IN ('source_origin','effective_date','superseded_by','authority_status')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            authority_columns, 4,
            "authority postcondition must still pass"
        );
        let reply_to_columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='reply_to'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            reply_to_columns, 0,
            "fixture must retain the unrelated mismatch"
        );
    }
}
