#!/usr/bin/env python3
"""generative-loop — Daily signal MVP for the hex generative layer.

Implements three daily output types:
  1. Cross-Source Pattern Alert  — fires when same topic appears in 2+ sources within 72h
  2. Project Momentum Pulse      — flags high-priority projects stalled 7+ days
  3. Research Feed Health Check  — monitors whether feeds are generating output

Usage:
    python3 generative-loop.py --cycle-type daily [--dry-run] [--verbose]
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Optional

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

HEX_ROOT = Path(os.environ.get("HEX_DIR", str(Path.home() / "hex")))
OUTPUTS_DIR = HEX_ROOT / "raw" / "research" / "generative-layer" / "outputs"
STATE_DIR = HEX_ROOT / "raw" / "research" / "generative-layer" / "state"
BOOKMARKS_DIR = HEX_ROOT / "raw" / "research" / "bookmarks"
SCOUT_DIR = HEX_ROOT / "raw" / "research" / "scout"
ME_FILE = HEX_ROOT / "me" / "me.md"
TODO_FILE = HEX_ROOT / "todo.md"
NORTH_STAR_FILE = HEX_ROOT / "projects" / "system-improvement" / "north-star.md"

DAILY_OUTPUT_CAP = 2
CLAUDE_MODEL = "claude-haiku-4-5-20251001"

STOPWORDS = {
    "the", "and", "for", "that", "with", "this", "from", "have", "will",
    "been", "they", "when", "your", "what", "which", "their", "there",
    "some", "more", "also", "into", "than", "then", "these", "about",
    "just", "time", "very", "only", "can", "you", "not", "but", "are",
    "was", "has", "had", "its", "all", "any", "each", "how", "our",
    "use", "used", "using", "may", "new", "one", "two", "per", "get",
    "like", "well", "work", "make", "does", "way", "set", "now", "run",
    "output", "input", "file", "files", "path", "note", "add", "via",
    "status", "type", "check", "source", "sources", "section", "line",
}

NOW = datetime.now(timezone.utc)


def log(msg):
    print(f"[generative-loop] {msg}", file=sys.stderr)


# ---------------------------------------------------------------------------
# Context loading helpers
# ---------------------------------------------------------------------------

def read_file_safe(path):
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except Exception:
        return ""


def get_recent_files(directory, max_age_hours):
    if not directory.exists():
        return []
    cutoff_ts = (NOW - timedelta(hours=max_age_hours)).timestamp()
    results = []
    try:
        for f in directory.iterdir():
            if not f.is_file():
                continue
            if f.name.startswith("_"):
                continue
            try:
                if f.stat().st_mtime >= cutoff_ts:
                    results.append(f)
            except OSError:
                continue
    except OSError:
        return []
    return sorted(results, key=lambda p: p.stat().st_mtime, reverse=True)


def all_non_index_files(directory):
    if not directory.exists():
        return []
    try:
        return [f for f in directory.iterdir()
                if f.is_file() and not f.name.startswith("_")]
    except OSError:
        return []


def extract_keywords(text):
    words = re.findall(r'[a-z][a-z0-9\-]{3,}', text.lower())
    return {w for w in words if w not in STOPWORDS and len(w) >= 4}


def parse_now_section(todo_text):
    projects = []
    in_now = False
    for line in todo_text.splitlines():
        if line.strip() == "## Now":
            in_now = True
            continue
        if in_now and re.match(r'^## ', line):
            break
        if in_now and re.match(r'\s*- \[ \]', line):
            m = re.search(r'\*\*(.+?)\*\*', line)
            if m:
                name = m.group(1)
                if " \u2014 " in name:
                    name = name.split(" \u2014 ")[0].strip()
                projects.append({"name": name, "line": line.strip()})
    return projects


def load_context():
    todo_text = read_file_safe(TODO_FILE)
    now_lines = []
    in_now = False
    for line in todo_text.splitlines():
        if line.strip() == "## Now":
            in_now = True
        elif in_now and re.match(r'^## ', line):
            break
        elif in_now:
            now_lines.append(line)
    return {
        "me": read_file_safe(ME_FILE),
        "north_star": read_file_safe(NORTH_STAR_FILE),
        "todo_now": "\n".join(now_lines[:40]),
        "todo_full": todo_text,
    }


# ---------------------------------------------------------------------------
# Claude synthesis
# ---------------------------------------------------------------------------

def run_claude(prompt, verbose):
    try:
        result = subprocess.run(
            [os.path.expanduser("~/.local/bin/claude"), "--dangerously-skip-permissions", "-p", prompt, "--model", CLAUDE_MODEL, "--max-turns", "3"],
            capture_output=True,
            text=True,
            timeout=120,
        )
        if result.returncode != 0:
            if verbose:
                log(f"claude -p failed (exit {result.returncode}): {result.stderr[:200]}")
            return f"[synthesis failed: exit {result.returncode}]"
        output = result.stdout.strip()
        if len(output) < 50:
            return f"[synthesis produced insufficient output: {output!r}]"
        return output
    except FileNotFoundError:
        return "[claude not found -- synthesis skipped]"
    except subprocess.TimeoutExpired:
        return "[synthesis timed out after 120s]"
    except Exception as e:
        return f"[synthesis error: {e}]"


# ---------------------------------------------------------------------------
# Output writing
# ---------------------------------------------------------------------------

def write_output(filename, content, today_dir, dry_run):
    output_path = today_dir / filename
    if dry_run:
        log(f"[DRY RUN] Would write {len(content)} chars to {output_path}")
        return output_path
    today_dir.mkdir(parents=True, exist_ok=True)
    tmp_path = output_path.with_suffix(".tmp")
    tmp_path.write_text(content, encoding="utf-8")
    tmp_path.rename(output_path)
    log(f"Wrote: {output_path}")
    return output_path


def update_cycle_log(written, candidates, duration_ms, dry_run):
    if dry_run:
        log(f"[DRY RUN] Would append cycle log: {len(written)} outputs, {candidates} candidates, {duration_ms}ms")
        return
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    log_path = STATE_DIR / "cycle-log.jsonl"
    entry = {
        "timestamp": NOW.isoformat(),
        "cycle_type": "daily",
        "outputs_generated": len(written),
        "candidates_scored": candidates,
        "duration_ms": duration_ms,
        "outputs": written,
    }
    with open(log_path, "a", encoding="utf-8") as f:
        f.write(json.dumps(entry) + "\n")
    log(f"Updated cycle log: {log_path}")


# ---------------------------------------------------------------------------
# Type 1: Cross-Source Pattern Alert
# ---------------------------------------------------------------------------

def detect_cross_source_patterns(verbose):
    bookmark_files = get_recent_files(BOOKMARKS_DIR, 72)
    scout_files = get_recent_files(SCOUT_DIR, 72)

    active_sources = {}
    if bookmark_files:
        active_sources["bookmarks"] = bookmark_files
    if scout_files:
        active_sources["scout"] = scout_files

    if len(active_sources) < 2:
        if verbose:
            log(f"Type 1: only {len(active_sources)} active source(s) -- need 2+, skipping")
        return None

    source_keywords = {}
    source_samples = {}
    for src, files in active_sources.items():
        combined = "\n".join(read_file_safe(f) for f in files[:5])
        source_keywords[src] = extract_keywords(combined)
        source_samples[src] = {
            "files": [f.name for f in files[:3]],
            "sample": combined[:600],
            "count": len(files),
        }

    kw_sets = list(source_keywords.values())
    common = kw_sets[0].copy()
    for s in kw_sets[1:]:
        common &= s

    generic = {
        "agent", "model", "data", "code", "system", "build", "test", "task",
        "tool", "user", "error", "https", "http", "null", "true", "false",
        "list", "dict", "json", "text", "call", "func", "class", "async",
    }
    meaningful = common - generic

    if not meaningful:
        if verbose:
            log("Type 1: no meaningful cross-source keywords -- skipping")
        return None

    if verbose:
        log(f"Type 1: cross-source patterns: {sorted(meaningful)[:5]}")

    return {
        "patterns": sorted(meaningful)[:12],
        "sources": source_samples,
        "source_names": list(active_sources.keys()),
        "file_counts": {k: len(v) for k, v in active_sources.items()},
    }


def generate_pattern_alert(detection, context, dry_run, verbose):
    today = NOW.strftime("%Y-%m-%d")
    patterns_str = ", ".join(detection["patterns"])
    sources_str = " and ".join(detection["source_names"])

    if dry_run:
        return (
            f"# Cross-Source Pattern Alert\n"
            f"_Cycle type: daily | Generated: {today} | Output type: Cross-Source Pattern Alert_\n\n"
            f"**TL;DR:** [DRY RUN -- would synthesize patterns: {patterns_str}]\n\n"
            f"**Source:** {sources_str} (last 72h)\n"
        )

    prompt = (
        f"You are generating a Cross-Source Pattern Alert for a personal AI assistant system (hex).\n\n"
        f"Personal context:\n{context['me'][:400]}\n\n"
        f"Active projects (Now section of todo.md):\n{context['todo_now'][:600]}\n\n"
        f"Pattern detected: these keywords appear in BOTH {sources_str} within the last 72h:\n"
        f"Keywords: {patterns_str}\n\n"
        f"Bookmarks sample ({detection['file_counts'].get('bookmarks',0)} files):\n"
        f"{detection['sources'].get('bookmarks',{}).get('sample','')[:400]}\n\n"
        f"Scout sample ({detection['file_counts'].get('scout',0)} files):\n"
        f"{detection['sources'].get('scout',{}).get('sample','')[:400]}\n\n"
        f"Write a Cross-Source Pattern Alert in this exact format:\n\n"
        f"# Cross-Source Pattern Alert\n"
        f"_Cycle type: daily | Generated: {today} | Output type: Cross-Source Pattern Alert_\n\n"
        f"**TL;DR:** [1-2 sentences: what's converging and why it matters now]\n\n"
        f"---\n\n## Pattern\n\n[Describe what's converging. Be specific.]\n\n"
        f"---\n\n## Connection to Active Work\n\n[Link to specific active projects, or say 'no direct connection' if none.]\n\n"
        f"---\n\n## Recommended action\n\n[1-3 concrete next steps]\n\n"
        f"---\n\n**Source:** bookmarks ({detection['file_counts'].get('bookmarks',0)} files) + scout ({detection['file_counts'].get('scout',0)} files), {today}\n"
    )
    return run_claude(prompt, verbose)


# ---------------------------------------------------------------------------
# Type 2: Project Momentum Pulse
# ---------------------------------------------------------------------------

def check_git_activity_for_project(project_name, days):
    """Check git log for commits touching a project's directory in the last N days."""
    try:
        since = (NOW - timedelta(days=days)).strftime("%Y-%m-%d")
        project_dir = f"projects/{project_name}/"
        # Also check project-adjacent paths (specs, raw data mentioning the project)
        result = subprocess.run(
            ["git", "-C", str(HEX_ROOT), "log", f"--since={since}", "--oneline",
             "--", project_dir],
            capture_output=True, text=True, timeout=10
        )
        return result.returncode == 0 and bool(result.stdout.strip())
    except Exception:
        return False


