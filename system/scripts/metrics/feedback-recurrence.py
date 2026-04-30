#!/usr/bin/env python3
"""
feedback-recurrence.py — measure whether feedback corrections recur in later sessions.

Reads feedback memory files from the Claude project memory directory (derived from HEX_DIR)
Scans session summaries written AFTER each feedback date.
Writes ~/.hex/audit/memory-effectiveness.jsonl
Exit 2 if any memory has recurrence_rate >= 0.20.
"""

import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

_HEX_DIR = os.environ.get("HEX_DIR", os.environ.get("HEX_DIR", "."))
MEMORY_DIR = Path(os.environ.get("CLAUDE_PROJECT_MEMORY", str(Path.home() / ".claude/projects" / ("-" + _HEX_DIR.replace("/", "-").lstrip("-")) / "memory")))
SUMMARIES_DIR = Path(_HEX_DIR) / ".hex" / "sessions" / "summaries"
AUDIT_DIR = Path.home() / ".hex/audit"
OUTPUT_FILE = AUDIT_DIR / "memory-effectiveness.jsonl"

RECURRENCE_RATE_THRESHOLD = 0.20


def parse_date_from_content(text: str) -> datetime | None:
    """Extract the earliest date mentioned in body text (YYYY-MM-DD format)."""
    dates = re.findall(r"(\d{4}-\d{2}-\d{2})", text)
    parsed = []
    for d in dates:
        try:
            parsed.append(datetime.strptime(d, "%Y-%m-%d").replace(tzinfo=timezone.utc))
        except ValueError:
            pass
    return min(parsed) if parsed else None


def parse_feedback_file(path: Path) -> dict | None:
    """Parse a feedback memory file into a structured record."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return None

    # Extract description from frontmatter
    description = ""
    desc_match = re.search(r"^description:\s*(.+)$", text, re.MULTILINE)
    if desc_match:
        description = desc_match.group(1).strip()

    # Extract the Rule line as correction type
    rule_match = re.search(r"\*\*Rule:\*\*\s*(.+)", text)
    correction_type = rule_match.group(1).strip() if rule_match else description[:120]

    # Date: try content first, then file mtime
    date_written = parse_date_from_content(text)
    if date_written is None:
        mtime = path.stat().st_mtime
        date_written = datetime.fromtimestamp(mtime, tz=timezone.utc)

    # Build keyword set from description + correction_type
    combined = f"{description} {correction_type}".lower()
    # Extract meaningful words (4+ chars, not stop words)
    stop = {
        "that", "this", "with", "from", "have", "when", "will", "been",
        "they", "them", "their", "what", "also", "which", "there", "were",
        "into", "more", "than", "then", "only", "should", "would", "could",
        "does", "make", "some", "after", "before", "about", "always",
    }
    keywords = {
        w for w in re.findall(r"[a-z]{4,}", combined)
        if w not in stop
    }

    return {
        "memory_file": path.name,
        "correction_type": correction_type[:120],
        "date_written": date_written.isoformat(),
        "keywords": keywords,
        "_date_obj": date_written,
    }


def load_summaries() -> list[dict]:
    """Load all session summaries with their text and mtime."""
    if not SUMMARIES_DIR.exists():
        return []
    summaries = []
    for p in SUMMARIES_DIR.glob("*.md"):
        try:
            mtime = datetime.fromtimestamp(p.stat().st_mtime, tz=timezone.utc)
            text = p.read_text(encoding="utf-8").lower()
            summaries.append({"path": p, "mtime": mtime, "text": text})
        except OSError:
            pass
    return summaries


def count_keyword_hits(keywords: set[str], text: str) -> int:
    """Count how many distinct keywords appear in the text."""
    return sum(1 for kw in keywords if kw in text)


def check_recurrence(feedback: dict, summaries: list[dict]) -> tuple[int, int, float]:
    """Return (sessions_since, recurrence_count, recurrence_rate)."""
    feedback_date = feedback["_date_obj"]
    keywords = feedback["keywords"]
    if not keywords:
        return 0, 0, 0.0

    # Sessions written strictly after feedback was created
    later = [s for s in summaries if s["mtime"] > feedback_date]
    sessions_since = len(later)
    if sessions_since == 0:
        return 0, 0, 0.0

    # A session "recurs" the feedback pattern if >= 2 keywords hit
    recurrent = [s for s in later if count_keyword_hits(keywords, s["text"]) >= 3]
    recurrence_count = len(recurrent)
    recurrence_rate = round(recurrence_count / sessions_since, 3)
    return sessions_since, recurrence_count, recurrence_rate


def main() -> int:
    AUDIT_DIR.mkdir(parents=True, exist_ok=True)

    # Collect feedback files
    if not MEMORY_DIR.exists():
        print(f"Memory directory not found: {MEMORY_DIR}", file=sys.stderr)
        OUTPUT_FILE.write_text("")
        print("Feedback effectiveness: 0/0 memories with recurrence_rate >= 20%")
        return 0

    feedback_files = sorted(MEMORY_DIR.glob("feedback_*.md"))
    if not feedback_files:
        print("Feedback effectiveness: 0/0 memories with recurrence_rate >= 20%")
        OUTPUT_FILE.write_text("")
        return 0

    summaries = load_summaries()

    records = []
    critical_count = 0
    total = 0

    for path in feedback_files:
        fb = parse_feedback_file(path)
        if fb is None:
            continue
        total += 1
        sessions_since, recurrence_count, recurrence_rate = check_recurrence(fb, summaries)

        record = {
            "memory_file": fb["memory_file"],
            "correction_type": fb["correction_type"],
            "date_written": fb["date_written"],
            "sessions_since": sessions_since,
            "recurrence_count": recurrence_count,
            "recurrence_rate": recurrence_rate,
        }
        records.append(record)
        if recurrence_rate >= RECURRENCE_RATE_THRESHOLD:
            critical_count += 1

    # Atomic write
    tmp = str(OUTPUT_FILE) + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        for r in records:
            f.write(json.dumps(r) + "\n")
    os.replace(tmp, OUTPUT_FILE)

    print(f"Feedback effectiveness: {critical_count}/{total} memories with recurrence_rate >= {RECURRENCE_RATE_THRESHOLD:.0%}")

    return 2 if critical_count > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
