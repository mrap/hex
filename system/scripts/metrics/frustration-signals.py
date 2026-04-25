#!/usr/bin/env python3
"""Scan session summaries for user frustration signals."""

import json
import os
import re
import sys
import time
from pathlib import Path

FRUSTRATION_PATTERNS = [
    r"as i'?ve said",
    r"keep (doing|writing|using|fucking)",
    r"seriously\?",
    r"what happened to",
    r"i'?m not seeing",
    r"you keep",
    r"a million times",
    r"told you (not to|to stop|already)",
    r"still not (working|fixed|done)",
    r"why do we keep",
    r"how many times",
]

COMPILED = [(p, re.compile(p, re.IGNORECASE)) for p in FRUSTRATION_PATTERNS]

SUMMARIES_DIR = Path(os.environ["AGENT_DIR"]) / ".hex" / "sessions" / "summaries"
AUDIT_DIR = Path.home() / ".hex" / "audit"
OUTPUT_FILE = AUDIT_DIR / "frustration-signals.jsonl"
THRESHOLD = 4
WINDOW_SECONDS = 24 * 3600


def get_sentences(text):
    return re.split(r'(?<=[.!?])\s+', text)


def scan_file(path, now):
    mtime = path.stat().st_mtime
    if now - mtime > WINDOW_SECONDS:
        return []

    session_id = path.stem
    text = path.read_text(encoding="utf-8", errors="replace")
    sentences = get_sentences(text)

    hits = []
    for sentence in sentences:
        for pattern_str, regex in COMPILED:
            if regex.search(sentence):
                hits.append({
                    "ts": mtime,
                    "session_id": session_id,
                    "pattern": pattern_str,
                    "sentence": sentence.strip()[:300],
                })
    return hits


def main():
    if not SUMMARIES_DIR.exists():
        print(f"Frustration signals: 0 sessions in 24h (threshold: {THRESHOLD})")
        AUDIT_DIR.mkdir(parents=True, exist_ok=True)
        OUTPUT_FILE.write_text("")
        sys.exit(0)

    now = time.time()
    all_hits = []
    sessions_with_hits = set()

    for md_file in SUMMARIES_DIR.glob("*.md"):
        hits = scan_file(md_file, now)
        if hits:
            sessions_with_hits.add(md_file.stem)
            all_hits.extend(hits)

    AUDIT_DIR.mkdir(parents=True, exist_ok=True)
    tmp = OUTPUT_FILE.with_suffix(".jsonl.tmp")
    with tmp.open("w") as f:
        for hit in all_hits:
            f.write(json.dumps(hit) + "\n")
    tmp.rename(OUTPUT_FILE)

    n = len(sessions_with_hits)
    print(f"Frustration signals: {n} sessions in 24h (threshold: {THRESHOLD})")

    sys.exit(2 if n >= THRESHOLD else 0)


if __name__ == "__main__":
    main()