def check_boi_activity_for_project(project_name, days):
    """Check BOI queue for recent specs mentioning this project name."""
    boi_queue = Path.home() / ".boi" / "queue"
    if not boi_queue.exists():
        return False
    cutoff_ts = (NOW - timedelta(days=days)).timestamp()
    name_lower = project_name.lower()
    try:
        for f in boi_queue.iterdir():
            if f.is_file() and f.stat().st_mtime >= cutoff_ts:
                try:
                    content = f.read_text(encoding="utf-8", errors="replace").lower()
                    if name_lower in content:
                        return True
                except OSError:
                    pass
    except OSError:
        pass
    return False


def check_context_file_modified(project_name, days):
    """Check if the project's context.md was modified in the last N days."""
    context_file = HEX_ROOT / "projects" / project_name / "context.md"
    if not context_file.exists():
        return False
    try:
        mtime = context_file.stat().st_mtime
        cutoff_ts = (NOW - timedelta(days=days)).timestamp()
        return mtime >= cutoff_ts
    except OSError:
        return False


def is_project_blocked(project_line):
    """Check if a project line indicates it's blocked or waiting on something."""
    line_lower = project_line.lower()
    blocked_markers = ["waiting on", "blocked", "on hold", "paused", "waiting for"]
    return any(marker in line_lower for marker in blocked_markers)


