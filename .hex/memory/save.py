#!/usr/bin/env python3
"""Save a memory to the hex memory database.

Usage:
    python3 .hex/memory/save.py 'JWT tokens use httpOnly cookies'
    python3 .hex/memory/save.py 'content here' --tags 'auth,security' --source 'middleware.ts'
"""

import argparse
import os
import sqlite3
import sys
from datetime import datetime, timezone

DB_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "memory.db")


def save(content, tags="", source=""):
    if not os.path.exists(DB_PATH):
        print("No memory database found. Run: bash setup.sh", file=sys.stderr)
        sys.exit(1)

    conn = sqlite3.connect(DB_PATH)
    c = conn.cursor()
    ts = datetime.now(timezone.utc).isoformat(timespec="seconds")

    c.execute(
        "INSERT INTO memories (content, tags, source, timestamp) VALUES (?, ?, ?, ?)",
        (content, tags, source, ts),
    )
    mem_id = c.lastrowid
    conn.commit()
    conn.close()

    print(f"Saved memory #{mem_id}: {content[:80]}{'...' if len(content) > 80 else ''}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Save a memory to hex")
    parser.add_argument("content", help="Memory content to save")
    parser.add_argument("--tags", default="", help="Comma-separated tags")
    parser.add_argument("--source", default="", help="Source file or context")
    args = parser.parse_args()
    save(args.content, tags=args.tags, source=args.source)
