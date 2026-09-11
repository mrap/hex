use chrono::{Local, NaiveDate};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::{DirEntry, WalkDir};

// ── Constants (mirror Python) ─────────────────────────────────────────────────

const MAX_CHUNK_WORDS: usize = 400;
const OVERLAP_WORDS: usize = 80;
const TRANSCRIPT_HOT_DAYS: i64 = 7;

const INDEX_DIRS: &[&str] = &[".", "me", "projects", "people", "evolution", "landings"];

// `_archive` / `hex-archive`: archived (dead) projects must never be embedded — a single
// archive sweep can move thousands of files in, triggering a multi-hour re-embed that holds
// the index lock, bloats memory.db, and pollutes search with dead content. See
// me/decisions/prune-archived-projects-2026-06-06.
const SKIP_PATTERNS: &[&str] = &[
    ".hex",
    ".claude",
    ".sessions",
    "node_modules",
    ".git",
    "_archive",
    "hex-archive",
];

// (subdir, strategy): "full" | "tiered" | "exclude"
const TIERED_RAW_DIRS: &[(&str, &str)] = &[
    ("raw/research", "full"),
    ("raw/captures", "full"),
    ("raw/transcripts", "tiered"),
    ("raw/reflect-runs", "exclude"),
    ("raw/handoffs", "exclude"),
    ("raw/reflections", "exclude"),
    ("raw/docs", "full"),
    ("raw/messages", "full"),
    ("raw/calendar", "full"),
];

// ── Chunk struct ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Chunk {
    pub heading: String,
    pub content: String,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn file_mtime(path: &Path) -> f64 {
    path.metadata()
        .and_then(|m| m.modified())
        .map(|t| {
            t.duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
        })
        .unwrap_or(0.0)
}

fn should_skip(rel_path: &str) -> bool {
    for pat in SKIP_PATTERNS {
        if rel_path.starts_with(pat) || rel_path.contains(&format!("/{}", pat)) {
            return true;
        }
    }
    false
}

/// Detect heading: returns (level, text) if line is a markdown heading (h1-h4).
fn parse_heading(line: &str) -> Option<(usize, &str)> {
    if !line.starts_with('#') {
        return None;
    }
    let level = line.chars().take_while(|&c| c == '#').count();
    if level > 4 {
        return None;
    }
    // SAFETY(string_slice): `level` counts leading ASCII '#' chars (each 1
    // byte), so byte offset `level` is a char boundary right after the '#'s.
    #[allow(clippy::string_slice)]
    let rest = &line[level..];
    if rest.starts_with(' ') && rest.len() > 1 {
        // SAFETY(string_slice): guarded by `rest.starts_with(' ')`, so byte 0
        // is an ASCII space; slicing at 1 lands on a char boundary.
        #[allow(clippy::string_slice)]
        let text = rest[1..].trim();
        if !text.is_empty() {
            return Some((level, text));
        }
    }
    None
}

const PRIVATE_PREFIXES: &[&str] = &["me/", "people/", "raw/"];

/// Index-time privacy flag (spec §7) — true if the file lives under a
/// sensitive prefix. Stored on each chunk so retrieval can filter by column.
pub fn is_private(rel_path: &str) -> bool {
    PRIVATE_PREFIXES.iter().any(|p| rel_path.starts_with(p))
}

pub fn get_source_weight(rel_path: &str, is_old_transcript: bool) -> f64 {
    if rel_path.starts_with("raw/transcripts") {
        return if is_old_transcript { 0.3 } else { 0.5 };
    }
    if rel_path.starts_with("me/decisions/") {
        return 1.5;
    }
    if rel_path.starts_with("people/") {
        return 1.5;
    }
    if rel_path.starts_with("me/") {
        return 1.2;
    }
    if rel_path.starts_with("projects/") {
        return 1.2;
    }
    if rel_path.starts_with("evolution/") {
        return 1.2;
    }
    if rel_path.starts_with("landings/") {
        return 1.0;
    }
    if rel_path.starts_with("raw/research") {
        return 1.0;
    }
    if rel_path.starts_with("raw/captures") {
        return 0.8;
    }
    1.0
}

pub fn is_old_transcript(filepath: &Path) -> bool {
    let stem = filepath.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if let Ok(date) = NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
        let today = Local::now().date_naive();
        let age = (today - date).num_days();
        return age > TRANSCRIPT_HOT_DAYS;
    }
    // Fall back to mtime
    let age_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        - file_mtime(filepath);
    age_secs / 86400.0 > TRANSCRIPT_HOT_DAYS as f64
}

// ── Summary extraction ────────────────────────────────────────────────────────

const SUMMARY_HEADINGS: &[&str] = &[
    "summary",
    "session summary",
    "notes for next session",
    "tasks",
    "files modified",
    "stats",
];

/// Case-insensitive substring search returning a byte offset into `haystack`.
///
/// `needle_lower` must already be lowercase. Unlike searching a separately
/// lowercased copy of the haystack, every offset returned here is a real char
/// boundary in `haystack` itself, so it is always safe to slice with. Case
/// folding can change UTF-8 byte length (e.g. 'İ' U+0130 is 2 bytes but
/// lowercases to 3), which is exactly why offsets must never be carried across
/// from a `to_lowercase()` copy.
fn find_ci(haystack: &str, needle_lower: &str) -> Option<usize> {
    if needle_lower.is_empty() {
        return Some(0);
    }
    haystack.char_indices().find_map(|(i, _)| {
        // SAFETY(string_slice): `i` comes from haystack.char_indices(), so it
        // is always a char boundary in `haystack`.
        #[allow(clippy::string_slice)]
        let mut hay = haystack[i..].chars().flat_map(char::to_lowercase);
        let mut needle = needle_lower.chars();
        loop {
            match (needle.next(), hay.next()) {
                (None, _) => return Some(i),
                (Some(_), None) => return None,
                (Some(n), Some(h)) if n == h => continue,
                _ => return None,
            }
        }
    })
}

/// Extract ECC:SUMMARY blocks and named heading sections from a transcript.
pub fn extract_summaries(content: &str) -> String {
    let mut extracted: Vec<String> = Vec::new();

    // ECC:SUMMARY block extraction (state machine — avoids regex dep).
    // All offsets below index into `content` directly; nothing is derived from
    // a lowercased copy, whose byte offsets can desync from the original.
    let start_kw = "ecc:summary:start";
    let end_kw = "ecc:summary:end";
    let mut search_from = 0;
    while search_from < content.len() {
        // SAFETY(string_slice): `search_from` is 0 or a value clamped up to a
        // char boundary below (the `is_char_boundary` loop); always a boundary.
        #[allow(clippy::string_slice)]
        let Some(s_off) = find_ci(&content[search_from..], start_kw) else {
            break;
        };
        let abs_s = search_from + s_off;
        // Find end of the <!-- ... --> comment that contains START
        // SAFETY(string_slice): `abs_s` = boundary `search_from` + `s_off`
        // (find_ci returns a boundary in the slice), so it is a char boundary.
        #[allow(clippy::string_slice)]
        let Some(comment_end_off) = content[abs_s..].find("-->") else {
            break;
        };
        let body_start = abs_s + comment_end_off + 3;

        // Find end marker
        // SAFETY(string_slice): `body_start` = `abs_s` + byte offset of the
        // ASCII "-->" + 3, all char boundaries.
        #[allow(clippy::string_slice)]
        let Some(e_off) = find_ci(&content[body_start..], end_kw) else {
            break;
        };
        let abs_e = body_start + e_off;
        // Walk back to find <!-- that opens the end comment. Ignore any opener
        // that precedes the block body (an END marker not wrapped in a comment)
        // so the slice below can never have start > end.
        // SAFETY(string_slice): `abs_e` = boundary `body_start` + `e_off`
        // (find_ci returns a boundary), so it is a char boundary.
        #[allow(clippy::string_slice)]
        let comment_open = content[..abs_e]
            .rfind("<!--")
            .filter(|&o| o >= body_start)
            .unwrap_or(abs_e);
        // SAFETY(string_slice): `body_start` and `comment_open` are both char
        // boundaries (comment_open is a rfind byte offset or `abs_e`).
        #[allow(clippy::string_slice)]
        let block_text = content[body_start..comment_open].trim();
        if !block_text.is_empty() {
            extracted.push(block_text.to_string());
        }
        // Skip past the end marker's `-->`. The keyword's lowercase byte length
        // may not match its length in `content`, so clamp forward to the next
        // char boundary. `abs_e > search_from` always, so this makes progress.
        let mut next = (abs_e + end_kw.len() + 4).min(content.len());
        while next < content.len() && !content.is_char_boundary(next) {
            next += 1;
        }
        search_from = next;
    }

    // Heading-based extraction
    let lines: Vec<&str> = content.split('\n').collect();
    let mut in_summary = false;
    let mut summary_lines: Vec<&str> = Vec::new();

    for line in &lines {
        if let Some((level, heading_text)) = parse_heading(line) {
            if level <= 3 {
                if in_summary && !summary_lines.is_empty() {
                    let block = summary_lines.join("\n").trim().to_string();
                    if !block.is_empty() {
                        extracted.push(block);
                    }
                    summary_lines.clear();
                }
                in_summary = SUMMARY_HEADINGS
                    .iter()
                    .any(|h| heading_text.to_lowercase() == *h);
                if in_summary {
                    summary_lines.push(line);
                }
                continue;
            }
        }
        if in_summary {
            summary_lines.push(line);
        }
    }
    if in_summary && !summary_lines.is_empty() {
        let block = summary_lines.join("\n").trim().to_string();
        if !block.is_empty() {
            extracted.push(block);
        }
    }

    extracted.retain(|s| !s.is_empty());
    extracted.join("\n\n")
}

