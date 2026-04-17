#!/usr/bin/env python3
# sync-safe
"""
Memory Indexer — Indexes all markdown and text files into SQLite FTS5 for search.

Usage:
    python3 memory_index.py              # Incremental index (only changed files)
    python3 memory_index.py --full       # Full reindex
    python3 memory_index.py --stats      # Show index stats

Part of the hex memory system.

Incremental strategy (two-stage):
  1. mtime pre-filter — skip files whose mtime hasn't changed (fast, avoids reads)
  2. SHA-256 content hash — skip files whose content is identical even if mtime changed
  Only files with both a new mtime AND new content hash get re-indexed.

Tiered indexing:
  - Curated files (me/, projects/, people/, evolution/, landings/): full index
  - raw/research, raw/captures: full index (unique content)
  - raw/transcripts (last 7 days): full index (hot buffer)
  - raw/transcripts (older): summary sections only
  - raw/reflect-runs, raw/handoffs, raw/reflections: excluded (process artifacts)

Source weighting:
  Each chunk gets a source_weight stored in chunk_meta table.
  Applied as a multiplier on BM25 scores at query time.

Chunk deduplication:
  Within each file, duplicate chunks (same heading + content hash) are skipped.

Hybrid search (Phase 2):
  If sqlite-vec and fastembed are installed, chunks get 384-dim embeddings stored
  in a vec_chunks virtual table. Search merges FTS5 BM25 + vector cosine via RRF.
  Falls back gracefully to FTS5-only if deps are missing.

  To force FTS5-only mode (e.g. for testing or offline use), set:
    HEX_DISABLE_VECTORS=1 python3 memory_index.py
"""

import hashlib
import json
import os
import sys
import sqlite3
import re
import time
from pathlib import Path
from datetime import datetime


# --- Optional hybrid search deps ---
# Force FTS5-only via env var (useful for testing, offline installs, or sandboxes)
_VECTORS_DISABLED = os.environ.get("HEX_DISABLE_VECTORS", "0") == "1"

if not _VECTORS_DISABLED:
    try:
        import sqlite_vec
        HAS_VEC = True
    except ImportError:
        HAS_VEC = False
        print("  NOTE: sqlite-vec not available — using FTS5-only mode", file=sys.stderr)
else:
    HAS_VEC = False

if not _VECTORS_DISABLED:
    try:
        from fastembed import TextEmbedding
        HAS_EMBED = True
    except ImportError:
        HAS_EMBED = False
        print("  NOTE: fastembed not available — using FTS5-only mode", file=sys.stderr)
else:
    HAS_EMBED = False

# True only when both vector deps are live
HYBRID_AVAILABLE = HAS_VEC and HAS_EMBED

EMBED_MODEL = "BAAI/bge-small-en-v1.5"  # 384 dims, ~33MB
EMBED_DIM = 384
_embedder = None


def _get_embedder():
    """Lazy-init the embedding model."""
    global _embedder
    if _embedder is None and HAS_EMBED:
        _embedder = TextEmbedding(model_name=EMBED_MODEL)
    return _embedder


def _embed_texts(texts: list) -> list:
    """Embed a list of texts. Returns list of float lists (384-dim each)."""
    embedder = _get_embedder()
    if embedder is None:
        return []
    embeddings = list(embedder.embed(texts))
    return [e.tolist() for e in embeddings]


def _find_root():
    """Walk up from script location to find the hex root (has CLAUDE.md)."""
    d = Path(__file__).resolve().parent
    for _ in range(6):
        if (d / "CLAUDE.md").exists():
            return d
        d = d.parent
    return Path(__file__).resolve().parent.parent


HEX_ROOT = _find_root()
DB_PATH = HEX_ROOT / ".hex" / "memory.db"

# Directories to index (relative to HEX_ROOT)
# "raw" is handled specially via TIERED_RAW_DIRS below
INDEX_DIRS = [
    ".",            # Root files (todo.md, etc.)
    "me",           # Personal context, learnings
    "projects",     # Project docs
    "people",       # Relationship profiles
    "evolution",    # Improvement engine files
    "landings",     # Daily landing targets
]

