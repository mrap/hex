#!/usr/bin/env python3
"""Scan todo.md and landings for stalled dependencies.

Looks for items containing dependency markers ("waiting on", "blocked by",
"pending response", "need response", "awaiting") and tracks how long each
has been present. Alerts on items older than a configurable threshold.

Usage:
    python3 stale_deps.py [--threshold DAYS] [--json]

Reads from:
    $AGENT_DIR/todo.md
    $AGENT_DIR/landings/  (most recent .md file)
    $AGENT_DIR/.hex/dependency-tracker.json  (state file)

Writes to:
    $AGENT_DIR/.hex/dependency-tracker.json  (updated state)
    stdout (human-readable or JSON report)
"""

import json
import os
import re
import sys
from datetime import datetime, timedelta
from pathlib import Path


def find_agent_dir():
    """Walk up from script location to find CLAUDE.md."""
    script_dir = Path(__file__).resolve().parent
    check = script_dir
    for _ in range(5):
        check = check.parent
        if (check / "CLAUDE.md").exists():
            return check
    return Path(os.environ.get("AGENT_DIR", "."))


def extract_dependency_items(text, source_file):
    """Find lines that look like dependency-blocked items."""
    markers = [
        r"waiting on",
        r"blocked by",
        r"pending response",
        r"need(?:s|ing)? response",
        r"awaiting",
        r"waiting for",
        r"depends on",
        r"need(?:s)? from",
    ]
    pattern = re.compile("|".join(markers), re.IGNORECASE)
    items = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if pattern.search(stripped):
            # Clean up markdown formatting
            clean = re.sub(r"^[-*\[\]x ]+", "", stripped).strip()
            if len(clean) > 10:  # Skip very short matches
                items.append({"text": clean, "source": source_file})
    return items


def load_tracker(tracker_path):
    """Load existing dependency tracker state."""
    if tracker_path.exists():
        try:
            with open(tracker_path) as f:
                return json.load(f)
        except (json.JSONDecodeError, OSError):
            pass
    return {"items": {}, "last_scan": None}


def save_tracker(tracker_path, state):
    """Save tracker state atomically."""
    tmp = str(tracker_path) + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f, indent=2)
    os.rename(tmp, str(tracker_path))


def item_key(item):
    """Generate a stable key for deduplication. Uses first 80 chars."""
    text = item["text"][:80].lower().strip()
    # Normalize whitespace
    text = re.sub(r"\s+", " ", text)
    return text


def main():
    threshold_days = 2
    output_json = False

    args = sys.argv[1:]
    i = 0
    while i < len(args):
        if args[i] == "--threshold" and i + 1 < len(args):
            threshold_days = int(args[i + 1])
            i += 2
        elif args[i] == "--json":
            output_json = True
            i += 1
        else:
            i += 1

    agent_dir = find_agent_dir()
    tracker_path = agent_dir / ".claude" / "dependency-tracker.json"
    today = datetime.now().strftime("%Y-%m-%d")

    # Collect dependency items from todo.md and latest landings
    all_items = []

    todo_path = agent_dir / "todo.md"
    if todo_path.exists():
        all_items.extend(extract_dependency_items(todo_path.read_text(), "todo.md"))

    landings_dir = agent_dir / "landings"
    if landings_dir.exists():
        landing_files = sorted(landings_dir.glob("????-??-??.md"), reverse=True)
        for lf in landing_files[:3]:  # Check last 3 days
            all_items.extend(
                extract_dependency_items(lf.read_text(), f"landings/{lf.name}")
            )

    # Load existing tracker
    state = load_tracker(tracker_path)

    # Update tracker with current items
    current_keys = set()
    for item in all_items:
        key = item_key(item)
        current_keys.add(key)
        if key not in state["items"]:
            state["items"][key] = {
                "text": item["text"],
                "source": item["source"],
                "first_seen": today,
                "last_seen": today,
            }
        else:
            state["items"][key]["last_seen"] = today
            # Update source if it moved
            state["items"][key]["source"] = item["source"]

    # Remove items no longer present (resolved)
    resolved = [k for k in state["items"] if k not in current_keys]
    for k in resolved:
        del state["items"][k]

    state["last_scan"] = today

    # Find stale items
    stale = []
    threshold = timedelta(days=threshold_days)
    for key, info in state["items"].items():
        first_seen = datetime.strptime(info["first_seen"], "%Y-%m-%d")
        age = datetime.now() - first_seen
        if age >= threshold:
            stale.append(
                {
                    "text": info["text"],
                    "source": info["source"],
                    "first_seen": info["first_seen"],
                    "days_stale": age.days,
                }
            )

    # Sort by staleness
    stale.sort(key=lambda x: x["days_stale"], reverse=True)

    # Save updated tracker
    tracker_path.parent.mkdir(parents=True, exist_ok=True)
    save_tracker(tracker_path, state)

    # Output
    if output_json:
        print(json.dumps({"stale": stale, "total_tracked": len(state["items"])}, indent=2))
    else:
        if stale:
            print(f"STALE DEPENDENCIES ({len(stale)} items past {threshold_days}-day threshold):")
            print()
            for item in stale:
                print(f"  [{item['days_stale']}d] {item['text']}")
                print(f"       Source: {item['source']} | First seen: {item['first_seen']}")
                print()
        else:
            print(f"No stale dependencies (threshold: {threshold_days} days, tracking {len(state['items'])} items).")


if __name__ == "__main__":
    main()