def detect_momentum_drops(verbose):
    todo_text = read_file_safe(TODO_FILE)
    if not todo_text:
        if verbose:
            log("Type 2: todo.md not found -- skipping")
        return None

    projects = parse_now_section(todo_text)
    if not projects:
        if verbose:
            log("Type 2: no active projects in Now section")
        return None

    stalled = []
    active_names = []
    for i, project in enumerate(projects[:5]):
        rank = i + 1
        name = project["name"]

        # Skip projects explicitly marked as blocked/waiting
        if is_project_blocked(project["line"]):
            if verbose:
                log(f"Type 2: [{name}] skipped — marked as blocked/waiting")
            continue

        # Check three per-project activity signals
        git_active = check_git_activity_for_project(name, days=7)
        boi_active = check_boi_activity_for_project(name, days=7)
        context_modified = check_context_file_modified(name, days=7)

        if verbose:
            log(f"Type 2: [{name}] git={git_active}, boi={boi_active}, context={context_modified}")

        if not git_active and not boi_active and not context_modified:
            stalled.append({
                "name": name,
                "rank": rank,
                "line": project["line"][:100],
                "signals": {"git": False, "boi": False, "context_modified": False},
            })
        else:
            active_names.append(name)

    if not stalled:
        if verbose:
            log("Type 2: all projects have recent per-project activity")
        return None

    top_stalled = [p for p in stalled if p["rank"] <= 3]
    if not top_stalled:
        if verbose:
            log("Type 2: no rank 1-3 projects stalled")
        return None

    return {
        "stalled": top_stalled,
        "active_names": active_names,
    }