# Raw subdirectories with their indexing strategy
# "full" = index everything, "summary" = summary sections only, "exclude" = skip
TIERED_RAW_DIRS = {
    "raw/research":     "full",
    "raw/captures":     "full",
    "raw/transcripts":  "tiered",   # recent=full, old=summary
    "raw/reflect-runs": "exclude",
    "raw/handoffs":     "exclude",
    "raw/reflections":  "exclude",
    "raw/docs":         "full",
    "raw/meeting-prep": "full",
    "raw/messages":     "full",
    "raw/calendar":     "full",
}

# Transcripts newer than this get full indexing; older get summary-only
TRANSCRIPT_HOT_DAYS = 7

# Files/dirs to skip
SKIP_PATTERNS = [
    ".hex",
    ".claude",
    ".sessions",
    "node_modules",
    ".git",
]

# Chunking config
MAX_CHUNK_WORDS = 400
OVERLAP_WORDS = 80

# Source weight assignments for chunk_meta
SOURCE_WEIGHTS = {
    "me/decisions/": 1.5,
    "people/":       1.5,
    "me/":           1.2,
    "projects/":     1.2,
    "evolution/":    1.2,
    "landings/":     1.0,
    "raw/research":  1.0,
    "raw/captures":  0.8,
}
# Defaults for transcripts
TRANSCRIPT_WEIGHT_RECENT = 0.5
TRANSCRIPT_WEIGHT_OLD = 0.3
DEFAULT_WEIGHT = 1.0


def should_skip(path: Path) -> bool:
    """Check if a file should be skipped."""
    rel = str(path.relative_to(HEX_ROOT))
    for pattern in SKIP_PATTERNS:
        if rel.startswith(pattern) or f"/{pattern}" in rel:
            return True
    return False


def _content_hash(content: str) -> str:
    """SHA-256 hex digest of file content."""
    return hashlib.sha256(content.encode("utf-8")).hexdigest()


def _get_source_weight(rel_path: str, is_old_transcript: bool = False) -> float:
    """Determine source weight for a file path."""
    if rel_path.startswith("raw/transcripts"):
        return TRANSCRIPT_WEIGHT_OLD if is_old_transcript else TRANSCRIPT_WEIGHT_RECENT
    for prefix, weight in SOURCE_WEIGHTS.items():
        if rel_path.startswith(prefix):
            return weight
    return DEFAULT_WEIGHT


def _extract_summaries(content: str) -> str:
    """Extract summary sections from a transcript for summary-only indexing.

    Looks for:
    - <!-- ECC:SUMMARY:START --> ... <!-- ECC:SUMMARY:END --> blocks
    - ## Summary / ## Session Summary headings (take content until next heading)
    - ## Notes for Next Session sections

    Returns extracted text, or empty string if no summaries found.
    """
    extracted = []

    # Extract ECC:SUMMARY blocks
    ecc_pattern = re.compile(
        r'<!--\s*ECC:SUMMARY:START\s*-->(.*?)<!--\s*ECC:SUMMARY:END\s*-->',
        re.DOTALL
    )
    for match in ecc_pattern.finditer(content):
        extracted.append(match.group(1).strip())

    # Extract ## Summary / ## Session Summary sections
    lines = content.split("\n")
    in_summary = False
    summary_lines = []
    for line in lines:
        heading_match = re.match(r"^(#{1,3})\s+(.+)$", line)
        if heading_match:
            heading_text = heading_match.group(2).strip().lower()
            if in_summary and summary_lines:
                extracted.append("\n".join(summary_lines).strip())
                summary_lines = []
            in_summary = heading_text in (
                "summary", "session summary", "notes for next session",
                "tasks", "files modified", "stats",
            )
            if in_summary:
                summary_lines.append(line)
        elif in_summary:
            summary_lines.append(line)

    if in_summary and summary_lines:
        extracted.append("\n".join(summary_lines).strip())

    return "\n\n".join(extracted)


def _is_old_transcript(filepath: Path) -> bool:
    """Check if a transcript file is older than the hot window."""
    try:
        # Try to parse date from filename (YYYY-MM-DD.md)
        stem = filepath.stem
        file_date = datetime.strptime(stem, "%Y-%m-%d")
        age_days = (datetime.now() - file_date).days
        return age_days > TRANSCRIPT_HOT_DAYS
    except ValueError:
        # Can't parse date, use mtime
        age_days = (time.time() - filepath.stat().st_mtime) / 86400
        return age_days > TRANSCRIPT_HOT_DAYS