// ── Chunking ──────────────────────────────────────────────────────────────────

/// Split markdown content into chunks by heading, then by word count.
/// Mirrors Python's chunk_by_heading().
pub fn chunk_by_heading(content: &str, deduplicate: bool) -> Vec<Chunk> {
    let mut raw_chunks: Vec<Chunk> = Vec::new();
    let mut current_heading = "(top)".to_string();
    let mut current_lines: Vec<&str> = Vec::new();

    for line in content.split('\n') {
        if let Some((_level, heading_text)) = parse_heading(line) {
            if !current_lines.is_empty() {
                let text = current_lines.join("\n").trim().to_string();
                if !text.is_empty() {
                    raw_chunks.push(Chunk {
                        heading: current_heading.clone(),
                        content: text,
                    });
                }
            }
            current_heading = heading_text.to_string();
            current_lines = vec![line];
        } else {
            current_lines.push(line);
        }
    }
    if !current_lines.is_empty() {
        let text = current_lines.join("\n").trim().to_string();
        if !text.is_empty() {
            raw_chunks.push(Chunk {
                heading: current_heading,
                content: text,
            });
        }
    }

    // Split oversized chunks with overlap
    let mut split_chunks: Vec<Chunk> = Vec::new();
    for chunk in raw_chunks {
        let words: Vec<&str> = chunk.content.split_whitespace().collect();
        if words.len() <= MAX_CHUNK_WORDS {
            split_chunks.push(chunk);
        } else {
            let mut i = 0usize;
            let mut sub_idx = 0usize;
            while i < words.len() {
                let end = (i + MAX_CHUNK_WORDS).min(words.len());
                let sub_content = words[i..end].join(" ");
                let heading = if sub_idx == 0 {
                    chunk.heading.clone()
                } else {
                    format!("{} (part {})", chunk.heading, sub_idx + 1)
                };
                split_chunks.push(Chunk {
                    heading,
                    content: sub_content,
                });
                sub_idx += 1;
                i += MAX_CHUNK_WORDS - OVERLAP_WORDS;
            }
        }
    }

    if !deduplicate {
        return split_chunks;
    }

    // Dedup by (heading.lowercase(), sha256(content))
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut deduped: Vec<Chunk> = Vec::new();
    for chunk in split_chunks {
        let key = (chunk.heading.to_lowercase(), content_hash(&chunk.content));
        if seen.insert(key) {
            deduped.push(chunk);
        }
    }
    deduped
}

// ── Schema ────────────────────────────────────────────────────────────────────

pub fn init_db(conn: &Connection) -> rusqlite::Result<()> {
    // pragma_update fails for journal_mode because it returns a result row.
    // Use query_row instead and discard the result.
    let _ = conn.query_row::<String, _, _>("PRAGMA journal_mode=WAL", [], |r| r.get(0));

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT UNIQUE NOT NULL,
            mtime REAL NOT NULL,
            content_hash TEXT NOT NULL DEFAULT '',
            indexed_at TEXT NOT NULL,
            chunk_count INTEGER DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS chunk_meta (
            chunk_rowid INTEGER PRIMARY KEY,
            source_weight REAL NOT NULL DEFAULT 1.0
        );
        ",
    )?;

    // Schema migration: chunks FTS5 → v3 (private column + vec_chunks)
    let chunks_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='chunks'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    if chunks_exists {
        let private_ok = conn.prepare("SELECT private FROM chunks LIMIT 0").is_ok();
        if !private_ok {
            let migration_done: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM metadata WHERE key='schema_migrated_chunks_v3'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0)
                > 0;
            if !migration_done {
                eprintln!("  NOTE: Upgrading chunks schema (→ v3: private column + vectors). Files will be re-indexed.");
                // Run the destructive steps + the completion marker atomically so a
                // crash between them cannot leave the DB in a half-migrated state.
                // The FTS5 CREATE VIRTUAL TABLE that follows is intentionally outside
                // this transaction — SQLite cannot roll back FTS5 virtual-table DDL.
                conn.execute_batch(
                    "BEGIN;
                     DROP TABLE IF EXISTS chunks;
                     DROP TABLE IF EXISTS vec_chunks;
                     DELETE FROM files;
                     DELETE FROM chunk_meta;
                     INSERT OR REPLACE INTO metadata (key, value) VALUES ('schema_migrated_chunks_v3', '1');
                     COMMIT;",
                )?;
            }
        }
    } else {
        conn.execute(
            "INSERT OR IGNORE INTO metadata (key, value) VALUES ('schema_migrated_chunks_v3', '1')",
            [],
        )?;
    }

    conn.execute_batch(
        "
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
            file_id,
            source_path,
            heading,
            chunk_index,
            content,
            private,
            tokenize='unicode61'
        );
        ",
    )?;

    // Migration: add content_hash column if missing (old DBs)
    let has_content_hash: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(files)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .flatten()
            .collect();
        cols.iter().any(|c| c == "content_hash")
    };
    if !has_content_hash {
        conn.execute(
            "ALTER TABLE files ADD COLUMN content_hash TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }

    super::vector::init_vec_table(conn)?;

    Ok(())
}

// ── Delete helpers ────────────────────────────────────────────────────────────

fn delete_chunks_for_file(conn: &Connection, file_id: i64) -> rusqlite::Result<()> {
    // Collect chunk rowids for chunk_meta + vec_chunks cleanup. Propagate a
    // row-read error instead of `.flatten()`-dropping it (S6): a silently
    // skipped rowid would survive the `delete_vecs` pass below but still be
    // removed by `DELETE FROM chunks`, stranding its vector — the exact
    // orphan-vector class this function exists to prevent (V1's 74k-orphan
    // bug was a missing delete; a silent skip is the same failure by another
    // route).
    let rowids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT rowid FROM chunks WHERE file_id = ?")?;
        let rows = stmt.query_map(params![file_id.to_string()], |r| r.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<i64>>>()?
    };

    if !rowids.is_empty() {
        // Delete chunk_meta in batches of 500
        for batch in rowids.chunks(500) {
            let placeholders: String = batch.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("DELETE FROM chunk_meta WHERE chunk_rowid IN ({placeholders})");
            let mut stmt = conn.prepare(&sql)?;
            for (i, id) in batch.iter().enumerate() {
                stmt.raw_bind_parameter(i + 1, *id)?;
            }
            stmt.raw_execute()?;
        }

        // Delete the matching vec_chunks rows — V1's 74k-orphan bug was a missing
        // DELETE here (spec §5.2).
        super::vector::delete_vecs(conn, &rowids)?;
    }

    // FTS5 delete by file_id (stored as text)
    conn.execute(
        "DELETE FROM chunks WHERE file_id = ?",
        params![file_id.to_string()],
    )?;
    Ok(())
}

// ── Index file ────────────────────────────────────────────────────────────────

/// Index a single file. Returns the number of chunks written.
pub fn index_file(
    conn: &Connection,
    filepath: &Path,
    hex_root: &Path,
    content: &str,
    mtime: f64,
    strategy: &str,
    embedder: &super::embed::Embedder,
) -> rusqlite::Result<usize> {
    let rel_path = filepath
        .strip_prefix(hex_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| filepath.to_string_lossy().to_string());
    let chash = content_hash(content);
    let private_flag: i64 = if is_private(&rel_path) { 1 } else { 0 };

    let is_old_tr = strategy == "summary";

    // Apply summary extraction for old transcripts
    let effective_content = if strategy == "summary" {
        let s = extract_summaries(content);
        if s.trim().is_empty() {
            // No summaries found — record in files table with 0 chunks and return
            let existing_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM files WHERE path = ?",
                    params![rel_path],
                    |r| r.get(0),
                )
                .ok();
            if let Some(fid) = existing_id {
                delete_chunks_for_file(conn, fid)?;
                conn.execute("DELETE FROM files WHERE id = ?", params![fid])?;
            }
            conn.execute(
                "INSERT INTO files (path, mtime, content_hash, indexed_at, chunk_count) VALUES (?, ?, ?, ?, 0)",
                params![rel_path, mtime, chash, Local::now().to_rfc3339()],
            )?;
            return Ok(0);
        }
        s
    } else {
        content.to_string()
    };

    let chunks = chunk_by_heading(&effective_content, true);

    // Remove old file record + chunks
    let existing_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM files WHERE path = ?",
            params![rel_path],
            |r| r.get(0),
        )
        .ok();
    if let Some(fid) = existing_id {
        delete_chunks_for_file(conn, fid)?;
        conn.execute("DELETE FROM files WHERE id = ?", params![fid])?;
    }

    // Insert new file record
    conn.execute(
        "INSERT INTO files (path, mtime, content_hash, indexed_at, chunk_count) VALUES (?, ?, ?, ?, ?)",
        params![rel_path, mtime, chash, Local::now().to_rfc3339(), chunks.len() as i64],
    )?;
    let file_id: i64 = conn.query_row("SELECT last_insert_rowid()", [], |r| r.get(0))?;

    let weight = get_source_weight(&rel_path, is_old_tr);

    // Insert FTS5 chunk rows, collecting rowids for the vector pass.
    let mut chunk_rowids: Vec<i64> = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        conn.execute(
            "INSERT INTO chunks (file_id, source_path, heading, chunk_index, content, private) \
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                file_id.to_string(),
                rel_path,
                chunk.heading,
                i.to_string(),
                chunk.content,
                private_flag
            ],
        )?;
        let chunk_rowid: i64 = conn.query_row("SELECT last_insert_rowid()", [], |r| r.get(0))?;
        chunk_rowids.push(chunk_rowid);
        conn.execute(
            "INSERT INTO chunk_meta (chunk_rowid, source_weight) VALUES (?, ?)",
            params![chunk_rowid, weight],
        )?;
    }

    // Vector pass: embed + persist chunk vectors in EMBED_BATCH-sized batches,
    // committing each batch before the next embeds (H-09 / OBS-019). An embed
    // failure or per-batch count mismatch is loud but non-fatal — the file keeps
    // its FTS5 rows (searchable) and any un-vectored chunks are repaired by
    // `backfill_missing_vectors` on a later tick. `hex memory stats` surfaces any
    // residual vec/chunk gap.
    let contents: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
    super::embed::log_rss(&format!(
        "pre-embed {} ({} chunks)",
        rel_path,
        contents.len()
    ));
    let stored = embed_and_store(conn, &rel_path, &chunk_rowids, &contents, |batch| {
        embedder.embed_documents(batch)
    });
    super::embed::log_rss(&format!(
        "post-embed {} ({}/{} vectors)",
        rel_path,
        stored,
        chunk_rowids.len()
    ));

    Ok(chunks.len())
}

