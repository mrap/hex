#!/usr/bin/env python3
"""Filter noise from raw/captures/ before triage.

Auto-archives captures that match known noise patterns:
- Short test/greeting messages (under 15 chars matching known patterns)
- BOI system artifacts (next-action-q-*.md)
- Overnight monitor/summary files

Usage:
    python3 capture-filter.py [--dry-run]
"""

import argparse
import os
import re
import shutil
import sys

CAPTURES_DIR = os.path.join(os.path.dirname(__file__), "..", "..", "raw", "captures")
CAPTURES_DIR = os.path.normpath(CAPTURES_DIR)
ARCHIVE_DIR = os.path.join(CAPTURES_DIR, "archive", "noise")

# Filename patterns that are always noise
NOISE_FILENAME_PATTERNS = [
    re.compile(r"^next-action-q-.*\.md$"),
    re.compile(r"^overnight-monitor-.*\.log$"),
    re.compile(r"^overnight-summary-.*\.md$"),
]

# Content pattern: short messages matching trivial text
NOISE_CONTENT_PATTERN = re.compile(
    r"^(test|foo|hi|hello|hey|are you there|ok|yes|no|\.)\s*$",
    re.IGNORECASE,
)
NOISE_CONTENT_MAX_LENGTH = 15


def is_candidate_file(name: str) -> bool:
    """Return True if the file should be evaluated (not a directory, not TRIAGE, not in archive)."""
    if name.startswith("TRIAGE-") and name.endswith(".md"):
        return False
    if name.startswith("."):
        return False
    return True


def is_noise_by_filename(name: str) -> str | None:
    """Return a reason string if the filename matches a noise pattern, else None."""
    for pattern in NOISE_FILENAME_PATTERNS:
        if pattern.match(name):
            return f"filename matches {pattern.pattern}"
    return None


def is_noise_by_content(filepath: str) -> str | None:
    """Return a reason string if the file content is trivial noise, else None."""
    try:
        content = open(filepath, "r", errors="replace").read().strip()
    except (OSError, UnicodeDecodeError):
        return None

    if len(content) < NOISE_CONTENT_MAX_LENGTH and NOISE_CONTENT_PATTERN.match(content):
        return f"short noise content: {content!r}"
    return None


def scan_captures(dry_run: bool = False) -> int:
    """Scan captures dir and archive noise files. Returns count of filtered files."""
    if not os.path.isdir(CAPTURES_DIR):
        print(f"Captures directory not found: {CAPTURES_DIR}", file=sys.stderr)
        return 0

    if not dry_run:
        os.makedirs(ARCHIVE_DIR, exist_ok=True)

    filtered = []

    for name in sorted(os.listdir(CAPTURES_DIR)):
        filepath = os.path.join(CAPTURES_DIR, name)

        # Skip directories and non-candidate files
        if os.path.isdir(filepath):
            continue
        if not is_candidate_file(name):
            continue

        # Check noise patterns
        reason = is_noise_by_filename(name) or is_noise_by_content(filepath)
        if reason:
            filtered.append((name, reason))

    if not filtered:
        print("No noise files found.")
        return 0

    prefix = "[DRY RUN] " if dry_run else ""

    for name, reason in filtered:
        src = os.path.join(CAPTURES_DIR, name)
        dst = os.path.join(ARCHIVE_DIR, name)
        print(f"  {prefix}{name} -> archive/noise/  ({reason})")
        if not dry_run:
            shutil.move(src, dst)

    print(f"\n{prefix}Filtered {len(filtered)} noise files to archive/noise/")
    return len(filtered)


def main():
    parser = argparse.ArgumentParser(description="Filter noise from raw/captures/")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Show what would be filtered without moving files",
    )
    args = parser.parse_args()
    scan_captures(dry_run=args.dry_run)


if __name__ == "__main__":
    main()