def chunk_by_heading(content: str, source_path: str, deduplicate: bool = True) -> list:
    """Split markdown content into chunks by heading.

    If deduplicate=True, skip chunks with duplicate (heading, content_hash)
    within the same file. This eliminates repeated spec prompts in transcripts.
    """
    lines = content.split("\n")
    chunks = []
    current_heading = "(top)"
    current_lines = []

    for line in lines:
        heading_match = re.match(r"^(#{1,4})\s+(.+)$", line)
        if heading_match:
            if current_lines:
                text = "\n".join(current_lines).strip()
                if text:
                    chunks.append({"heading": current_heading, "content": text})
            current_heading = heading_match.group(2).strip()
            current_lines = [line]
        else:
            current_lines.append(line)

    if current_lines:
        text = "\n".join(current_lines).strip()
        if text:
            chunks.append({"heading": current_heading, "content": text})

    # Split large chunks further
    split_chunks = []
    for chunk in chunks:
        words = chunk["content"].split()
        if len(words) <= MAX_CHUNK_WORDS:
            split_chunks.append(chunk)
        else:
            i = 0
            sub_idx = 0
            while i < len(words):
                end = min(i + MAX_CHUNK_WORDS, len(words))
                sub_content = " ".join(words[i:end])
                split_chunks.append({
                    "heading": chunk["heading"] + (f" (part {sub_idx + 1})" if sub_idx > 0 else ""),
                    "content": sub_content,
                })
                sub_idx += 1
                i += MAX_CHUNK_WORDS - OVERLAP_WORDS

    # Deduplicate within file
    if deduplicate:
        seen = set()
        deduped = []
        for chunk in split_chunks:
            key = (chunk["heading"].lower(), _content_hash(chunk["content"]))
            if key not in seen:
                seen.add(key)
                deduped.append(chunk)
        return deduped

    return split_chunks


def init_db(conn: sqlite3.Connection):
    """Create tables if they don't exist. Migrates existing DBs forward.

    Schema migration notes:
    - v0.1.0 DBs have chunks FTS5 without file_id column. The hybrid indexer
      requires file_id for efficient chunk cleanup. On detection, the chunks
      table is rebuilt transparently and a metadata flag is set to prevent
      re-migration on subsequent runs.
    - chunk_meta and vec_chunks are added lazily (IF NOT EXISTS) so v0.1.0
      DBs are upgraded in-place without a --full reindex requirement for those
      tables. The chunks table rebuild does trigger a full re-index of all files.
    """
    global HAS_VEC, HYBRID_AVAILABLE
    conn.executescript("""
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
    """)

    # --- Schema migration: ensure chunks FTS5 has file_id column ---
    # v0.1.0 created chunks WITHOUT file_id. FTS5 tables cannot be ALTER'd,
    # so we detect the missing column, drop, recreate, and wipe files so the
    # next run does a full re-index automatically.
    existing_chunks = conn.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='chunks'"
    ).fetchone()
    if existing_chunks:
        # Check if file_id is present by inspecting a pragma on the content table.
        # For FTS5, PRAGMA table_info returns shadow tables; use content snippet instead.
        try:
            conn.execute("SELECT file_id FROM chunks LIMIT 0")
            # Column exists — no migration needed
        except sqlite3.OperationalError:
            # file_id missing — v0.1.0 schema, needs migration
            migration_done = conn.execute(
                "SELECT value FROM metadata WHERE key='schema_migrated_chunks_v2'"
            ).fetchone()
            if not migration_done:
                print("  NOTE: Upgrading chunks table schema (v0.1.0 → v0.2.0). Files will be re-indexed.", file=sys.stderr)
                conn.executescript("""
                    DROP TABLE IF EXISTS chunks;
                    DELETE FROM files;
                    DELETE FROM chunk_meta;
                """)
                conn.execute(
                    "INSERT OR REPLACE INTO metadata (key, value) VALUES ('schema_migrated_chunks_v2', '1')"
                )
    else:
        # Fresh install — mark migration done to avoid future false triggers
        conn.execute(
            "INSERT OR IGNORE INTO metadata (key, value) VALUES ('schema_migrated_chunks_v2', '1')"
        )

    # Create chunks FTS5 (with file_id)
    conn.execute("""
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
            file_id,
            source_path,
            heading,
            chunk_index,
            content,
            tokenize='unicode61'
        )
    """)

    # Create vec_chunks table if sqlite-vec is available
    if HAS_VEC:
        try:
            conn.enable_load_extension(True)
            sqlite_vec.load(conn)
            conn.enable_load_extension(False)
            conn.execute(f"""
                CREATE VIRTUAL TABLE IF NOT EXISTS vec_chunks USING vec0(
                    chunk_rowid INTEGER PRIMARY KEY,
                    embedding float[{EMBED_DIM}]
                )
            """)
        except Exception as e:
            # Extension loading can fail in some sandboxes or restricted environments.
            # Degrade gracefully to FTS5-only.
            HAS_VEC = False
            HYBRID_AVAILABLE = False
            print(f"  NOTE: sqlite-vec extension failed to load ({e}) — using FTS5-only mode", file=sys.stderr)

    conn.commit()

    # Migration: add content_hash column if missing (existing DBs)
    cols = {row[1] for row in conn.execute("PRAGMA table_info(files)").fetchall()}
    if "content_hash" not in cols:
        conn.execute("ALTER TABLE files ADD COLUMN content_hash TEXT NOT NULL DEFAULT ''")

    conn.commit()