/// Embed `contents` (aligned 1:1 with `chunk_rowids`, same order) in
/// `EMBED_BATCH`-sized batches, persisting each batch's vectors *before* the
/// next batch is embedded. Returns the number of vectors actually stored.
///
/// Two reasons for per-batch persistence:
///   * **Peak working set (OBS-019).** A single `embed_documents(N)` call
///     allocates per-layer activation tensors proportional to N; for nomic-v1.5
///     (768-dim transformer) a large N overflows the 4 GB Docker E2E container.
///     `EMBED_BATCH = 8` bounds each forward pass.
///   * **Interruption durability (H-09).** Committing each batch immediately
///     (rusqlite autocommit) means a kill between batches loses at most
///     `EMBED_BATCH` chunks' vectors; the committed FTS5 chunk rows for any
///     un-vectored batch are repaired by `backfill_missing_vectors` on a later
///     tick. Previously the whole file's vectors were accumulated and inserted
///     only after the last batch, so a mid-file SIGTERM left every chunk of the
///     file vector-less — the 2026-06-12 wedge-kill signature.
///
/// Failures are loud but non-fatal (S6): an embed error stops the loop (the
/// remaining chunks stay FTS5-only, repaired later); a per-batch count mismatch
/// is logged and skips only that batch.
fn embed_and_store<F>(
    conn: &Connection,
    rel_path: &str,
    chunk_rowids: &[i64],
    contents: &[String],
    mut embed_batch: F,
) -> usize
where
    F: FnMut(&[String]) -> anyhow::Result<Vec<Vec<f32>>>,
{
    const EMBED_BATCH: usize = 8;
    let mut stored = 0usize;
    for (rowid_batch, content_batch) in chunk_rowids
        .chunks(EMBED_BATCH)
        .zip(contents.chunks(EMBED_BATCH))
    {
        match embed_batch(content_batch) {
            Ok(vecs) if vecs.len() == rowid_batch.len() => {
                for (rowid, vec) in rowid_batch.iter().zip(vecs.iter()) {
                    match super::vector::insert_vec(conn, *rowid, vec) {
                        Ok(()) => stored += 1,
                        Err(e) => {
                            eprintln!("  ERROR storing vector for chunk {rowid} of {rel_path}: {e}")
                        }
                    }
                }
            }
            Ok(vecs) => {
                // S6 — never silently drop chunks: a count mismatch is loud. Skip
                // only this batch; later batches are still embedded.
                eprintln!(
                    "  ERROR embedding {rel_path}: batch expected {} vectors, got {} (FTS5-only for these chunks)",
                    rowid_batch.len(),
                    vecs.len()
                );
            }
            Err(e) => {
                // Embed failure — stop; remaining chunks stay FTS5-only and are
                // repaired by backfill_missing_vectors on a later tick.
                eprintln!("  ERROR embedding {rel_path} (FTS5-only for remaining chunks): {e}");
                break;
            }
        }
    }
    stored
}

// ── File discovery ────────────────────────────────────────────────────────────

fn checked_source_root(hex_root: &Path, relative: &str) -> io::Result<Option<PathBuf>> {
    let root_metadata = std::fs::metadata(hex_root)?;
    if !root_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "memory source root is not a directory: {}",
                hex_root.display()
            ),
        ));
    }
    if relative == "." {
        return Ok(Some(hex_root.to_path_buf()));
    }

    let mut current = hex_root.to_path_buf();
    for component in Path::new(relative).components() {
        let std::path::Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid configured memory source root: {relative}"),
            ));
        };
        current.push(component);
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() {
            return Ok(None);
        }
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "configured memory source component is not a directory: {}",
                    current.display()
                ),
            ));
        }
    }
    Ok(Some(current))
}

fn keep_discovery_entry<F>(
    entry: &DirEntry,
    hex_root: &Path,
    observer: &mut F,
    path_error: &mut Option<io::Error>,
) -> bool
where
    F: FnMut(&Path),
{
    if path_error.is_some() {
        return false;
    }
    observer(entry.path());
    if entry.depth() == 0 {
        return true;
    }
    if entry.file_type().is_symlink() {
        return false;
    }
    match entry.path().strip_prefix(hex_root) {
        Ok(relative) => !should_skip(&relative.to_string_lossy()),
        Err(error) => {
            *path_error = Some(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "memory discovery escaped root at {}: {error}",
                    entry.path().display()
                ),
            ));
            false
        }
    }
}

fn collect_source_root<F>(
    hex_root: &Path,
    relative: &str,
    recursive: bool,
    strategy: &str,
    observer: &mut F,
    files: &mut Vec<(PathBuf, String)>,
) -> io::Result<()>
where
    F: FnMut(&Path),
{
    let Some(source_root) = checked_source_root(hex_root, relative)? else {
        return Ok(());
    };
    let max_depth = if recursive { usize::MAX } else { 1 };
    let mut path_error = None;
    let mut source_files = Vec::new();
    {
        let walker = WalkDir::new(&source_root)
            .follow_links(false)
            .follow_root_links(relative == ".")
            .max_depth(max_depth)
            .into_iter()
            .filter_entry(|entry| keep_discovery_entry(entry, hex_root, observer, &mut path_error));
        for entry in walker {
            let entry = entry.map_err(|error| {
                let kind = error
                    .io_error()
                    .map(io::Error::kind)
                    .unwrap_or(io::ErrorKind::Other);
                io::Error::new(kind, error.to_string())
            })?;
            if entry.depth() == 0 || !entry.file_type().is_file() {
                continue;
            }
            let Some(file_name) = entry.path().file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !file_name.ends_with(".md") && !file_name.ends_with(".txt") {
                continue;
            }
            source_files.push(entry.into_path());
        }
    }
    if let Some(error) = path_error {
        return Err(error);
    }
    source_files.sort_by(|left, right| {
        let extension_order = |path: &Path| match path.file_name().and_then(|name| name.to_str()) {
            Some(name) if name.ends_with(".md") => 0,
            Some(name) if name.ends_with(".txt") => 1,
            _ => unreachable!("source files were filtered by extension"),
        };
        extension_order(left)
            .cmp(&extension_order(right))
            .then_with(|| left.cmp(right))
    });
    for path in source_files {
        let file_strategy = if strategy == "tiered" && is_old_transcript(&path) {
            "summary"
        } else if strategy == "tiered" {
            "full"
        } else {
            strategy
        };
        files.push((path, file_strategy.to_string()));
    }
    Ok(())
}

fn get_indexable_files_with_observer<F>(
    hex_root: &Path,
    mut observer: F,
) -> io::Result<Vec<(PathBuf, String)>>
where
    F: FnMut(&Path),
{
    let mut files = Vec::new();
    for relative in INDEX_DIRS {
        collect_source_root(
            hex_root,
            relative,
            *relative != ".",
            "full",
            &mut observer,
            &mut files,
        )?;
    }
    for (relative, strategy) in TIERED_RAW_DIRS {
        if *strategy != "exclude" {
            collect_source_root(
                hex_root,
                relative,
                true,
                strategy,
                &mut observer,
                &mut files,
            )?;
        }
    }
    Ok(files)
}

/// Collect all indexable regular files without following filesystem links.
pub fn get_indexable_files(hex_root: &Path) -> io::Result<Vec<(PathBuf, String)>> {
    get_indexable_files_with_observer(hex_root, |_| {})
}

// ── Main indexer ──────────────────────────────────────────────────────────────

fn set_metadata(conn: &Connection, key: &str, value: &str) {
    if let Err(e) = conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES (?, ?)",
        params![key, value],
    ) {
        eprintln!("[memory index] metadata write failed ({key}): {e}");
    }
}

fn get_metadata(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM metadata WHERE key = ?",
        params![key],
        |r| r.get(0),
    )
    .ok()
}

