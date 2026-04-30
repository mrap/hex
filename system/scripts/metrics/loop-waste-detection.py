#!/usr/bin/env python3
"""Detect loop waste: same work repeated across sessions within 48h."""

import json
import os
import re
import sys
import time
from collections import defaultdict
from pathlib import Path

_HEX_ROOT = Path(os.environ.get("HEX_DIR", "").strip() or (Path.home() / "hex"))
SUMMARIES_DIR = _HEX_ROOT / ".hex" / "sessions" / "summaries"
AUDIT_DIR = Path.home() / ".hex" / "audit"
OUTPUT_FILE = AUDIT_DIR / "loop-detections.jsonl"
WINDOW_SECONDS = 48 * 3600
SIMILARITY_THRESHOLD = 0.80
MIN_COMBINED_TASKS = 3

URL_RE = re.compile(r"https?://\S+|www\.\S+")
TIMESTAMP_RE = re.compile(r"\b\d{4}-\d{2}-\d{2}(T\d{2}:\d{2}(:\d{2})?)?\b|\b\d{1,2}:\d{2}(:\d{2})?\b")

# BOI worker sessions share a long common preamble as their first task entry;
# they are distinct workers on distinct specs and must not be treated as duplicates.
BOI_WORKER_RE = re.compile(r"#\s*BOI\s+Worker", re.IGNORECASE)


def is_boi_worker_session(tasks):
    return bool(tasks) and bool(BOI_WORKER_RE.match(tasks[0]))


def extract_tasks_section(text):
    """Return list of task strings from the Tasks section."""
    tasks = []
    in_tasks = False
    for line in text.splitlines():
        if re.match(r"#+\s+Tasks\s*$", line.strip()):
            in_tasks = True
            continue
        if in_tasks:
            if re.match(r"#+\s+", line.strip()):
                break
            m = re.match(r"[-*]\s+(.+)", line.strip())
            if m:
                tasks.append(m.group(1).strip())
    return tasks


def normalize(text):
    """Lowercase, strip URLs and timestamps."""
    text = text.lower()
    text = URL_RE.sub("", text)
    text = TIMESTAMP_RE.sub("", text)
    text = re.sub(r"[^\w\s]", " ", text)
    return text


def task_list_trigrams(tasks):
    """Combine all task text and return trigram set."""
    combined = " ".join(normalize(t) for t in tasks)
    tokens = combined.split()
    if len(tokens) < 3:
        return set(tuple(tokens))
    return {(tokens[i], tokens[i+1], tokens[i+2]) for i in range(len(tokens) - 2)}


def jaccard(set_a, set_b):
    if not set_a and not set_b:
        return 0.0
    union = set_a | set_b
    if not union:
        return 0.0
    return len(set_a & set_b) / len(union)


def load_summaries():
    """Return list of (session_id, mtime, tasks) for summaries within 48h window."""
    now = time.time()
    cutoff = now - WINDOW_SECONDS
    results = []
    if not SUMMARIES_DIR.exists():
        return results
    for path in SUMMARIES_DIR.glob("*.md"):
        mtime = path.stat().st_mtime
        if mtime < cutoff:
            continue
        text = path.read_text(errors="replace")
        tasks = extract_tasks_section(text)
        if not tasks:
            continue
        if is_boi_worker_session(tasks):
            continue
        results.append({
            "session_id": path.stem,
            "mtime": mtime,
            "tasks": tasks,
            "path": path,
        })
    return results


def detect_loops(summaries):
    """Return one record per cluster of similar sessions (not one per pair).

    Groups similar sessions into connected components via union-find, then
    reports each cluster as a single loop waste event.  This prevents a
    group of N identical sessions from inflating the count by C(N,2).
    """
    n = len(summaries)
    parent = list(range(n))

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(x, y):
        rx, ry = find(x), find(y)
        if rx != ry:
            parent[rx] = ry

    # Pass 1: collect all similar pairs and union them
    similar_pairs = []
    for i in range(n):
        for j in range(i + 1, n):
            a = summaries[i]
            b = summaries[j]
            combined_count = len(a["tasks"]) + len(b["tasks"])
            if combined_count <= MIN_COMBINED_TASKS:
                continue
            tg_a = task_list_trigrams(a["tasks"])
            tg_b = task_list_trigrams(b["tasks"])
            sim = jaccard(tg_a, tg_b)
            if sim > SIMILARITY_THRESHOLD:
                similar_pairs.append((i, j, sim))
                union(i, j)

    if not similar_pairs:
        return []

    # Pass 2: find representative (highest-sim) pair per cluster using final roots
    best_pair: dict[int, tuple] = {}
    for i, j, sim in similar_pairs:
        root = find(i)
        prev = best_pair.get(root)
        if prev is None or sim > prev[2]:
            best_pair[root] = (i, j, sim)

    loops = []
    for root, (i, j, sim) in best_pair.items():
        cluster_size = sum(1 for k in range(n) if find(k) == root)
        a = summaries[i]
        b = summaries[j]
        preview_a = a["tasks"][0][:80] if a["tasks"] else ""
        preview_b = b["tasks"][0][:80] if b["tasks"] else ""
        loops.append({
            "ts": max(a["mtime"], b["mtime"]),
            "session_a": a["session_id"],
            "session_b": b["session_id"],
            "similarity": round(sim, 4),
            "task_preview": f"{preview_a} | {preview_b}",
            "cluster_size": cluster_size,
        })

    return loops


def main():
    AUDIT_DIR.mkdir(parents=True, exist_ok=True)

    summaries = load_summaries()
    loops = detect_loops(summaries)

    tmp = str(OUTPUT_FILE) + ".tmp"
    with open(tmp, "w") as f:
        for record in loops:
            f.write(json.dumps(record) + "\n")
    os.replace(tmp, OUTPUT_FILE)

    n = len(loops)
    print(f"Loop detection: {n} loops found in 48h")

    if n > 0:
        sys.exit(2)
    sys.exit(0)


if __name__ == "__main__":
    main()
