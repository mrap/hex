#!/usr/bin/env python3
"""Detect sessions with suspiciously overlapping file modification sets."""

import json
import os
import re
import sys
import time
from pathlib import Path

SUMMARIES_DIR = Path(os.environ["AGENT_DIR"]) / ".hex" / "sessions" / "summaries"
AUDIT_DIR = Path.home() / ".hex" / "audit"
OUTPUT_FILE = AUDIT_DIR / "session-anomalies.jsonl"
WINDOW_SECONDS = 24 * 3600
JACCARD_THRESHOLD = 0.60
MTIME_GAP_SECONDS = 4 * 3600


def parse_files_modified(text):
    """Extract file paths from the 'Files Modified' section."""
    m = re.search(r"###\s+Files Modified\s*\n(.*?)(?=\n###|\Z)", text, re.DOTALL)
    if not m:
        return []
    paths = []
    for line in m.group(1).splitlines():
        line = line.strip().lstrip("- ").strip()
        if line:
            paths.append(line)
    return paths


def jaccard(set_a, set_b):
    if not set_a and not set_b:
        return 0.0
    intersection = len(set_a & set_b)
    union = len(set_a | set_b)
    return intersection / union if union > 0 else 0.0


def load_sessions(now):
    sessions = []
    if not SUMMARIES_DIR.exists():
        return sessions
    for md_file in sorted(SUMMARIES_DIR.glob("*.md")):
        mtime = md_file.stat().st_mtime
        if now - mtime > WINDOW_SECONDS:
            continue
        text = md_file.read_text(encoding="utf-8", errors="replace")
        files = parse_files_modified(text)
        if files:
            sessions.append({
                "session_id": md_file.stem,
                "mtime": mtime,
                "files": set(files),
            })
    return sessions


def main():
    AUDIT_DIR.mkdir(parents=True, exist_ok=True)

    now = time.time()
    sessions = load_sessions(now)

    anomalies = []
    n = len(sessions)
    for i in range(n):
        for j in range(i + 1, n):
            a, b = sessions[i], sessions[j]
            mtime_gap = abs(a["mtime"] - b["mtime"])
            if mtime_gap > MTIME_GAP_SECONDS:
                continue
            score = jaccard(a["files"], b["files"])
            if score >= JACCARD_THRESHOLD:
                preview = sorted(a["files"] & b["files"])[:3]
                anomalies.append({
                    "ts": now,
                    "session_a": a["session_id"],
                    "session_b": b["session_id"],
                    "jaccard": round(score, 3),
                    "mtime_gap_seconds": round(mtime_gap),
                    "shared_files_preview": preview,
                })

    tmp = OUTPUT_FILE.with_suffix(".jsonl.tmp")
    with tmp.open("w") as f:
        for item in anomalies:
            f.write(json.dumps(item) + "\n")
    tmp.rename(OUTPUT_FILE)

    print(
        f"Context continuity: {len(anomalies)} duplicate session pairs in 24h (threshold: 1)"
    )

    sys.exit(2 if anomalies else 0)


if __name__ == "__main__":
    main()
