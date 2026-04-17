#!/usr/bin/env python3
"""session-delta — persist session eval records to memory.db

Called by session-reflect.sh after a hex session ends.
Reads the latest reflection JSON (if any) and appends eval_records
to the memory SQLite database.

Usage:
    python3 session-delta.py [--session-id ID] [--db PATH] [--dry-run]
"""
import argparse
import json
import os
import sqlite3
import sys
from datetime import datetime

HEX_DIR = os.environ.get("HEX_DIR", os.path.expanduser("~/hex"))
DEFAULT_DB = os.path.join(HEX_DIR, ".hex", "memory.db")


def ensure_table(conn: sqlite3.Connection) -> None:
    conn.execute("""
        CREATE TABLE IF NOT EXISTS eval_records (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id  TEXT,
            recorded_at TEXT NOT NULL,
            payload     TEXT
        )
    """)
    conn.commit()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--session-id", default="")
    parser.add_argument("--db", default=DEFAULT_DB)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    if not os.path.exists(args.db):
        print(f"session-delta: memory.db not found at {args.db}, skipping", file=sys.stderr)
        return 0

    payload = json.dumps({"session_id": args.session_id, "source": "session-delta"})

    if args.dry_run:
        print(f"session-delta: dry-run — would insert eval_record for session {args.session_id!r}")
        return 0

    try:
        conn = sqlite3.connect(args.db)
        ensure_table(conn)
        conn.execute(
            "INSERT INTO eval_records (session_id, recorded_at, payload) VALUES (?, ?, ?)",
            (args.session_id, datetime.utcnow().isoformat(), payload),
        )
        conn.commit()
        conn.close()
        print(f"session-delta: eval_record persisted for session {args.session_id!r}")
    except Exception as exc:  # pylint: disable=broad-except
        print(f"session-delta: warning — could not write to db: {exc}", file=sys.stderr)
        return 0  # non-fatal

    return 0


if __name__ == "__main__":
    sys.exit(main())
