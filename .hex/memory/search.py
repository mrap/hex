#!/usr/bin/env python3
"""Search hex memory using SQLite FTS5 full-text search.

Usage:
    python3 .hex/memory/search.py 'authentication middleware'
    python3 .hex/memory/search.py 'auth' --top 5
    python3 .hex/memory/search.py 'auth' --compact
    python3 .hex/memory/search.py 'auth' --context 200
"""

import argparse
import os
import sqlite3
import sys
from datetime import datetime, timezone

DB_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "memory.db")


def _log_search(conn, query):
    """Log search query for stats tracking."""
    c = conn.cursor()
    c.execute(
        "CREATE TABLE IF NOT EXISTS search_log "
        "(id INTEGER PRIMARY KEY AUTOINCREMENT, query TEXT, timestamp TEXT)"
    )
    c.execute(
        "INSERT INTO search_log (query, timestamp) VALUES (?, ?)",
        (query, datetime.now(timezone.utc).isoformat(timespec="seconds")),
    )
    conn.commit()


def _fts_query(c, query, top):
    """Run FTS5 MATCH query. Returns list of rows or empty list."""
    try:
        c.execute(
            """
            SELECT m.id, m.content, m.tags, m.source, m.timestamp,
                   bm25(memories_fts) AS rank
            FROM memories_fts
            JOIN memories m ON m.id = memories_fts.rowid
            WHERE memories_fts MATCH ?
            ORDER BY rank
            LIMIT ?
            """,
            (query, top),
        )
        return c.fetchall()
    except sqlite3.OperationalError:
        return []


def _prefix_query(query):
    """Convert 'auth middleware' → 'auth* middleware*' for prefix matching."""
    import re as _re
    # Don't modify queries that already use FTS5 operators
    if any(op in query for op in ['"', '*', 'OR', 'AND', 'NOT', 'NEAR']):
        return None
    words = query.strip().split()
    if not words:
        return None
    return " ".join(w + "*" for w in words if w)


def _like_fallback(c, query, top):
    """LIKE-based substring fallback when FTS5 returns nothing."""
    pattern = f"%{query}%"
    try:
        c.execute(
            """
            SELECT id, content, tags, source, timestamp, 0 AS rank
            FROM memories
            WHERE content LIKE ? OR tags LIKE ? OR source LIKE ?
            ORDER BY id DESC
            LIMIT ?
            """,
            (pattern, pattern, pattern, top),
        )
        return c.fetchall()
    except sqlite3.OperationalError:
        return []


def search(query, top=10, compact=False, context=120):
    if not os.path.exists(DB_PATH):
        print("No memory database found. Run: bash setup.sh", file=sys.stderr)
        sys.exit(1)

    if not query or not query.strip():
        print("Usage: python3 .hex/memory/search.py 'your query'", file=sys.stderr)
        sys.exit(1)

    conn = sqlite3.connect(DB_PATH)
    _log_search(conn, query)
    c = conn.cursor()

    # Strategy 1: Exact FTS5 match
    rows = _fts_query(c, query, top)

    # Strategy 2: Prefix expansion (auth → auth*)
    if not rows:
        prefix_q = _prefix_query(query)
        if prefix_q:
            rows = _fts_query(c, prefix_q, top)

    # Strategy 3: LIKE substring fallback
    if not rows:
        rows = _like_fallback(c, query, top)

    conn.close()

    if not rows:
        print(f"No memories found for: {query}")
        return

    if compact:
        for row in rows:
            _id, content, tags, source, ts, _rank = row
            snippet = content[:context].replace("\n", " ").strip()
            tag_str = f" [{tags}]" if tags else ""
            print(f"[{_id}]{tag_str} {snippet}")
        return

    for i, row in enumerate(rows):
        _id, content, tags, source, ts, _rank = row
        if i > 0:
            print("---")
        print(f"#{_id}  {ts}  {source}")
        if tags:
            print(f"tags: {tags}")
        snippet = content[:context * 3] if len(content) > context * 3 else content
        print(snippet)
    print(f"\n({len(rows)} result{'s' if len(rows) != 1 else ''})")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Search hex memory")
    parser.add_argument("query", help="Search query (FTS5 syntax supported)")
    parser.add_argument("--top", type=int, default=10, help="Max results (default: 10)")
    parser.add_argument("--compact", action="store_true", help="One-line output per result")
    parser.add_argument("--context", type=int, default=120, help="Characters of context (default: 120)")
    args = parser.parse_args()
    search(args.query, top=args.top, compact=args.compact, context=args.context)