def generate_momentum_pulse(detection, context, dry_run, verbose):
    today = NOW.strftime("%Y-%m-%d")
    stalled_names = [p["name"] for p in detection["stalled"]]

    if dry_run:
        return (
            f"# Project Momentum Pulse\n"
            f"_Cycle type: daily | Generated: {today} | Output type: Project Momentum Pulse_\n\n"
            f"**TL;DR:** [DRY RUN -- would synthesize stalled projects: {stalled_names}]\n\n"
            f"**Source:** todo.md + git log + BOI queue (checked {today})\n"
        )

    prompt = (
        f"You are generating a Project Momentum Pulse for hex (personal AI system).\n\n"
        f"Personal context:\n{context['me'][:300]}\n\n"
        f"North Star:\n{context['north_star'][:300]}\n\n"
        f"The following high-priority projects show no system activity in the last 7 days:\n"
        f"{json.dumps(detection['stalled'], indent=2)}\n\n"
        f"Active projects (have recent activity): {detection['active_names']}\n"
        f"Each stalled project had zero git commits, zero BOI specs, and no context.md updates for 7+ days.\n\n"
        f"Write a Project Momentum Pulse in this format:\n\n"
        f"# Project Momentum Pulse\n"
        f"_Cycle type: daily | Generated: {today} | Output type: Project Momentum Pulse_\n\n"
        f"**TL;DR:** [1-2 sentences]\n\n"
        f"---\n\n## Stalled Projects\n\n[For each: name, rank, why it matters, likely blocker]\n\n"
        f"---\n\n## Momentum Assessment\n\n[Expected stall or unexpected? What does it signal?]\n\n"
        f"---\n\n## Recommended action\n\n[1-3 specific next steps]\n\n"
        f"---\n\n**Source:** todo.md Now section + git log + BOI queue (checked {today})\n"
    )
    return run_claude(prompt, verbose)


# ---------------------------------------------------------------------------
# Type 3: Research Feed Health Check
# ---------------------------------------------------------------------------

