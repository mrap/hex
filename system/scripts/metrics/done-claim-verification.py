#!/usr/bin/env python3
"""Detect unverified UI done-claims in session summaries."""

import json
import os
import re
import sys
import time
from pathlib import Path

SUMMARIES_DIR = Path(os.environ["AGENT_DIR"]) / ".hex" / "sessions" / "summaries"
AUDIT_DIR = Path.home() / ".hex" / "audit"
OUTPUT_FILE = AUDIT_DIR / "done-claim-verification.jsonl"
WINDOW_SECONDS = 24 * 3600

DONE_CLAIM_PATTERN = re.compile(
    r"\b(ready for you|it works|done|complete|try it now|deployed|live|working)\b",
    re.IGNORECASE,
)

URL_OR_SERVICE_PATTERN = re.compile(
    r"(https?://\S+|localhost:\d+|\.vercel\.app|\.netlify\.app|mrap\.me|app\.|api\.|staging\.|prod\.|deploy)",
    re.IGNORECASE,
)

BROWSER_TOOL_PATTERN = re.compile(
    r"(playwright|browser_navigate|browser_click|browser_snapshot|browser_screenshot)",
    re.IGNORECASE,
)


def parse_summary(text):
    """Return (tasks_text, tools_used_text, notes_for_user_text)."""
    tools_used = ""
    m = re.search(r"###\s+Tools Used\s*\n(.*?)(?=\n###|\Z)", text, re.DOTALL)
    if m:
        tools_used = m.group(1).strip()

    notes = ""
    m = re.search(r"###\s+Notes for.*?\n(.*?)(?=\n###|\Z)", text, re.DOTALL)
    if m:
        notes = m.group(1).strip()

    return tools_used, notes


def scan_file(path, now):
    mtime = path.stat().st_mtime
    if now - mtime > WINDOW_SECONDS:
        return []

    session_id = path.stem
    text = path.read_text(encoding="utf-8", errors="replace")
    tools_used, notes = parse_summary(text)

    has_browser = bool(BROWSER_TOOL_PATTERN.search(tools_used))

    results = []
    for line in text.splitlines():
        if DONE_CLAIM_PATTERN.search(line) and URL_OR_SERVICE_PATTERN.search(line):
            claim_type = "verified" if has_browser else "unverified"
            results.append({
                "ts": mtime,
                "session_id": session_id,
                "claim_type": claim_type,
                "sentence": line.strip()[:300],
                "has_browser_tool": has_browser,
            })

    return results


def main():
    AUDIT_DIR.mkdir(parents=True, exist_ok=True)

    if not SUMMARIES_DIR.exists():
        print("Done-claim verification: 0 claims found in 24h")
        OUTPUT_FILE.write_text("")
        sys.exit(0)

    now = time.time()
    all_results = []
    unverified_count = 0

    for md_file in sorted(SUMMARIES_DIR.glob("*.md")):
        hits = scan_file(md_file, now)
        all_results.extend(hits)
        unverified_count += sum(1 for h in hits if h["claim_type"] == "unverified")

    tmp = OUTPUT_FILE.with_suffix(".jsonl.tmp")
    with tmp.open("w") as f:
        for item in all_results:
            f.write(json.dumps(item) + "\n")
    tmp.rename(OUTPUT_FILE)

    total = len(all_results)
    print(
        f"Done-claim verification: {total} claims in 24h, "
        f"{unverified_count} unverified (threshold: 1)"
    )

    sys.exit(2 if unverified_count > 0 else 0)


if __name__ == "__main__":
    main()
