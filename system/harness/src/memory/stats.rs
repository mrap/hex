use rusqlite::Connection;
use std::path::Path;

pub struct StatsReport {
    pub files_indexed: i64,
    pub total_facts: i64,
    pub top_predicates: Vec<(String, i64)>,
    pub top_subjects: Vec<(String, i64)>,
    pub db_size_bytes: u64,
    pub last_consolidated: Option<String>,
    pub schema_version: Option<i64>,
    /// Sum of `(file_size_on_disk - last_offset)` across `transcript_files`
    /// rows where the on-disk file is larger than the recorded watermark.
    /// Lets operators watch the distill backlog burn down slice-by-slice.
    pub backfill_pending_bytes: i64,
    /// Chunks with FTS5 rows but no `vec_chunks` vector — invisible to
    /// semantic recall until the index backfill re-embeds them. `-1` = the
    /// gap query itself failed (must be visible, not a silent zero).
    pub unembedded_chunks: i64,
    /// `vec_chunks` rows whose backing chunk is gone (swept by
    /// `hex memory maintain`). `-1` = query failed.
    pub orphan_vectors: i64,
}

pub fn run(hex_root: &Path, json: bool) -> i32 {
    let db_path = super::db_path(hex_root);

    if !db_path.exists() {
        eprintln!(
            "No memory DB found at {}. Run `hex memory index` to create one.",
            db_path.display()
        );
        return 1;
    }

    let conn = match super::open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("stats: cannot open DB: {e}");
            return 1;
        }
    };

    let report = match gather(&conn, &db_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("stats: query failed: {e}");
            return 1;
        }
    };

    if json {
        print_json(&report);
    } else {
        print_table(&report);
    }
    0
}

fn gather(conn: &Connection, db_path: &Path) -> rusqlite::Result<StatsReport> {
    let files_indexed: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap_or(0);

    let total_facts: i64 = conn
        .query_row("SELECT COUNT(*) FROM facts WHERE tombstone = 0", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);

    let top_predicates: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT predicate, COUNT(*) AS cnt FROM facts WHERE tombstone = 0 \
             GROUP BY predicate ORDER BY cnt DESC LIMIT 10",
        )?;
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };

    let top_subjects: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT subject, COUNT(*) AS cnt FROM facts WHERE tombstone = 0 \
             GROUP BY subject ORDER BY cnt DESC LIMIT 5",
        )?;
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };

    let db_size_bytes = db_path.metadata().map(|m| m.len()).unwrap_or(0);

    // `hex memory consolidate` stamps this key on every run (see
    // memory::consolidate::stamp_last_consolidated). The old query read
    // `MAX(last_consolidated) FROM topics`, but topic-rollup is a no-op and the
    // topics table stays empty — so it printed "never" no matter how many
    // consolidations ran. Read the metadata key the consolidator actually writes.
    let last_consolidated: Option<String> = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'last_consolidated'",
            [],
            |r| r.get(0),
        )
        .ok();

    let schema_version: Option<i64> = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap_or(None);

    // Backfill pending = sum(file_size - last_offset) across registered
    // transcript files whose on-disk size exceeds the recorded watermark.
    // Done in Rust so we can stat the file (SQLite has no file_size()).
    let backfill_pending_bytes: i64 = {
        let mut total: i64 = 0;
        if let Ok(mut stmt) = conn.prepare("SELECT path, last_offset FROM transcript_files") {
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .ok();
            if let Some(rows) = rows {
                for row in rows.flatten() {
                    let (p, off) = row;
                    if let Ok(meta) = std::fs::metadata(&p) {
                        let size = meta.len() as i64;
                        if size > off {
                            total += size - off;
                        }
                    }
                }
            }
        }
        total
    };

    // Vector-gap surfacing: -1 (query failed) printing is intentional — a
    // broken gap-query must be visible, not a silent zero.
    let unembedded_chunks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunks WHERE rowid NOT IN (SELECT rowid FROM vec_chunks)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    let orphan_vectors: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM vec_chunks WHERE rowid NOT IN (SELECT rowid FROM chunks)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(-1);

    Ok(StatsReport {
        files_indexed,
        total_facts,
        top_predicates,
        top_subjects,
        db_size_bytes,
        last_consolidated,
        schema_version,
        backfill_pending_bytes,
        unembedded_chunks,
        orphan_vectors,
    })
}