def check_feed_health():
    feed_configs = [
        {"id": "bookmarks", "label": "Bookmark watcher", "directory": BOOKMARKS_DIR, "expected_hours": 48},
        {"id": "scout",     "label": "Tech scout",        "directory": SCOUT_DIR,     "expected_hours": 48},
    ]

    feeds = {}
    any_silent = False

    for cfg in feed_configs:
        d = cfg["directory"]
        recent = get_recent_files(d, cfg["expected_hours"])
        all_files = all_non_index_files(d)

        if recent:
            last_file = recent[0]
            hours_since = (NOW.timestamp() - last_file.stat().st_mtime) / 3600
            feeds[cfg["id"]] = {
                "label": cfg["label"],
                "status": "healthy",
                "recent_count": len(recent),
                "hours_since_last": round(hours_since, 1),
                "last_file": last_file.name,
            }
        elif all_files:
            most_recent = max(all_files, key=lambda f: f.stat().st_mtime)
            days_since = (NOW.timestamp() - most_recent.stat().st_mtime) / 86400
            feeds[cfg["id"]] = {
                "label": cfg["label"],
                "status": "stale",
                "recent_count": 0,
                "days_since_last": round(days_since, 1),
                "last_file": most_recent.name,
            }
            any_silent = True
        else:
            feeds[cfg["id"]] = {
                "label": cfg["label"],
                "status": "never_generated",
                "recent_count": 0,
            }
            any_silent = True

    return {"feeds": feeds, "any_silent": any_silent, "checked_at": NOW.isoformat()}