def _set_metadata(conn: sqlite3.Connection, key: str, value: str):
    conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES (?, ?)",
        (key, value),
    )


def _get_metadata(conn: sqlite3.Connection, key: str):
    row = conn.execute("SELECT value FROM metadata WHERE key = ?", (key,)).fetchone()
    return row[0] if row else None


def get_indexable_files() -> list:
    """Find all markdown and text files to index.

    Returns list of (Path, strategy) tuples where strategy is one of:
    - "full": index all content
    - "summary": index summary sections only (old transcripts)
    """
    files = []

    # Standard directories: full index
    for index_dir in INDEX_DIRS:
        dir_path = HEX_ROOT / index_dir
        if not dir_path.exists():
            continue
        if index_dir == ".":
            for f in dir_path.glob("*.md"):
                if not should_skip(f):
                    files.append((f, "full"))
        else:
            for f in dir_path.rglob("*.md"):
                if not should_skip(f):
                    files.append((f, "full"))
            for f in dir_path.rglob("*.txt"):
                if not should_skip(f):
                    files.append((f, "full"))

    # Tiered raw directories
    for raw_subdir, strategy in TIERED_RAW_DIRS.items():
        dir_path = HEX_ROOT / raw_subdir
        if not dir_path.exists():
            continue
        if strategy == "exclude":
            continue

        for f in dir_path.rglob("*.md"):
            if should_skip(f):
                continue
            if strategy == "tiered":
                # Transcripts: recent = full, old = summary
                if _is_old_transcript(f):
                    files.append((f, "summary"))
                else:
                    files.append((f, "full"))
            else:
                files.append((f, strategy))
        for f in dir_path.rglob("*.txt"):
            if should_skip(f):
                continue
            if strategy == "tiered":
                if _is_old_transcript(f):
                    files.append((f, "summary"))
                else:
                    files.append((f, "full"))
            else:
                files.append((f, strategy))

    return files


