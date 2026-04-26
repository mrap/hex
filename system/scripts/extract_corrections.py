#!/usr/bin/env python3
"""Extract correction and preference messages from a Claude Code session JSONL.

Usage: extract_corrections.py <jsonl_path>

Outputs matched messages to stdout (one block per correction).
Exits with code 0 whether or not corrections are found — empty stdout means none.
"""

import json
import re
import sys

CORRECTION_PATTERNS = [
    r'^(no|nope|not|wrong|incorrect|that.s not)',
    r'^(don.t|stop|avoid|never|instead)',
    r'(actually,|wait,|hold on)',
    r'(should be|should have|should not)',
    r'(always|never|from now on|prefer|instead of)',
    r'(again\?|still|why did|what happened)',
]


def extract_corrections(jsonl_path):
    messages = []
    with open(jsonl_path) as f:
        for line in f:
            try:
                obj = json.loads(line)
                if obj.get("type") == "user":
                    messages.append(obj)
            except json.JSONDecodeError:
                continue

    corrections = []
    for msg in messages:
        text = ""
        content = msg.get("message", {}).get("content", "")
        if isinstance(content, str):
            text = content
        elif isinstance(content, list):
            text = " ".join(b.get("text", "") for b in content if b.get("type") == "text")

        # Skip system-injected content: skill expansions, hook outputs, long prompts
        # Real user corrections are short (< 500 chars) and don't contain markdown headers
        if len(text) > 500 or text.startswith("#") or text.startswith("Base directory"):
            continue

        if any(re.search(p, text.lower()) for p in CORRECTION_PATTERNS):
            corrections.append(text[:500])

    return corrections


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: extract_corrections.py <jsonl_path>", file=sys.stderr)
        sys.exit(1)

    corrections = extract_corrections(sys.argv[1])
    for i, c in enumerate(corrections, 1):
        print(f"--- Correction {i} ---")
        print(c)
        print()