def generate_feed_health_report(health, dry_run):
    today = NOW.strftime("%Y-%m-%d")
    feeds = health["feeds"]

    silent_labels = [f["label"] for f in feeds.values() if f["status"] != "healthy"]
    healthy_labels = [f["label"] for f in feeds.values() if f["status"] == "healthy"]

    if not silent_labels:
        tldr = "All research feeds are generating output at expected rates. Infrastructure is healthy."
    elif len(silent_labels) == len(feeds):
        tldr = f"All {len(feeds)} research feeds are silent. Generative layer is running blind on live data."
    else:
        tldr = (f"{len(silent_labels)} of {len(feeds)} feeds silent: {', '.join(silent_labels)}. "
                f"{len(healthy_labels)} feed(s) healthy.")

    lines = [
        "# Research Feed Health Check",
        f"_Cycle type: daily | Generated: {today} | Output type: Research Feed Health Check_",
        "",
        f"**TL;DR:** {tldr}",
        "",
        "---",
        "",
        "## Feed Status",
        "",
        "| Feed | Status | Last output | Recent files (48h) |",
        "|------|--------|-------------|---------------------|",
    ]

    for fid, feed in feeds.items():
        s = feed["status"]
        if s == "healthy":
            status_col = f"OK -- {feed['hours_since_last']}h ago"
            last_col = feed.get("last_file", "--")
            recent_col = str(feed.get("recent_count", 0))
        elif s == "stale":
            status_col = f"STALE -- {feed['days_since_last']}d ago"
            last_col = feed.get("last_file", "--")
            recent_col = "0"
        else:
            status_col = "CRITICAL -- never generated"
            last_col = "Never"
            recent_col = "0"
        lines.append(f"| {feed['label']} | {status_col} | {last_col} | {recent_col} |")

    lines += ["", "---", ""]

    if silent_labels:
        lines += [
            "## Impact",
            "",
            "With these feeds silent, the following generative output types cannot fire:",
            "",
            "- **Cross-Source Pattern Alert** (Type 1) -- requires 2+ active feeds",
            "- **Content Opportunity Draft** (Type 5) -- needs bookmark/scout signal",
            "- **Connection Surface** (Type 6) -- needs multi-feed data",
            "",
            "---",
            "",
        ]

    lines += ["## Recommended action", ""]
    if not silent_labels:
        lines.append("No action needed. Monitor on next daily cycle.")
    else:
        action_num = 1
        for fid, feed in feeds.items():
            if feed["status"] != "healthy":
                if fid == "bookmarks":
                    lines.append(
                        f"{action_num}. **{feed['label']}:** Verify `~/.boi/scripts/bookmark-watcher.py` "
                        f"is running and writing to `{BOOKMARKS_DIR}`"
                    )
                elif fid == "scout":
                    lines.append(
                        f"{action_num}. **{feed['label']}:** Verify hex-events `tech-scout-daily` policy "
                        f"is active. Script: `$HEX_DIR/.hex/scripts/tech-scout.sh`"
                    )
                action_num += 1

    lines += [
        "",
        "---",
        "",
        f"**Source:** File modification timestamps in `raw/research/bookmarks/` and "
        f"`raw/research/scout/` (checked {today})",
    ]

    prefix = "[DRY RUN] " if dry_run else ""
    return prefix + "\n".join(lines)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="Hex generative layer -- daily signal MVP")
    parser.add_argument("--cycle-type", choices=["daily", "weekly", "monthly"], default="daily")
    parser.add_argument("--dry-run", action="store_true",
                        help="Show plan and output previews without writing files or calling claude")
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()

    start_ms = int(time.time() * 1000)
    today = NOW.strftime("%Y-%m-%d")
    today_dir = OUTPUTS_DIR / today

    if args.cycle_type != "daily":
        log(f"cycle type '{args.cycle_type}' not yet implemented (Phase 1: daily only)")
        sys.exit(0)

    log(f"Starting daily cycle | HEX_DIR={HEX_ROOT} | dry_run={args.dry_run}")

    context = load_context()

    # ---- Candidate evaluation ----
    candidates = []

    log("Checking Type 1: Cross-Source Pattern Alert...")
    pattern_detection = detect_cross_source_patterns(verbose=args.verbose)
    if pattern_detection:
        candidates.append(("cross-source-pattern", pattern_detection, 10))
        log(f"  -> CANDIDATE (priority 10) -- patterns: {pattern_detection['patterns'][:3]}")
    else:
        log("  -> skipped (no cross-source patterns)")

    log("Checking Type 2: Project Momentum Pulse...")
    momentum_detection = detect_momentum_drops(verbose=args.verbose)
    if momentum_detection:
        candidates.append(("momentum-pulse", momentum_detection, 9))
        log(f"  -> CANDIDATE (priority 9) -- {len(momentum_detection['stalled'])} stalled project(s)")
    else:
        log("  -> skipped (no momentum drops)")

    log("Checking Type 3: Research Feed Health Check...")
    health = check_feed_health()
    priority = 8 if health["any_silent"] else 5
    candidates.append(("feed-health", health, priority))
    log(f"  -> CANDIDATE (priority {priority}) -- any_silent={health['any_silent']}")

    # Sort by priority, select top DAILY_OUTPUT_CAP
    candidates.sort(key=lambda c: c[2], reverse=True)
    selected = candidates[:DAILY_OUTPUT_CAP]
    log(f"Selected {len(selected)} of {len(candidates)} candidates (cap={DAILY_OUTPUT_CAP})")

    # ---- Generate outputs ----
    written = []
    filename_map = {
        "cross-source-pattern": "001-cross-source-pattern-alert.md",
        "momentum-pulse":       "002-project-momentum-pulse.md",
        "feed-health":          "003-research-feed-health.md",
    }

    for output_type, detection, priority in selected:
        log(f"Generating: {output_type} (priority={priority})...")

        if output_type == "cross-source-pattern":
            content = generate_pattern_alert(detection, context, args.dry_run, args.verbose)
        elif output_type == "momentum-pulse":
            content = generate_momentum_pulse(detection, context, args.dry_run, args.verbose)
        elif output_type == "feed-health":
            content = generate_feed_health_report(detection, args.dry_run)
        else:
            continue

        if content:
            filename = filename_map[output_type]
            output_path = write_output(filename, content, today_dir, args.dry_run)
            if output_path:
                written.append(filename)
                if args.dry_run:
                    print(content[:500] + ("..." if len(content) > 500 else ""))
                    print()

    # ---- Update state ----
    duration_ms = int(time.time() * 1000) - start_ms
    update_cycle_log(written, len(candidates), duration_ms, args.dry_run)

    log(f"Done -- {len(written)} output(s) in {duration_ms}ms")
    for name in written:
        print(f"  -> {today_dir / name}")


if __name__ == "__main__":
    main()