def index_file(conn: sqlite3.Connection, filepath: Path, content: str, mtime: float,
               strategy: str = "full") -> int:
    """Index a single file (content already read). Returns number of chunks created.

    strategy: "full" indexes all content, "summary" extracts summary sections only.

    INVARIANT: ``mtime`` must be the value observed *before* ``content`` was
    read (i.e. the stat that triggered the re-index).  Using a fresher mtime
    would hide subsequent modifications and cause the next incremental run to
    skip the file even though its content is stale (TOCTOU race).
    """
    rel_path = str(filepath.relative_to(HEX_ROOT))
    chash = _content_hash(content)

    # Apply summary extraction for old transcripts
    if strategy == "summary":
        content = _extract_summaries(content)
        if not content.strip():
            # No summary sections found; skip this file entirely
            # Still record it in files table so we don't re-check it every run
            row = conn.execute("SELECT id FROM files WHERE path = ?", (rel_path,)).fetchone()
            if row:
                conn.execute("DELETE FROM chunks WHERE file_id = ?", (str(row[0]),))
                conn.execute("DELETE FROM chunk_meta WHERE chunk_rowid IN (SELECT rowid FROM chunks WHERE file_id = ?)", (str(row[0]),))
                conn.execute("DELETE FROM files WHERE id = ?", (row[0],))
            conn.execute(
                "INSERT INTO files (path, mtime, content_hash, indexed_at, chunk_count) VALUES (?, ?, ?, ?, ?)",
                (rel_path, mtime, chash, datetime.now().isoformat(), 0),
            )
            return 0

    is_old_transcript = strategy == "summary"
    chunks = chunk_by_heading(content, rel_path, deduplicate=True)

    # Remove old entry for this file
    row = conn.execute("SELECT id FROM files WHERE path = ?", (rel_path,)).fetchone()
    if row:
        # Clean up chunk_meta for old chunks
        old_rowids = conn.execute(
            "SELECT rowid FROM chunks WHERE file_id = ?", (str(row[0]),)
        ).fetchall()
        if old_rowids:
            placeholders = ",".join("?" for _ in old_rowids)
            conn.execute(
                f"DELETE FROM chunk_meta WHERE chunk_rowid IN ({placeholders})",
                [r[0] for r in old_rowids],
            )
        conn.execute("DELETE FROM chunks WHERE file_id = ?", (str(row[0]),))
        conn.execute("DELETE FROM files WHERE id = ?", (row[0],))

    # Insert file record
    conn.execute(
        "INSERT INTO files (path, mtime, content_hash, indexed_at, chunk_count) VALUES (?, ?, ?, ?, ?)",
        (rel_path, mtime, chash, datetime.now().isoformat(), len(chunks)),
    )
    file_id = conn.execute("SELECT last_insert_rowid()").fetchone()[0]

    # Determine source weight for this file
    weight = _get_source_weight(rel_path, is_old_transcript=is_old_transcript)

    # Insert chunks + chunk_meta
    chunk_rowids = []
    chunk_texts = []
    for i, chunk in enumerate(chunks):
        conn.execute(
            "INSERT INTO chunks (file_id, source_path, heading, chunk_index, content) VALUES (?, ?, ?, ?, ?)",
            (str(file_id), rel_path, chunk["heading"], str(i), chunk["content"]),
        )
        chunk_rowid = conn.execute("SELECT last_insert_rowid()").fetchone()[0]
        conn.execute(
            "INSERT INTO chunk_meta (chunk_rowid, source_weight) VALUES (?, ?)",
            (chunk_rowid, weight),
        )
        chunk_rowids.append(chunk_rowid)
        chunk_texts.append(chunk["content"][:1000])  # Truncate for embedding efficiency

    # Store embeddings if available (batch per file)
    if HYBRID_AVAILABLE and chunk_texts:
        try:
            embeddings = _embed_texts(chunk_texts)
            for rowid, emb in zip(chunk_rowids, embeddings):
                conn.execute(
                    "INSERT INTO vec_chunks (chunk_rowid, embedding) VALUES (?, ?)",
                    (rowid, json.dumps(emb)),
                )
        except Exception as e:
            print(f"  WARNING: embedding failed for {rel_path}: {e}")

    return len(chunks)