/// Per-run wall-clock budget for [`run_index`]. Default 600s (10 min). Caps the
/// dominant 2026-06-12 wedge signature: a throttled crawl over a large multi-file
/// worklist (e.g. ~945 files post-reboot) burning a core unbounded between the
/// 15-minute cron ticks. This is a BETWEEN-files gate — it is checked at the top
/// of each file, so a single in-flight `index_file` is not interrupted mid-embed
/// (that case is bounded instead by H-09's per-batch commit + the throttle, and
/// is unlikely to exceed the budget — ~240+ chunks in one file at the throttled
/// rate; a within-file budget is the queued follow-up, FIX-016).
/// `HEX_INDEX_BUDGET_SECS` tunes it; `0` disables the cap (returns `None`).
/// Values below the ~2s cold model-load starve the run (bail at file 0) — keep
/// it well above that; the default has ample headroom.
fn run_budget() -> Option<std::time::Duration> {
    let secs = std::env::var("HEX_INDEX_BUDGET_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(600);
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

pub fn run_index(hex_root: &Path, full: bool) -> i32 {
    let t0 = std::time::Instant::now();
    let db_path = super::db_path(hex_root);

    if let Some(parent) = db_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("hex memory index: cannot create .hex dir: {e}");
            return 1;
        }
    }

    // Single-instance guard: a slow run (e.g. a large append-only file re-embed)
    // must not pile up behind the 15-min cron. Hold an exclusive lock for the
    // duration; if another run holds it, skip cleanly (exit 0 — overlap is normal).
    use fs2::FileExt;
    let lock_path = db_path.with_file_name("memory-index.lock");
    let lock_file = match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "hex memory index: cannot open lock {}: {e}",
                lock_path.display()
            );
            return 1;
        }
    };
    if lock_file.try_lock_exclusive().is_err() {
        println!("hex memory index: another run is in progress — skipping");
        return 0;
    }
    let _index_lock = lock_file; // released when run_index returns

    // Discovery must complete before DB setup, model construction, indexing, or
    // stale-record cleanup. An incomplete source list cannot authorize deletion.
    let file_tuples = match get_indexable_files(hex_root) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("hex memory index: source discovery failed: {error}");
            return 1;
        }
    };

    let conn = match super::open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hex memory index: cannot open {}: {e}", db_path.display());
            return 1;
        }
    };

    if let Err(e) = init_db(&conn) {
        eprintln!("hex memory index: schema init failed: {e}");
        return 1;
    }

    let embedder = match super::embed::Embedder::new(hex_root) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("hex memory index: embedding model failed to load: {e}");
            return 1;
        }
    };

    // Build lookup of existing records: path → (mtime, content_hash)
    let existing: std::collections::HashMap<String, (f64, String)> = {
        let mut stmt = conn
            .prepare("SELECT path, mtime, content_hash FROM files")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, String>(2).unwrap_or_default(),
            ))
        })
        .unwrap()
        .flatten()
        .map(|(p, m, h)| (p, (m, h)))
        .collect()
    };

    println!("Found {} files to check", file_tuples.len());

    let mut indexed = 0usize;
    let mut skipped_mtime = 0usize;
    let mut skipped_hash = 0usize;
    let mut total_chunks = 0usize;
    let budget = run_budget();
    let mut over_budget = false;

    for (i, (filepath, strategy)) in file_tuples.iter().enumerate() {
        // Defense-in-depth (S6): a pathological corpus change or slow file must
        // not quietly burn a core for an unbounded time between 15-min ticks
        // (the 2026-06-12 wedge class). Once over budget, bail LOUDLY before
        // starting another file; the files already indexed are committed
        // (autocommit) and the rest resume on the next tick.
        if let Some(b) = budget {
            if t0.elapsed() > b {
                eprintln!(
                    "hex memory index: EXCEEDED {}s wall-clock budget after {indexed} indexed \
                     ({} of {} files unprocessed) — bailing loudly; remaining resume next tick \
                     (set HEX_INDEX_BUDGET_SECS to tune, 0 disables)",
                    b.as_secs(),
                    file_tuples.len() - i,
                    file_tuples.len()
                );
                over_budget = true;
                break;
            }
        }
        let rel_path = match filepath.strip_prefix(hex_root) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let mtime = file_mtime(filepath);

        if !full {
            let prev = existing.get(&rel_path);

            // Stage 1: mtime pre-filter
            if let Some((prev_mtime, _)) = prev {
                if (*prev_mtime - mtime).abs() < 1e-6 {
                    skipped_mtime += 1;
                    continue;
                }
            }

            // Stage 2: content hash check
            let content = match std::fs::read_to_string(filepath) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("  SKIP {rel_path}: {e}");
                    continue;
                }
            };
            if content.trim().is_empty() {
                continue;
            }
            let chash = content_hash(&content);
            if let Some((_, prev_hash)) = prev {
                if !prev_hash.is_empty() && *prev_hash == chash {
                    // Content identical — update mtime only
                    if let Err(e) = conn.execute(
                        "UPDATE files SET mtime = ? WHERE path = ?",
                        params![mtime, rel_path],
                    ) {
                        eprintln!("[memory index] mtime refresh failed for {rel_path}: {e}");
                    }
                    skipped_hash += 1;
                    continue;
                }
            }

            // Actually re-index
            match index_file(
                &conn, filepath, hex_root, &content, mtime, strategy, &embedder,
            ) {
                Ok(n) => {
                    if n > 0 {
                        indexed += 1;
                        total_chunks += n;
                        let tag = if strategy != "full" {
                            format!(" [{strategy}]")
                        } else {
                            String::new()
                        };
                        println!("  Indexed: {rel_path} ({n} chunks{tag})");
                    } else if strategy == "summary" {
                        println!("  Indexed: {rel_path} (0 chunks, no summaries found [summary])");
                    }
                }
                Err(e) => {
                    eprintln!("  ERROR indexing {rel_path}: {e}");
                }
            }
        } else {
            // Full mode: read unconditionally
            let content = match std::fs::read_to_string(filepath) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("  SKIP {rel_path}: {e}");
                    continue;
                }
            };
            if content.trim().is_empty() {
                continue;
            }
            match index_file(
                &conn, filepath, hex_root, &content, mtime, strategy, &embedder,
            ) {
                Ok(n) => {
                    if n > 0 {
                        indexed += 1;
                        total_chunks += n;
                        let tag = if strategy != "full" {
                            format!(" [{strategy}]")
                        } else {
                            String::new()
                        };
                        println!("  Indexed: {rel_path} ({n} chunks{tag})");
                    } else if strategy == "summary" {
                        println!("  Indexed: {rel_path} (0 chunks, no summaries found [summary])");
                    }
                }
                Err(e) => {
                    eprintln!("  ERROR indexing {rel_path}: {e}");
                }
            }
        }
    }

    // If we bailed on the wall-clock budget, skip the (potentially slow)
    // backfill and the cleanup sweep and exit LOUDLY non-zero (S6) — the next
    // tick resumes the remaining files. Files already indexed are committed.
    if over_budget {
        set_metadata(&conn, "last_run", &Local::now().to_rfc3339());
        set_metadata(
            &conn,
            "last_run_mode",
            if full { "full" } else { "incremental" },
        );
        let elapsed = t0.elapsed().as_secs_f64();
        eprintln!(
            "hex memory index: BAILED after {elapsed:.1}s over budget — {indexed} indexed, \
             {skipped_mtime} unchanged (mtime), {skipped_hash} unchanged (hash), \
             {total_chunks} new chunks; cleanup + backfill skipped, resuming next tick"
        );
        return 1;
    }

    // Cleanup: remove DB records for files no longer on disk
    let all_paths: HashSet<String> = file_tuples
        .iter()
        .filter_map(|(p, _)| {
            p.strip_prefix(hex_root)
                .ok()
                .map(|r| r.to_string_lossy().to_string())
        })
        .collect();

    let mut removed = 0usize;
    for db_path_str in existing.keys() {
        if !all_paths.contains(db_path_str) {
            let existing_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM files WHERE path = ?",
                    params![db_path_str],
                    |r| r.get(0),
                )
                .ok();
            if let Some(fid) = existing_id {
                if let Err(e) = delete_chunks_for_file(&conn, fid) {
                    eprintln!("  ERROR removing chunks for {db_path_str}: {e}");
                } else if let Err(e) = conn.execute("DELETE FROM files WHERE id = ?", params![fid])
                {
                    eprintln!("  ERROR removing file record {db_path_str}: {e}");
                } else {
                    removed += 1;
                    println!("  Removed: {db_path_str}");
                }
            }
        }
    }

    // Backfill: chunks whose embed failed in an earlier run stay FTS5-only
    // FOREVER unless their file changes (assessment: 1,060 chunks / 7.1%
    // invisible to semantic recall). Re-embed up to a per-run cap here.
    const BACKFILL_CAP: usize = 500;
    match backfill_missing_vectors(&conn, &embedder, BACKFILL_CAP) {
        Ok(0) => {}
        Ok(n) => println!("index: backfilled {n} missing chunk vector(s)"),
        Err(e) => eprintln!("index: vector backfill FAILED: {e}"),
    }

    set_metadata(&conn, "last_run", &Local::now().to_rfc3339());
    set_metadata(
        &conn,
        "last_run_mode",
        if full { "full" } else { "incremental" },
    );

    let elapsed = t0.elapsed().as_secs_f64();
    println!(
        "\nDone in {elapsed:.2}s: {indexed} indexed, \
         {skipped_mtime} unchanged (mtime), {skipped_hash} unchanged (hash), \
         {removed} removed, {total_chunks} new chunks"
    );

    0
}

/// Re-embed chunks that have FTS5 rows but no `vec_chunks` vector (an embed
/// failure in an earlier run). Capped per run so a large gap burns down over
/// successive 15-min cron ticks without blowing the run's time budget.
/// Mirrors the per-file embed block in `index_file` (EMBED_BATCH=8,
/// `embed_documents`, `insert_vec`): failures are loud but non-fatal.
fn backfill_missing_vectors(
    conn: &Connection,
    embedder: &super::embed::Embedder,
    cap: usize,
) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT c.rowid, c.content FROM chunks c
         WHERE c.rowid NOT IN (SELECT rowid FROM vec_chunks)
         LIMIT ?1",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([cap as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    let mut done = 0;
    for batch in rows.chunks(8) {
        let texts: Vec<String> = batch.iter().map(|(_, c)| c.clone()).collect();
        match embedder.embed_documents(&texts) {
            Ok(vecs) if vecs.len() == batch.len() => {
                for ((rowid, _), vec) in batch.iter().zip(vecs) {
                    super::vector::insert_vec(conn, *rowid, &vec)?;
                    done += 1;
                }
            }
            Ok(v) => eprintln!(
                "index backfill: batch len mismatch ({} != {})",
                v.len(),
                batch.len()
            ),
            Err(e) => eprintln!("index backfill: embed batch failed: {e}"),
        }
    }
    Ok(done)
}