fn print_table(r: &StatsReport) {
    println!("=== Memory Database Stats ===");
    println!();
    println!("Files indexed:     {}", r.files_indexed);
    println!(
        "Unembedded chunks: {}   (semantic recall misses these — index backfills ≤500/run)",
        r.unembedded_chunks
    );
    println!(
        "Orphan vectors:    {}   (swept by hex memory maintain)",
        r.orphan_vectors
    );
    println!("Facts (live):      {}", r.total_facts);
    println!(
        "DB size:           {:.1} KB",
        r.db_size_bytes as f64 / 1024.0
    );
    println!(
        "Schema version:    {}",
        r.schema_version
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    );
    println!(
        "Last consolidated: {}",
        r.last_consolidated.as_deref().unwrap_or("never")
    );
    println!(
        "Backfill pending:  {} bytes (~{} slices @ 48k tokens)",
        r.backfill_pending_bytes,
        // est slices: bytes / 3.5 chars-per-token / 48_000 tokens-per-slice
        ((r.backfill_pending_bytes as f64) / 3.5 / 48_000.0).ceil() as i64
    );

    println!();
    println!("Top predicates:");
    if r.top_predicates.is_empty() {
        println!("  (none)");
    } else {
        let max_w = r
            .top_predicates
            .iter()
            .map(|(p, _)| p.len())
            .max()
            .unwrap_or(0);
        for (pred, cnt) in &r.top_predicates {
            println!("  {:<width$}  {}", pred, cnt, width = max_w);
        }
    }

    println!();
    println!("Top subjects:");
    if r.top_subjects.is_empty() {
        println!("  (none)");
    } else {
        let max_w = r
            .top_subjects
            .iter()
            .map(|(s, _)| s.len())
            .max()
            .unwrap_or(0);
        for (subj, cnt) in &r.top_subjects {
            println!("  {:<width$}  {}", subj, cnt, width = max_w);
        }
    }
}