def run_index(full: bool = False):
    """Run the indexer.

    Incremental (default):
      Stage 1 — mtime pre-filter: if mtime unchanged, skip without reading file.
      Stage 2 — content hash: read file, compute SHA-256. If hash matches DB, update
                mtime only (no re-chunk). Otherwise re-index.
    Full: re-index every file unconditionally.
    """
    t0 = time.monotonic()
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(DB_PATH))
    conn.execute("PRAGMA journal_mode=WAL")
    init_db(conn)

    # Build lookup of existing file records: path → (mtime, content_hash)
    existing = {}
    for path, mtime, chash in conn.execute("SELECT path, mtime, content_hash FROM files").fetchall():
        existing[path] = (mtime, chash or "")

    file_tuples = get_indexable_files()
    files = [ft[0] for ft in file_tuples]
    strategy_map = {str(ft[0]): ft[1] for ft in file_tuples}
    print(f"Found {len(files)} files to check")

    indexed = 0
    skipped_mtime = 0
    skipped_hash = 0
    total_chunks = 0

    for filepath in files:
        rel_path = str(filepath.relative_to(HEX_ROOT))
        mtime = filepath.stat().st_mtime
        strategy = strategy_map.get(str(filepath), "full")

        if not full:
            prev = existing.get(rel_path)

            # Stage 1: mtime pre-filter (no file read needed)
            if prev and prev[0] == mtime:
                skipped_mtime += 1
                continue

            # mtime changed — read file and check content hash
            try:
                content = filepath.read_text(encoding="utf-8", errors="replace")
            except Exception as e:
                print(f"  SKIP {rel_path}: {e}")
                continue

            if not content.strip():
                continue

            # Stage 2: content hash check
            chash = _content_hash(content)
            if prev and prev[1] and prev[1] == chash:
                # Content identical — just update mtime so next run skips at stage 1
                conn.execute("UPDATE files SET mtime = ? WHERE path = ?", (mtime, rel_path))
                skipped_hash += 1
                continue
        else:
            # Full mode: read unconditionally
            try:
                content = filepath.read_text(encoding="utf-8", errors="replace")
            except Exception as e:
                print(f"  SKIP {rel_path}: {e}")
                continue
            if not content.strip():
                continue

        # Actually re-index this file
        chunks = index_file(conn, filepath, content, mtime, strategy=strategy)
        if chunks > 0:
            indexed += 1
            total_chunks += chunks
            tag = f" [{strategy}]" if strategy != "full" else ""
            print(f"  Indexed: {rel_path} ({chunks} chunks{tag})")
        elif strategy == "summary":
            print(f"  Indexed: {rel_path} (0 chunks, no summaries found [summary])")

    # Clean up files that no longer exist on disk
    all_paths = {str(f.relative_to(HEX_ROOT)) for f in files}
    removed = 0
    for db_path in list(existing.keys()):
        if db_path not in all_paths:
            row = conn.execute("SELECT id FROM files WHERE path = ?", (db_path,)).fetchone()
            if row:
                conn.execute("DELETE FROM chunks WHERE file_id = ?", (str(row[0]),))
                conn.execute("DELETE FROM files WHERE id = ?", (row[0],))
                removed += 1
                print(f"  Removed: {db_path}")

    _set_metadata(conn, "last_run", datetime.now().isoformat())
    _set_metadata(conn, "last_run_mode", "full" if full else "incremental")
    conn.commit()
    conn.close()

    elapsed = time.monotonic() - t0
    print(f"\nDone in {elapsed:.2f}s: {indexed} indexed, "
          f"{skipped_mtime} unchanged (mtime), {skipped_hash} unchanged (hash), "
          f"{removed} removed, {total_chunks} new chunks")


def show_stats():
    """Show index statistics."""
    if not DB_PATH.exists():
        print("No index found. Run without --stats to create one.")
        return

    conn = sqlite3.connect(str(DB_PATH))

    file_count = conn.execute("SELECT COUNT(*) FROM files").fetchone()[0]
    chunk_count = conn.execute("SELECT COUNT(*) FROM chunks").fetchone()[0]
    hashed = conn.execute("SELECT COUNT(*) FROM files WHERE content_hash != ''").fetchone()[0]

    print(f"Database: {DB_PATH}")
    print(f"Size: {DB_PATH.stat().st_size / 1024:.1f} KB")
    print(f"Files indexed: {file_count} ({hashed} with content hash)")
    print(f"Total chunks: {chunk_count}")

    # Vector mode status
    if HYBRID_AVAILABLE:
        try:
            vec_count = conn.execute("SELECT COUNT(*) FROM vec_chunks").fetchone()[0]
            print(f"Vector embeddings: {vec_count} (hybrid mode active)")
        except Exception:
            print("Vector embeddings: table missing (run --full to rebuild)")
    else:
        print("Vector embeddings: disabled (FTS5-only mode)")

    # Metadata
    last_run = _get_metadata(conn, "last_run")
    last_mode = _get_metadata(conn, "last_run_mode")
    if last_run:
        print(f"Last run: {last_run} ({last_mode or 'unknown'})")
    print()

    print("By directory:")
    rows = conn.execute("""
        SELECT
            CASE
                WHEN source_path LIKE '%/%' THEN substr(source_path, 1, instr(source_path, '/') - 1)
                ELSE '(root)'
            END as dir,
            COUNT(DISTINCT source_path) as files,
            COUNT(*) as chunks
        FROM chunks
        GROUP BY dir
        ORDER BY chunks DESC
    """).fetchall()
    for dir_name, files, chunks in rows:
        print(f"  {dir_name}: {files} files, {chunks} chunks")

    conn.close()


if __name__ == "__main__":
    if "--stats" in sys.argv:
        show_stats()
    elif "--full" in sys.argv:
        print("Full reindex...")
        run_index(full=True)
    elif "--help" in sys.argv or "-h" in sys.argv:
        print(__doc__)
        sys.exit(0)
    else:
        print("Incremental index...")
        run_index(full=False)