// ── Stats ─────────────────────────────────────────────────────────────────────

/// Human-readable note for a `vec_chunks` vs `chunks` count mismatch, or `None`
/// when they match. A *deficit* (fewer vectors than chunks) is chunks awaiting
/// embed — backfilled on later index ticks. A *surplus* (more vectors than
/// chunks) is orphan vectors — swept by the weekly `hex memory maintain`.
/// Surfacing the surplus positively avoids the confusing negative
/// "N chunk(s) without a vector" the old single-branch message printed (P2/S6).
fn vec_gap_message(chunk_count: i64, vec_count: i64) -> Option<String> {
    use std::cmp::Ordering;
    match vec_count.cmp(&chunk_count) {
        Ordering::Equal => None,
        Ordering::Less => Some(format!(
            "WARNING: {} chunk(s) without a vector (backfilled on later ticks)",
            chunk_count - vec_count
        )),
        Ordering::Greater => Some(format!(
            "NOTE: {} orphan vector(s) (surplus; swept by weekly maintain)",
            vec_count - chunk_count
        )),
    }
}

pub fn show_stats(hex_root: &Path) -> i32 {
    let db_path = super::db_path(hex_root);
    if !db_path.exists() {
        println!("No index found. Run `hex memory index` to create one.");
        return 1;
    }

    let conn = match super::open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hex memory stats: cannot open db: {e}");
            return 1;
        }
    };

    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap_or(0);
    let chunk_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
        .unwrap_or(0);
    let hashed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE content_hash != ''",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let db_size_kb = db_path
        .metadata()
        .map(|m| m.len() as f64 / 1024.0)
        .unwrap_or(0.0);

    println!("Database: {}", db_path.display());
    println!("Size: {db_size_kb:.1} KB");
    println!("Files indexed: {file_count} ({hashed} with content hash)");
    println!("Total chunks: {chunk_count}");
    let vec_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get(0))
        .unwrap_or(0);
    println!("Vector embeddings: {vec_count} (sqlite-vec, nomic 768-d)");
    if let Some(msg) = vec_gap_message(chunk_count, vec_count) {
        println!("  {msg}");
    }

    let last_run = get_metadata(&conn, "last_run");
    let last_mode = get_metadata(&conn, "last_run_mode");
    if let Some(run) = last_run {
        let mode = last_mode.as_deref().unwrap_or("unknown");
        println!("Last run: {run} ({mode})");
    }
    println!();

    println!("By directory:");
    let mut stmt = conn
        .prepare(
            "
        SELECT
            CASE
                WHEN source_path LIKE '%/%'
                THEN substr(source_path, 1, instr(source_path, '/') - 1)
                ELSE '(root)'
            END as dir,
            COUNT(DISTINCT source_path) as files,
            COUNT(*) as chunks
        FROM chunks
        GROUP BY dir
        ORDER BY chunks DESC
    ",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .unwrap();
    for row in rows.flatten() {
        println!("  {}: {} files, {} chunks", row.0, row.1, row.2);
    }

    0
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run(hex_root: &Path, full: bool, stats: bool) -> i32 {
    if stats {
        show_stats(hex_root)
    } else {
        let mode = if full {
            "Full reindex"
        } else {
            "Incremental index"
        };
        println!("{mode}...");
        run_index(hex_root, full)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_heading_basic() {
        assert_eq!(parse_heading("# Hello"), Some((1, "Hello")));
        assert_eq!(parse_heading("## Section"), Some((2, "Section")));
        assert_eq!(parse_heading("#### Deep"), Some((4, "Deep")));
        assert_eq!(parse_heading("##### TooDeep"), None);
        assert_eq!(parse_heading("Not a heading"), None);
        assert_eq!(parse_heading("#NoSpace"), None);
    }

    #[test]
    fn test_chunk_by_heading_simple() {
        let content = "# Title\nSome content here.\n## Section\nMore content.";
        let chunks = chunk_by_heading(content, false);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "Title");
        assert!(chunks[0].content.contains("Some content"));
        assert_eq!(chunks[1].heading, "Section");
    }

    #[test]
    fn test_chunk_large_content_splits_with_overlap() {
        // Build content with 500 words under one heading.
        // The heading line ("# Big") is kept in current_lines, so the chunk content
        // includes "# Big" (2 extra tokens) → total 502 tokens.
        // first chunk:  words[0..400]       = 400 tokens; i → 320
        // second chunk: words[320..502]      = 182 tokens; i → 640 (done)
        let many_words = vec!["word"; 500].join(" ");
        let content = format!("# Big\n{}", many_words);
        let chunks = chunk_by_heading(content.as_str(), false);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "Big");
        assert_eq!(chunks[1].heading, "Big (part 2)");
        assert_eq!(chunks[0].content.split_whitespace().count(), 400);
        assert_eq!(chunks[1].content.split_whitespace().count(), 182);
    }

    #[test]
    fn test_chunk_deduplication() {
        let content = "# A\nSame content.\n# A\nSame content.";
        let chunks_dedup = chunk_by_heading(content, true);
        let chunks_no_dedup = chunk_by_heading(content, false);
        assert_eq!(chunks_no_dedup.len(), 2);
        assert_eq!(chunks_dedup.len(), 1);
    }

    #[test]
    fn test_source_weight_matching() {
        assert_eq!(get_source_weight("me/decisions/foo.md", false), 1.5);
        assert_eq!(get_source_weight("people/alice.md", false), 1.5);
        assert_eq!(get_source_weight("me/journal.md", false), 1.2);
        assert_eq!(get_source_weight("projects/foo/bar.md", false), 1.2);
        assert_eq!(get_source_weight("evolution/x.md", false), 1.2);
        assert_eq!(get_source_weight("landings/today.md", false), 1.0);
        assert_eq!(get_source_weight("raw/research/paper.md", false), 1.0);
        assert_eq!(get_source_weight("raw/captures/clip.md", false), 0.8);
        assert_eq!(
            get_source_weight("raw/transcripts/2020-01-01.md", true),
            0.3
        );
        assert_eq!(
            get_source_weight("raw/transcripts/2026-05-01.md", false),
            0.5
        );
        assert_eq!(get_source_weight("misc/unknown.md", false), 1.0);
    }

    #[test]
    fn memory_source_eligibility_characterizes_ordinary_selection() {
        let fixture = TempDir::new().unwrap();
        let root = fixture.path().join("workspace");
        let expected = [
            (".md", "full"),
            (".root-hidden.md", "full"),
            ("CLAUDE.md", "full"),
            (".txt", "full"),
            ("notes.txt", "full"),
            ("me/.profile.md", "full"),
            ("me/me.md", "full"),
            ("me/.notes/nested.txt", "full"),
            ("projects/example/.evidence.md", "full"),
            ("projects/example/context.md", "full"),
            ("people/alice/profile.txt", "full"),
            ("evolution/observations.md", "full"),
            ("landings/2099-01-01.md", "full"),
            ("raw/research/paper.md", "full"),
            ("raw/captures/clip.txt", "full"),
            ("raw/transcripts/2000-01-01.md", "summary"),
            ("raw/transcripts/2999-01-01.md", "full"),
            ("raw/docs/reference.md", "full"),
            ("raw/messages/message.txt", "full"),
            ("raw/calendar/event.md", "full"),
        ];
        for (relative, _) in expected {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("fixture: {relative}\n")).unwrap();
        }

        let excluded = [
            "misc/deep.md",
            "me/UPPER.MD",
            "me/image.png",
            "projects/example/.claude/ignored.md",
            "projects/example/.git/ignored.md",
            "projects/example/.hex/ignored.md",
            "projects/example/.sessions/ignored.md",
            "projects/example/_archive/ignored.md",
            "projects/example/hex-archive/ignored.md",
            "projects/example/node_modules/ignored.md",
            "raw/handoffs/ignored.md",
            "raw/reflect-runs/ignored.md",
            "raw/reflections/ignored.md",
            "raw/root.md",
        ];
        for relative in excluded {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("excluded fixture: {relative}\n")).unwrap();
        }

        let selected = get_indexable_files(&root).expect("complete ordinary fixture discovery");
        assert_eq!(
            selected.len(),
            expected.len(),
            "ordinary discovery returned duplicate or unexpected paths"
        );
        let selected: Vec<_> = selected
            .into_iter()
            .map(|(path, strategy)| (path.strip_prefix(&root).unwrap().to_path_buf(), strategy))
            .collect();
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(path, strategy)| (PathBuf::from(path), strategy.to_string()))
            .collect();

        assert_eq!(
            selected, expected,
            "ordinary source roots, ordering, strategies, hidden paths, extensions, or skip rules changed"
        );
    }

    // This regression intentionally fails against glob-based discovery. It uses
    // only owned temporary fixtures and calls discovery without indexing.
    #[cfg(unix)]
    #[test]
    fn memory_source_eligibility_rejects_external_registry_without_mutation() {
        use std::collections::BTreeMap;
        use std::os::unix::fs::symlink;

        let fixture = TempDir::new().unwrap();
        let root = fixture.path().join("workspace");
        let external = fixture.path().join("external-registry");
        std::fs::create_dir_all(root.join("projects/example/evidence/cargo")).unwrap();
        std::fs::create_dir_all(external.join("package")).unwrap();
        let canonical = root.join("projects/example/context.md");
        std::fs::write(&canonical, "# Current project\nRetain this document.\n").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "# Workspace\n").unwrap();
        std::fs::create_dir_all(root.join("projects/example/.git")).unwrap();
        std::fs::write(
            root.join("projects/example/.git/ignored.md"),
            "ignored subtree\n",
        )
        .unwrap();
        let vendor = external.join("package/README.md");
        std::fs::write(&vendor, "# Synthetic vendor\nNot workspace memory.\n").unwrap();
        let vendor_text = external.join("package/NOTICE.txt");
        std::fs::write(&vendor_text, "Synthetic external notice.\n").unwrap();
        let alias = root.join("projects/example/evidence/cargo/registry");
        symlink(&external, &alias).unwrap();
        let before_link = std::fs::read_link(&alias).unwrap();
        let before_vendor = std::fs::read(&vendor).unwrap();
        let before_text = std::fs::read(&vendor_text).unwrap();
        let before_canonical = std::fs::read(&canonical).unwrap();

        let mut visited = Vec::new();
        let selected = get_indexable_files_with_observer(&root, |path| {
            visited.push(path.strip_prefix(&root).unwrap().to_path_buf());
        })
        .expect("complete no-follow discovery");

        assert!(std::fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_link(&alias).unwrap(), before_link);
        assert_eq!(std::fs::read(&vendor).unwrap(), before_vendor);
        assert_eq!(std::fs::read(&vendor_text).unwrap(), before_text);
        assert_eq!(std::fs::read(&canonical).unwrap(), before_canonical);
        assert!(
            !root.join(".hex").exists(),
            "discovery creates no runtime state"
        );
        let selected: BTreeMap<_, _> = selected
            .into_iter()
            .map(|(path, strategy)| (path.strip_prefix(&root).unwrap().to_path_buf(), strategy))
            .collect();
        assert_eq!(
            selected.get(Path::new("CLAUDE.md")),
            Some(&"full".to_string())
        );
        assert_eq!(
            selected.get(Path::new("projects/example/context.md")),
            Some(&"full".to_string())
        );
        assert!(
            !selected
                .keys()
                .any(|path| path.starts_with("projects/example/evidence/cargo/registry")),
            "dependency symlink pulled external registry into discovery: {:?}",
            selected.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            selected.len(),
            2,
            "ordinary sources preserved; external aliases rejected"
        );
        let relative_alias = Path::new("projects/example/evidence/cargo/registry");
        assert!(visited.iter().any(|path| path == relative_alias));
        assert!(
            !visited
                .iter()
                .any(|path| path.starts_with(relative_alias.join("package"))),
            "discovery observed a descendant after rejecting the directory link"
        );
        let skipped = Path::new("projects/example/.git");
        assert!(visited.iter().any(|path| path == skipped));
        assert!(
            !visited
                .iter()
                .any(|path| path != skipped && path.starts_with(skipped)),
            "discovery observed a descendant after rejecting a skipped directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn memory_source_eligibility_preserves_caller_root_alias_authority() {
        use std::collections::BTreeSet;
        use std::os::unix::fs::symlink;

        let fixture = TempDir::new().unwrap();
        let physical_parent = fixture.path().join("physical-parent");
        let physical_root = physical_parent.join("workspace");
        std::fs::create_dir_all(physical_root.join("me")).unwrap();
        std::fs::write(physical_root.join("CLAUDE.md"), "root\n").unwrap();
        std::fs::write(physical_root.join("me/me.md"), "nested\n").unwrap();

        let direct_alias = fixture.path().join("direct-workspace-alias");
        symlink(&physical_root, &direct_alias).unwrap();
        let ancestor_alias = fixture.path().join("ancestor-alias");
        symlink(&physical_parent, &ancestor_alias).unwrap();
        let rooted_below_alias = ancestor_alias.join("workspace");

        for root in [&direct_alias, &rooted_below_alias] {
            let selected = get_indexable_files(root).expect("caller root alias is authoritative");
            let relative: BTreeSet<_> = selected
                .into_iter()
                .map(|(path, _)| {
                    assert!(
                        path.starts_with(root),
                        "discovery changed caller path spelling"
                    );
                    path.strip_prefix(root).unwrap().to_path_buf()
                })
                .collect();
            assert_eq!(
                relative,
                [PathBuf::from("CLAUDE.md"), PathBuf::from("me/me.md")]
                    .into_iter()
                    .collect()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn memory_source_eligibility_rejects_directory_file_and_root_aliases() {
        use std::collections::BTreeSet;
        use std::os::unix::fs::symlink;

        let fixture = TempDir::new().unwrap();
        let root = fixture.path().join("workspace");
        let external = fixture.path().join("external");
        std::fs::create_dir_all(root.join("projects/example/canonical-dir")).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "canonical root\n").unwrap();
        std::fs::write(
            root.join("projects/example/context.md"),
            "canonical project\n",
        )
        .unwrap();
        std::fs::write(
            root.join("projects/example/canonical-dir/inside.md"),
            "canonical directory\n",
        )
        .unwrap();
        std::fs::write(external.join("outside.md"), "external file\n").unwrap();

        let links = [
            (external.clone(), root.join("projects/example/external-dir")),
            (
                root.join("projects/example/canonical-dir"),
                root.join("projects/example/in-root-dir"),
            ),
            (
                external.join("outside.md"),
                root.join("projects/example/external-file.md"),
            ),
            (
                root.join("projects/example/context.md"),
                root.join("projects/example/in-root-file.md"),
            ),
            (root.join("CLAUDE.md"), root.join("AGENTS.md")),
        ];
        for (target, link) in &links {
            symlink(target, link).unwrap();
        }
        let before: Vec<_> = links
            .iter()
            .map(|(_, link)| (link.clone(), std::fs::read_link(link).unwrap()))
            .collect();

        let selected = get_indexable_files(&root).expect("complete alias matrix discovery");
        let selected: BTreeSet<_> = selected
            .into_iter()
            .map(|(path, _)| path.strip_prefix(&root).unwrap().to_path_buf())
            .collect();

        assert_eq!(
            selected,
            [
                PathBuf::from("CLAUDE.md"),
                PathBuf::from("projects/example/canonical-dir/inside.md"),
                PathBuf::from("projects/example/context.md"),
            ]
            .into_iter()
            .collect()
        );
        for (link, target) in before {
            assert_eq!(std::fs::read_link(link).unwrap(), target);
        }
        assert_eq!(
            std::fs::read(external.join("outside.md")).unwrap(),
            b"external file\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn memory_source_eligibility_rejects_configured_and_intermediate_root_aliases() {
        use std::os::unix::fs::symlink;

        let fixture = TempDir::new().unwrap();
        let root = fixture.path().join("workspace");
        let external_me = fixture.path().join("external-me");
        let external_raw = fixture.path().join("external-raw");
        std::fs::create_dir_all(root.join("projects/example")).unwrap();
        std::fs::create_dir_all(&external_me).unwrap();
        std::fs::create_dir_all(external_raw.join("research")).unwrap();
        std::fs::write(root.join("projects/example/context.md"), "canonical\n").unwrap();
        std::fs::write(external_me.join("me.md"), "external me\n").unwrap();
        std::fs::write(external_raw.join("research/paper.md"), "external raw\n").unwrap();
        symlink(&external_me, root.join("me")).unwrap();
        symlink(&external_raw, root.join("raw")).unwrap();

        let selected = get_indexable_files(&root).expect("reject aliased configured roots");
        let relative: Vec<_> = selected
            .into_iter()
            .map(|(path, _)| path.strip_prefix(&root).unwrap().to_path_buf())
            .collect();

        assert_eq!(relative, [PathBuf::from("projects/example/context.md")]);
        assert_eq!(std::fs::read_link(root.join("me")).unwrap(), external_me);
        assert_eq!(std::fs::read_link(root.join("raw")).unwrap(), external_raw);
    }

    #[cfg(unix)]
    #[test]
    fn memory_source_eligibility_rejects_broken_and_cycle_links_without_descent() {
        use std::os::unix::fs::symlink;

        let fixture = TempDir::new().unwrap();
        let root = fixture.path().join("workspace");
        let project = root.join("projects/example");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("context.md"), "canonical\n").unwrap();
        let broken = project.join("broken.md");
        let cycle = project.join("cycle");
        symlink(project.join("missing.md"), &broken).unwrap();
        symlink(&project, &cycle).unwrap();

        let mut visited = Vec::new();
        let selected = get_indexable_files_with_observer(&root, |path| {
            visited.push(path.strip_prefix(&root).unwrap().to_path_buf());
        })
        .expect("broken and cycle links are entries, not discovery roots");
        let relative: Vec<_> = selected
            .into_iter()
            .map(|(path, _)| path.strip_prefix(&root).unwrap().to_path_buf())
            .collect();

        assert_eq!(relative, [PathBuf::from("projects/example/context.md")]);
        assert!(visited.contains(&PathBuf::from("projects/example/broken.md")));
        assert!(visited.contains(&PathBuf::from("projects/example/cycle")));
        let relative_cycle = Path::new("projects/example/cycle");
        assert!(
            !visited
                .iter()
                .any(|path| path != relative_cycle && path.starts_with(relative_cycle)),
            "cycle link was traversed"
        );
    }

    #[test]
    fn memory_source_eligibility_allows_missing_optional_roots() {
        let fixture = TempDir::new().unwrap();
        let root = fixture.path().join("workspace");
        std::fs::create_dir(&root).unwrap();

        assert!(get_indexable_files(&root)
            .expect("missing configured roots are optional")
            .is_empty());
    }

    #[test]
    fn memory_source_eligibility_propagates_metadata_errors() {
        let fixture = TempDir::new().unwrap();
        let root = fixture.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "ordinary root file\n").unwrap();
        std::fs::write(root.join("me"), "not a directory\n").unwrap();

        let mut visited = Vec::new();
        let error = get_indexable_files_with_observer(&root, |path| {
            visited.push(path.strip_prefix(&root).unwrap().to_path_buf());
        })
        .expect_err("configured source metadata error must fail discovery");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error
            .to_string()
            .contains("configured memory source component"));
        assert!(visited.contains(&PathBuf::from("CLAUDE.md")));
        assert!(!root.join(".hex").exists());
    }

    #[test]
    fn test_extract_summaries_handles_case_folding_length_change() {
        // 'İ' (U+0130, Latin Capital Letter I with Dot Above) is 2 bytes in UTF-8.
        // Rust's `to_lowercase()` turns it into "i" + a combining dot above
        // (U+0307) — 3 bytes total, one byte longer per occurrence.
        //
        // Regression guard: extract_summaries() used to find the ECC:SUMMARY
        // marker offsets in `content.to_lowercase()` and then reuse those
        // offsets to slice the ORIGINAL `content`. Enough of these chars ahead
        // of a marker desyncs the byte offsets between the two strings, so the
        // slice landed on the wrong span (or out of bounds / mid-char,
        // panicking). Offsets must now come from `content` itself via find_ci.
        let prefix = "İ".repeat(30);
        let content = format!(
            "{prefix}\n<!-- ECC:SUMMARY:START -->\nThe actual summary text.\n<!-- ECC:SUMMARY:END -->\n"
        );
        let result = extract_summaries(&content);
        assert_eq!(result.trim(), "The actual summary text.");
    }

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = content_hash("hello world");
        let h2 = content_hash("hello world");
        let h3 = content_hash("different");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        // SHA256 hex is 64 chars
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn test_init_db_creates_vec_and_private_schema() {
        let tmp = TempDir::new().unwrap();
        let conn = super::super::open_db(&tmp.path().join("memory.db")).unwrap();
        init_db(&conn).unwrap();

        // chunks FTS5 now has a `private` column (prepare validates it).
        assert!(conn.prepare("SELECT private FROM chunks LIMIT 0").is_ok());
        // vec_chunks vec0 table exists.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get(0))
            .unwrap();
    }

    // ── H-09 / OBS-019: per-batch vector persistence ────────────────────────
    // The 2026-06-12 wedge-kill left a whole transcript's 83 chunks vector-less
    // because `index_file` accumulated ALL of a file's vectors and inserted them
    // only after the last batch embedded — a SIGTERM mid-file therefore lost
    // every vector. `embed_and_store` now persists each EMBED_BATCH-sized batch
    // BEFORE embedding the next, so an interruption loses at most one batch (the
    // rest are repaired by `backfill_missing_vectors`). The injected embed
    // closure lets these run without the ONNX model.

    fn vec_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn embed_and_store_persists_earlier_batches_when_a_later_batch_fails() {
        let tmp = TempDir::new().unwrap();
        let conn = super::super::open_db(&tmp.path().join("memory.db")).unwrap();
        init_db(&conn).unwrap();

        // 20 chunks → batches of 8, 8, 4. Fail on the 2nd batch.
        let rowids: Vec<i64> = (1..=20).collect();
        let contents: Vec<String> = (0..20).map(|i| format!("chunk {i}")).collect();
        let mut calls = 0;
        let stored = embed_and_store(&conn, "test.md", &rowids, &contents, |batch| {
            calls += 1;
            if calls == 2 {
                anyhow::bail!("simulated interruption on batch 2");
            }
            Ok(batch
                .iter()
                .map(|_| vec![0.1f32; super::super::vector::EMBED_DIM])
                .collect())
        });

        // Batch 1 (8 chunks) committed before batch 2 failed. The pre-fix code
        // stored 0 here (insert ran only after the whole file embedded).
        assert_eq!(stored, 8, "only the first batch should be stored");
        assert_eq!(
            vec_count(&conn),
            8,
            "batch-1 vectors must be durable despite batch-2 failure"
        );
    }

    #[test]
    fn embed_and_store_persists_all_batches_on_success() {
        let tmp = TempDir::new().unwrap();
        let conn = super::super::open_db(&tmp.path().join("memory.db")).unwrap();
        init_db(&conn).unwrap();

        let rowids: Vec<i64> = (1..=20).collect();
        let contents: Vec<String> = (0..20).map(|i| format!("chunk {i}")).collect();
        let stored = embed_and_store(&conn, "test.md", &rowids, &contents, |batch| {
            Ok(batch
                .iter()
                .map(|_| vec![0.2f32; super::super::vector::EMBED_DIM])
                .collect())
        });

        assert_eq!(stored, 20);
        assert_eq!(vec_count(&conn), 20);
    }

    #[test]
    fn embed_and_store_is_loud_and_skips_only_the_mismatched_batch() {
        // S6: a per-batch count mismatch must not silently drop chunks, and must
        // not poison the other batches. Batch 2 returns the wrong vector count.
        let tmp = TempDir::new().unwrap();
        let conn = super::super::open_db(&tmp.path().join("memory.db")).unwrap();
        init_db(&conn).unwrap();

        let rowids: Vec<i64> = (1..=20).collect();
        let contents: Vec<String> = (0..20).map(|i| format!("chunk {i}")).collect();
        let mut calls = 0;
        let stored = embed_and_store(&conn, "test.md", &rowids, &contents, |batch| {
            calls += 1;
            let n = if calls == 2 {
                batch.len() - 1
            } else {
                batch.len()
            };
            Ok((0..n)
                .map(|_| vec![0.3f32; super::super::vector::EMBED_DIM])
                .collect())
        });

        // Batches 1 (8) and 3 (4) stored; batch 2 skipped loudly → 12 total.
        assert_eq!(
            stored, 12,
            "mismatched batch 2 skipped; batches 1 and 3 stored"
        );
        assert_eq!(vec_count(&conn), 12);
    }

    #[test]
    fn embed_and_store_skips_overcount_batch_without_truncating() {
        // An embedder returning MORE vectors than the batch must be caught by the
        // count-equality guard and skipped — NOT silently truncated via zip.
        let tmp = TempDir::new().unwrap();
        let conn = super::super::open_db(&tmp.path().join("memory.db")).unwrap();
        init_db(&conn).unwrap();

        let rowids: Vec<i64> = (1..=8).collect();
        let contents: Vec<String> = (0..8).map(|i| format!("chunk {i}")).collect();
        let stored = embed_and_store(&conn, "test.md", &rowids, &contents, |batch| {
            Ok((0..batch.len() + 1) // one too many
                .map(|_| vec![0.4f32; super::super::vector::EMBED_DIM])
                .collect())
        });

        assert_eq!(stored, 0, "over-count batch must be skipped, not truncated");
        assert_eq!(vec_count(&conn), 0);
    }

    #[test]
    fn embed_and_store_handles_empty_and_exact_multiple_boundaries() {
        let tmp = TempDir::new().unwrap();
        let conn = super::super::open_db(&tmp.path().join("memory.db")).unwrap();
        init_db(&conn).unwrap();

        // 0 chunks → no work, no panic (empty slices → zip yields nothing).
        let s0 = embed_and_store(&conn, "empty.md", &[], &[], |batch| {
            Ok(batch
                .iter()
                .map(|_| vec![0.5f32; super::super::vector::EMBED_DIM])
                .collect())
        });
        assert_eq!(s0, 0);

        // Exact multiple of 8 (16 → two full batches, no trailing partial).
        let rowids: Vec<i64> = (1..=16).collect();
        let contents: Vec<String> = (0..16).map(|i| format!("c{i}")).collect();
        let s16 = embed_and_store(&conn, "sixteen.md", &rowids, &contents, |batch| {
            Ok(batch
                .iter()
                .map(|_| vec![0.5f32; super::super::vector::EMBED_DIM])
                .collect())
        });
        assert_eq!(s16, 16);
        assert_eq!(vec_count(&conn), 16);
    }

    #[test]
    fn test_is_private_paths() {
        assert!(is_private("me/decisions/foo.md"));
        assert!(is_private("people/alice/profile.md"));
        assert!(is_private("raw/transcripts/2026-05-01.md"));
        assert!(!is_private("projects/foo/context.md"));
        assert!(!is_private("CLAUDE.md"));
    }

    #[test]
    fn test_should_skip_excludes_archives() {
        // Archived projects must NOT be indexed. A big archive sweep (e.g. moving dead
        // projects under projects/_archive/) otherwise triggers a multi-hour re-embed and
        // pollutes search with dead content (cf. me/decisions/prune-archived-projects-2026-06-06).
        assert!(should_skip("projects/_archive/foo/context.md"));
        assert!(should_skip("projects/_archive/integrations-bakeoff/x.md"));
        assert!(should_skip("hex-archive/projects/foo/bar.md"));
        // Active (non-archived) content is still indexed.
        assert!(!should_skip("projects/active-thing/context.md"));
        assert!(!should_skip("me/decisions/x.md"));
    }

    // ── Orphan-vector invariant lock (Task 1, 2026-06-13) ───────────────────
    // The ~1834 production orphans (vec_chunks rows with no chunks row) were
    // LEGACY residue from V1's 74k-orphan era — NOT a live re-index leak. The
    // count is invariant across re-indexing, and `delete_chunks_for_file`
    // deletes vec_chunks rows BEFORE the chunks rows, keyed on file_id, so no
    // re-index / file-deletion ordering can strand a vector (a mid-run kill
    // yields at worst chunks-WITHOUT-vecs — the H-09 direction, self-healed by
    // backfill). These tests LOCK that invariant: they pass on current code by
    // design and fail the day a refactor drops the `delete_vecs` call (the V1
    // bug) or mis-keys the rowid collection. No ONNX — chunks + matching vecs
    // are inserted directly, mirroring index_file's on-disk layout.

    fn existing_file_id(conn: &Connection, path: &str) -> Option<i64> {
        conn.query_row("SELECT id FROM files WHERE path = ?", params![path], |r| {
            r.get(0)
        })
        .ok()
    }

    fn orphan_vec_count(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM vec_chunks WHERE rowid NOT IN (SELECT rowid FROM chunks)",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Mirror `index_file`'s "remove old record + chunks, then insert fresh"
    /// flow (index.rs ~513-555 + the embed loop), writing a vec per chunk at the
    /// chunk's own rowid exactly as the real path does — but without ONNX.
    fn reindex(conn: &Connection, path: &str, n: usize) {
        if let Some(fid) = existing_file_id(conn, path) {
            delete_chunks_for_file(conn, fid).unwrap();
            conn.execute("DELETE FROM files WHERE id = ?", params![fid])
                .unwrap();
        }
        conn.execute(
            "INSERT INTO files (path, mtime, content_hash, indexed_at, chunk_count) \
             VALUES (?, 0.0, 'h', '', ?)",
            params![path, n as i64],
        )
        .unwrap();
        let fid: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .unwrap();
        for i in 0..n {
            conn.execute(
                "INSERT INTO chunks (file_id, source_path, heading, chunk_index, content, private) \
                 VALUES (?, ?, '', ?, ?, 0)",
                params![fid.to_string(), path, i.to_string(), format!("chunk {i} of {path}")],
            )
            .unwrap();
            let rowid: i64 = conn
                .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
                .unwrap();
            conn.execute(
                "INSERT INTO chunk_meta (chunk_rowid, source_weight) VALUES (?, 1.0)",
                params![rowid],
            )
            .unwrap();
            super::super::vector::insert_vec(
                conn,
                rowid,
                &vec![0.1f32; super::super::vector::EMBED_DIM],
            )
            .unwrap();
        }
    }

    #[test]
    fn reindex_cycles_leave_no_orphan_vectors() {
        let tmp = TempDir::new().unwrap();
        let conn = super::super::open_db(&tmp.path().join("memory.db")).unwrap();
        init_db(&conn).unwrap();

        // Two files; A is inserted last so it owns the highest rowids.
        reindex(&conn, "b.md", 3);
        reindex(&conn, "a.md", 5);
        assert_eq!(vec_count(&conn), 8);
        assert_eq!(orphan_vec_count(&conn), 0);

        // Re-index A SHRUNK (5 → 2): frees its max rowids; new chunks reuse the
        // freed rowids. The old vecs must already be gone (delete-before-insert).
        reindex(&conn, "a.md", 2);
        assert_eq!(
            orphan_vec_count(&conn),
            0,
            "reindex-shrink stranded a vector"
        );
        assert_eq!(vec_count(&conn), 5);

        // Re-index A GROWN (2 → 6).
        reindex(&conn, "a.md", 6);
        assert_eq!(orphan_vec_count(&conn), 0, "reindex-grow stranded a vector");
        assert_eq!(vec_count(&conn), 9);

        // File-removal cleanup path (run_index: delete_chunks_for_file + DELETE
        // files for a path no longer on disk).
        let fid_b = existing_file_id(&conn, "b.md").unwrap();
        delete_chunks_for_file(&conn, fid_b).unwrap();
        conn.execute("DELETE FROM files WHERE id = ?", params![fid_b])
            .unwrap();
        assert_eq!(
            orphan_vec_count(&conn),
            0,
            "file deletion stranded a vector"
        );
        assert_eq!(vec_count(&conn), 6, "only A's six vectors remain");
    }

    #[test]
    fn vec_gap_message_distinguishes_deficit_surplus_and_match() {
        assert_eq!(vec_gap_message(10, 10), None, "equal counts → no message");

        let deficit = vec_gap_message(10, 7).expect("deficit must report");
        assert!(
            deficit.contains("3 chunk(s) without a vector"),
            "got: {deficit}"
        );

        // The real production shape: 16688 chunks, 18522 vecs → 1834 orphans.
        let surplus = vec_gap_message(16688, 18522).expect("surplus must report");
        assert!(surplus.contains("1834 orphan vector(s)"), "got: {surplus}");
        assert!(surplus.contains("surplus"), "got: {surplus}");
    }

    #[test]
    fn run_budget_default_disable_tune_and_garbage_fallback() {
        // HEX_INDEX_BUDGET_SECS is read by no other test, so this set/remove is
        // isolated despite cargo's parallel test threads.
        std::env::remove_var("HEX_INDEX_BUDGET_SECS");
        assert_eq!(
            run_budget(),
            Some(std::time::Duration::from_secs(600)),
            "default 10min"
        );
        std::env::set_var("HEX_INDEX_BUDGET_SECS", "0");
        assert_eq!(run_budget(), None, "0 disables the cap");
        std::env::set_var("HEX_INDEX_BUDGET_SECS", "30");
        assert_eq!(
            run_budget(),
            Some(std::time::Duration::from_secs(30)),
            "explicit value honored"
        );
        std::env::set_var("HEX_INDEX_BUDGET_SECS", "notanumber");
        assert_eq!(
            run_budget(),
            Some(std::time::Duration::from_secs(600)),
            "garbage falls back to the default, never panics"
        );
        std::env::remove_var("HEX_INDEX_BUDGET_SECS");
    }

    // Codifies the e2e bail contract verified manually against the built binary
    // (budget=3s on a 25-file --full reindex bailed after 2 files with exit 1).
    // Model-dependent (run_index loads ONNX) → #[ignore]; run with --ignored.
    #[test]
    #[ignore]
    fn run_index_over_budget_exits_nonzero() {
        use std::io::Write;
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join("me/decisions")).unwrap();
        std::fs::write(hex_root.join("CLAUDE.md"), "# ws\n").unwrap();
        for n in 0..20 {
            let mut f =
                std::fs::File::create(hex_root.join(format!("me/decisions/d{n}.md"))).unwrap();
            writeln!(
                f,
                "# Doc {n}\n\nContent about hex memory indexing.\n\n## More\nText."
            )
            .unwrap();
        }
        // A 1s budget is below the cold model-load (~2s), so the very first
        // top-of-loop check is already over budget → loud bail, non-zero exit.
        std::env::set_var("HEX_INDEX_BUDGET_SECS", "1");
        let code = run_index(hex_root, true);
        std::env::remove_var("HEX_INDEX_BUDGET_SECS");
        assert_eq!(
            code, 1,
            "an over-budget run must exit non-zero (S6 loud bail)"
        );
    }

    #[test]
    #[ignore] // model-dependent — run with --ignored
    fn test_index_file_writes_vectors() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join(".hex")).unwrap();
        let conn = super::super::open_db(&hex_root.join(".hex/memory.db")).unwrap();
        init_db(&conn).unwrap();
        let embedder = super::super::embed::Embedder::new(hex_root).unwrap();

        let f = hex_root.join("me/decisions/x.md");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        let content = "# Decision\nWe chose sqlite-vec for the vector store.";
        std::fs::write(&f, content).unwrap();

        let n = index_file(&conn, &f, hex_root, content, 0.0, "full", &embedder).unwrap();
        assert!(n >= 1);
        let vec_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vec_count, n as i64, "every chunk gets a vector");
        // private flag was stored
        let priv_flag: i64 = conn
            .query_row("SELECT private FROM chunks LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(priv_flag, 1, "me/decisions/ is private");
    }

    #[test]
    fn test_init_db_creates_schema() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("memory.db");
        let conn = super::super::open_db(&db_path).unwrap();
        init_db(&conn).unwrap();

        // Verify files table
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        // Verify chunks FTS5 table
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .unwrap();
        // Verify chunk_meta table
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunk_meta", [], |r| r.get(0))
            .unwrap();
        // Verify metadata table
        let val: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_migrated_chunks_v3'",
                [],
                |r| r.get(0),
            )
            .ok();
        assert_eq!(val.as_deref(), Some("1"));
    }

    #[test]
    #[ignore] // run_index now builds an embedder and loads the model
    fn test_incremental_index_skips_unchanged() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        let hex_dir = hex_root.join(".hex");
        std::fs::create_dir_all(&hex_dir).unwrap();

        // Create a CLAUDE.md so it looks like a hex root
        std::fs::write(hex_root.join("CLAUDE.md"), "# Hex").unwrap();

        // Create a simple markdown file
        let me_dir = hex_root.join("me");
        std::fs::create_dir_all(&me_dir).unwrap();
        let test_file = me_dir.join("test.md");
        std::fs::write(&test_file, "# Test\nSome content here.").unwrap();

        // First index run
        let code1 = run_index(hex_root, false);
        assert_eq!(code1, 0);

        let db = Connection::open(hex_dir.join("memory.db")).unwrap();
        // test.md should have been indexed with at least 1 chunk
        let test_chunks: i64 = db
            .query_row(
                "SELECT chunk_count FROM files WHERE path LIKE '%test.md'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(test_chunks > 0);
        let files_after_run1: i64 = db
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();

        // Second run — files unchanged, should skip at mtime stage (same file count)
        let code2 = run_index(hex_root, false);
        assert_eq!(code2, 0);
        let files_after_run2: i64 = db
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files_after_run2, files_after_run1);
    }

    #[test]
    #[ignore] // now requires the embedding model
    fn test_index_file_inserts_chunks_and_meta() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        let hex_dir = hex_root.join(".hex");
        std::fs::create_dir_all(&hex_dir).unwrap();

        let db_path = hex_dir.join("memory.db");
        let conn = super::super::open_db(&db_path).unwrap();
        init_db(&conn).unwrap();

        let test_file = hex_root.join("test.md");
        let content = "# Hello\nWorld content here.\n## More\nExtra info.";
        std::fs::write(&test_file, content).unwrap();

        let embedder = super::super::embed::Embedder::new(hex_root).unwrap();
        let n = index_file(&conn, &test_file, hex_root, content, 0.0, "full", &embedder).unwrap();
        assert!(n >= 2);

        let chunk_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(chunk_count, n as i64);

        let meta_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunk_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(meta_count, n as i64);
    }
}