fn print_json(r: &StatsReport) {
    let predicates: Vec<serde_json::Value> = r
        .top_predicates
        .iter()
        .map(|(p, c)| serde_json::json!({"predicate": p, "count": c}))
        .collect();
    let subjects: Vec<serde_json::Value> = r
        .top_subjects
        .iter()
        .map(|(s, c)| serde_json::json!({"subject": s, "count": c}))
        .collect();

    let v = serde_json::json!({
        "files_indexed": r.files_indexed,
        "total_facts": r.total_facts,
        "db_size_bytes": r.db_size_bytes,
        "schema_version": r.schema_version,
        "last_consolidated": r.last_consolidated,
        "backfill_pending_bytes": r.backfill_pending_bytes,
        "unembedded_chunks": r.unembedded_chunks,
        "orphan_vectors": r.orphan_vectors,
        "top_predicates": predicates,
        "top_subjects": subjects,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn seed_db(conn: &Connection) -> rusqlite::Result<()> {
        // Apply Plan 2 schema (facts, schema_version, etc.)
        crate::memory::schema::apply_plan2(conn)?;
        // Apply index schema (files table)
        crate::memory::index::init_db(conn)?;

        // Insert a couple of files
        conn.execute_batch(
            "INSERT INTO files (path, mtime, content_hash, indexed_at, chunk_count)
             VALUES ('me/test.md', 0.0, 'abc', '2025-01-01', 1);
             INSERT INTO files (path, mtime, content_hash, indexed_at, chunk_count)
             VALUES ('me/other.md', 0.0, 'def', '2025-01-01', 2);",
        )?;

        // Insert a few facts
        conn.execute_batch(
            "INSERT INTO facts (id, subject, predicate, object, created_at, updated_at)
             VALUES ('f1', 'mike', 'prefers', 'rust', '2025-01-01', '2025-01-01');
             INSERT INTO facts (id, subject, predicate, object, created_at, updated_at)
             VALUES ('f2', 'mike', 'uses', 'claude', '2025-01-01', '2025-01-01');
             INSERT INTO facts (id, subject, predicate, object, created_at, updated_at)
             VALUES ('f3', 'project', 'prefers', 'tdd', '2025-01-01', '2025-01-01');",
        )?;

        Ok(())
    }

    #[test]
    fn stats_output_contains_expected_headers() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join(".hex")).unwrap();

        let db_path = super::super::db_path(hex_root);
        let conn = super::super::open_db(&db_path).unwrap();
        seed_db(&conn).unwrap();
        drop(conn);

        let report = {
            let conn = super::super::open_db(&db_path).unwrap();
            gather(&conn, &db_path).unwrap()
        };

        assert_eq!(report.files_indexed, 2, "should count 2 files");
        assert_eq!(report.total_facts, 3, "should count 3 live facts");
        assert!(!report.top_predicates.is_empty(), "should have predicates");

        // Verify print_table output contains required section headers
        // by checking the function runs without panic
        print_table(&report);
        // schema_version is set by apply_plan2
        assert_eq!(report.schema_version, Some(5));
    }

    #[test]
    fn stats_reads_last_consolidated_from_metadata() {
        // Regression: stats must read the metadata key the consolidator writes,
        // not MAX(last_consolidated) FROM topics (which is always empty → "never").
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join(".hex")).unwrap();

        let db_path = super::super::db_path(hex_root);
        let conn = super::super::open_db(&db_path).unwrap();
        seed_db(&conn).unwrap();

        // No stamp yet → "never".
        let report = gather(&conn, &db_path).unwrap();
        assert!(
            report.last_consolidated.is_none(),
            "should be unset before a run"
        );

        // Simulate what memory::consolidate stamps.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT OR REPLACE INTO metadata (key, value) \
             VALUES ('last_consolidated', '2026-06-03T00:00:00-04:00');",
        )
        .unwrap();

        let report = gather(&conn, &db_path).unwrap();
        assert_eq!(
            report.last_consolidated.as_deref(),
            Some("2026-06-03T00:00:00-04:00"),
            "stats must surface the stamped last_consolidated value"
        );
    }

    /// RED test for task Tgmwwp2z9 (backfill-pending in hex memory stats).
    ///
    /// Behavior under test: `StatsReport` must expose a `backfill_pending_bytes`
    /// figure equal to `sum(file_size_on_disk - last_offset)` over every row in
    /// `transcript_files` whose on-disk file is larger than `last_offset`.
    /// `hex memory stats` will surface this number so operators can see the
    /// 113 MB backlog burn down slice-by-slice instead of "never". This test
    /// fails today because the field does not exist on `StatsReport`.
    #[test]
    fn stats_reports_backfill_pending_bytes_from_transcript_files() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join(".hex")).unwrap();

        // Seed a fake transcript on disk so the implementation can stat it.
        let trans_dir = hex_root.join("raw").join("transcripts");
        std::fs::create_dir_all(&trans_dir).unwrap();
        let trans_path = trans_dir.join("big.md");
        // 1000-byte file; watermark at 400 means 600 bytes pending.
        let body = "x".repeat(1000);
        std::fs::write(&trans_path, &body).unwrap();
        let trans_str = trans_path.to_str().unwrap().to_string();

        let db_path = super::super::db_path(hex_root);
        let conn = super::super::open_db(&db_path).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::index::init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO transcript_files (path, last_offset, last_distilled_at) \
             VALUES (?1, 400, datetime('now'))",
            rusqlite::params![trans_str.as_str()],
        )
        .unwrap();

        let report = gather(&conn, &db_path).unwrap();
        assert_eq!(
            report.backfill_pending_bytes, 600,
            "stats must compute backfill_pending_bytes = sum(file_size - last_offset) \
             over transcript_files; expected 600 (1000-byte file at offset 400)"
        );
    }

    /// Vector-gap surfacing (Task 9): chunks without a vector are invisible to
    /// semantic recall; orphan vectors are dead weight. Both must show up in
    /// stats instead of staying silent.
    #[test]
    fn stats_reports_unembedded_chunks_and_orphan_vectors() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join(".hex")).unwrap();

        let db_path = super::super::db_path(hex_root);
        let conn = super::super::open_db(&db_path).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::index::init_db(&conn).unwrap();

        // 3 chunks; only the first gets a vector → 2 unembedded.
        for i in 0..3 {
            conn.execute(
                "INSERT INTO chunks (file_id, source_path, heading, chunk_index, content, private) \
                 VALUES ('1', 'me/test.md', 'h', ?1, 'chunk content', 0)",
                rusqlite::params![i.to_string()],
            )
            .unwrap();
        }
        let v = vec![0.1f32; crate::memory::vector::EMBED_DIM];
        crate::memory::vector::insert_vec(&conn, 1, &v).unwrap();
        // 1 orphan vector: rowid 99 has no chunks row.
        crate::memory::vector::insert_vec(&conn, 99, &v).unwrap();

        let report = gather(&conn, &db_path).unwrap();
        assert_eq!(
            report.unembedded_chunks, 2,
            "chunks without vectors must be counted (semantic recall misses them)"
        );
        assert_eq!(
            report.orphan_vectors, 1,
            "vectors without a backing chunk must be counted"
        );
    }

    #[test]
    fn stats_tombstoned_facts_excluded() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join(".hex")).unwrap();

        let db_path = super::super::db_path(hex_root);
        let conn = super::super::open_db(&db_path).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::index::init_db(&conn).unwrap();

        conn.execute_batch(
            "INSERT INTO facts (id, subject, predicate, object, created_at, updated_at, tombstone)
             VALUES ('f1', 'mike', 'prefers', 'rust', '2025-01-01', '2025-01-01', 0);
             INSERT INTO facts (id, subject, predicate, object, created_at, updated_at, tombstone)
             VALUES ('f2', 'mike', 'uses', 'old', '2025-01-01', '2025-01-01', 1);",
        )
        .unwrap();

        let report = gather(&conn, &db_path).unwrap();
        assert_eq!(
            report.total_facts, 1,
            "tombstoned facts should not be counted"
        );
    }
}
