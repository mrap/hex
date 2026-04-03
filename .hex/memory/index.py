#!/usr/bin/env python3
"""Index workspace markdown files into hex memory.

Chunks files by heading. Incremental by default (skips unchanged content).

Usage:
    python3 .hex/memory/index.py              # Incremental index
    python3 .hex/memory/index.py --full       # Full rebuild
    python3 .hex/memory/index.py --dir docs/  # Index specific directory
"""

import argparse
import hashlib
import os
import re
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path

DB_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "memory.db")


def chunk_markdown(text, filepath):
    """Split markdown into chunks by heading."""
    chunks = []
    current_heading = os.path.basename(filepath)
    current_lines = []

    for line in text.split("\n"):
        if re.match(r"^#{1,4}\s+", line):
            if current_lines:
                body = "\n".join(current_lines).strip()
                if body:
                    chunks.append((current_heading, body))
            current_heading = line.lstrip("#").strip()
            current_lines = []
        else:
            current_lines.append(line)

    if current_lines:
        body = "\n".join(current_lines).strip()
        if body:
            chunks.append((current_heading, body))

    return chunks


def content_hash(text):
    return hashlib.sha256(text.encode()).hexdigest()[:16]


def index(directory=".", full=False):
    if not os.path.exists(DB_PATH):
        print("No memory database found. Run: bash setup.sh", file=sys.stderr)
        sys.exit(1)

    conn = sqlite3.connect(DB_PATH)
    c = conn.cursor()

    # Track indexed hashes to avoid duplicates
    c.execute("SELECT content, source FROM memories WHERE tags LIKE '%indexed%'")
    existing = {(row[0][:100], row[1]) for row in c.fetchall()}

    if full:
        c.execute("DELETE FROM memories WHERE tags LIKE '%indexed%'")
        existing = set()
        print("Full rebuild: cleared indexed memories")

    # Find markdown files
    workspace = Path(directory).resolve()
    md_files = sorted(workspace.rglob("*.md"))

    # Skip .hex/memory and common non-content dirs
    skip_dirs = {".git", "node_modules", ".venv", "__pycache__", ".hex/memory"}
    md_files = [
        f for f in md_files
        if not any(skip in str(f) for skip in skip_dirs)
    ]

    indexed = 0
    skipped = 0
    ts = datetime.now(timezone.utc).isoformat(timespec="seconds")

    for filepath in md_files:
        try:
            text = filepath.read_text(encoding="utf-8", errors="ignore")
        except (OSError, PermissionError):
            continue

        rel_path = str(filepath.relative_to(workspace))
        chunks = chunk_markdown(text, rel_path)

        for heading, body in chunks:
            content = f"[{heading}] {body}"
            key = (content[:100], rel_path)

            if key in existing:
                skipped += 1
                continue

            c.execute(
                "INSERT INTO memories (content, tags, source, timestamp) VALUES (?, ?, ?, ?)",
                (content, f"indexed,{heading}", rel_path, ts),
            )
            existing.add(key)
            indexed += 1

    conn.commit()
    conn.close()

    print(f"Indexed {indexed} chunks from {len(md_files)} files ({skipped} unchanged)")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Index markdown files into hex memory")
    parser.add_argument("--full", action="store_true", help="Full rebuild (clear indexed memories first)")
    parser.add_argument("--dir", default=".", help="Directory to index (default: current directory)")
    args = parser.parse_args()
    index(directory=args.dir, full=args.full)
