#!/usr/bin/env python3
"""
hex-feedback-loops.py — check health of all feedback loops defined in
docs/hex-system-design-v2.md.

Outputs JSON to stdout. Non-zero exit if any loop is broken.

Usage:
    python3 hex-feedback-loops.py           # full report
    python3 hex-feedback-loops.py --loop l1 # single loop
    python3 hex-feedback-loops.py --alert   # also emit hex-events for broken loops
"""

import argparse
import json
import os
import sys
import time
from datetime import datetime, timezone

HEX_ROOT = os.environ.get("HEX_ROOT", os.path.expanduser("~/hex"))
AUDIT_DIR = os.path.expanduser("~/.hex/audit")
TELEMETRY_PATH = os.path.join(HEX_ROOT, ".hex", "telemetry")

# Data sources for each loop
KR_SNAPSHOTS = os.path.join(AUDIT_DIR, "kr-snapshots.jsonl")
PATTERNS_LIBRARY = os.path.join(AUDIT_DIR, "patterns.jsonl")
LOOP_HISTORY = os.path.join(AUDIT_DIR, "initiative-loop-history.jsonl")
MEMORY_EFFECTIVENESS = os.path.join(AUDIT_DIR, "memory-effectiveness.jsonl")
PULSE_MESSAGES = os.path.join(AUDIT_DIR, "pulse-messages.jsonl")
WATCHDOG_HEARTBEAT = os.path.join(AUDIT_DIR, "watchdog-heartbeat.jsonl")
APPROACH_LIBRARY = os.path.join(AUDIT_DIR, "tuning-recommendations.jsonl")
BEHAVIORAL_DB = os.path.join(HEX_ROOT, ".hex", "memory.db")

# A loop is "broken" if no full cycle in this window
BROKEN_THRESHOLD_SECONDS = 6 * 3600   # 6h
STALLED_THRESHOLD_SECONDS = 24 * 3600 # 24h


# ── helpers ───────────────────────────────────────────────────────────────────

def _now() -> float:
    return time.time()


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def _cutoff(hours: float) -> float:
    return _now() - hours * 3600


def _parse_ts(val) -> float:
    if not val:
        return 0.0
    try:
        if isinstance(val, (int, float)):
            return float(val)
        dt = datetime.fromisoformat(str(val).replace("Z", "+00:00"))
        return dt.timestamp()
    except Exception:
        try:
            return float(val)
        except Exception:
            return 0.0


def _iso(epoch: float):
    if epoch <= 0:
        return None
    return datetime.fromtimestamp(epoch, tz=timezone.utc).isoformat(timespec="seconds")


def _read_jsonl(path: str) -> list:
    if not os.path.exists(path):
        return []
    rows = []
    try:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    except OSError:
        pass
    return rows


def _last_ts(rows: list, ts_key: str = "ts") -> float:
    best = 0.0
    for r in rows:
        t = _parse_ts(r.get(ts_key, ""))
        if t > best:
            best = t
    return best


def _count_since(rows: list, cutoff: float, ts_key: str = "ts") -> int:
    return sum(1 for r in rows if _parse_ts(r.get(ts_key, "")) >= cutoff)


# ── L1: Outcome → Pivot ───────────────────────────────────────────────────────

def check_l1() -> dict:
    """
    Trigger  : kr-snapshots.jsonl  (Step 1 of initiative loop fires, records KR values)
    Action   : patterns.jsonl      (Step 8 self-assess fires, logs what worked/pivoted)
    Feedback : initiative-loop-history.jsonl (loop runs complete)

    Healthy  : snapshots taken recently AND patterns written recently
    Stalled  : snapshots firing but no patterns (Step 8 not reaching pivot dispatch)
    Broken   : no snapshots in 24h (trigger not firing)
    """
    c24 = _cutoff(24)
    snapshots = _read_jsonl(KR_SNAPSHOTS)
    patterns  = _read_jsonl(PATTERNS_LIBRARY)
    history   = _read_jsonl(LOOP_HISTORY)

    trigger_count  = _count_since(snapshots, c24)
    action_count   = _count_since(patterns,  c24)
    feedback_count = _count_since(history,   c24)

    last_pattern_ts  = _last_ts(patterns)
    last_snapshot_ts = _last_ts(snapshots)

    if trigger_count == 0:
        status = "broken"
    elif action_count == 0:
        # Snapshots exist but Step 8 never produced a pattern — stalled
        status = "stalled"
    elif _now() - last_pattern_ts > BROKEN_THRESHOLD_SECONDS:
        status = "stalled"
    else:
        status = "healthy"

    return {
        "name": "outcome-to-pivot",
        "label": "L1: Outcome → Pivot",
        "description": "KR measurement → stall detection → pivot spec dispatched",
        "status": status,
        "last_cycle": _iso(last_pattern_ts),
        "trigger_count_24h": trigger_count,
        "action_count_24h": action_count,
        "feedback_count_24h": feedback_count,
        "notes": (
            "Stalled: KR snapshots fire but Step 8 pivot logic has not logged patterns. "
            "Self-assess wired by t-3 but approach-library still empty."
            if status == "stalled" else
            "Broken: no KR snapshots in 24h — initiative loop not running."
            if status == "broken" else ""
        ),
    }


# ── L2: Error → Lesson ────────────────────────────────────────────────────────

def check_l2() -> dict:
    """
    Trigger  : memory-effectiveness.jsonl  (corrections logged after Mike feedback)
    Action   : memory.db (behavioral_patterns table)              (correction classified + written to DB)
    Feedback : (check-behavior queries at session start)

    Broken if memory.db (behavioral_patterns table) doesn't exist (M7 not implemented).
    """
    c24 = _cutoff(24)
    effectiveness = _read_jsonl(MEMORY_EFFECTIVENESS)
    trigger_count = _count_since(effectiveness, c24)

    action_count   = 0
    feedback_count = 0
    behavioral_exists = os.path.exists(BEHAVIORAL_DB)

    if behavioral_exists:
        try:
            import sqlite3
            conn = sqlite3.connect(BEHAVIORAL_DB)
            cur  = conn.cursor()
            cur.execute(
                "SELECT COUNT(*) FROM behavioral_patterns "
                "WHERE first_seen >= datetime('now', '-1 day')"
            )
            row = cur.fetchone()
            action_count = row[0] if row else 0
            conn.close()
        except Exception:
            pass

    if not behavioral_exists:
        status = "broken"
    elif action_count == 0:
        status = "stalled"
    else:
        status = "healthy"

    return {
        "name": "error-to-lesson",
        "label": "L2: Error → Lesson",
        "description": "Correction received → classified → behavioral_patterns DB → queried at session start",
        "status": status,
        "last_cycle": None,
        "trigger_count_24h": trigger_count,
        "action_count_24h": action_count,
        "feedback_count_24h": feedback_count,
        "notes": (
            "Broken: memory.db (behavioral_patterns table) not yet built. Corrections accumulate in memory-effectiveness.jsonl "
            "but agents never query them. t-6 closes this loop."
            if status == "broken" else
            "Stalled: DB exists but no patterns written in 24h."
            if status == "stalled" else ""
        ),
    }


# ── L3: Outcome → Threshold ───────────────────────────────────────────────────

def check_l3() -> dict:
    """
    Trigger  : patterns.jsonl populated (approach-library entries logged by L1)
    Action   : tuning-recommendations.jsonl  (fleet-lead reads patterns, proposes charter changes)
    Feedback : charter.yaml modifications (agent params tuned)

    Depends on L1 being healthy. Currently stalled because fleet-lead doesn't
    read tuning-recommendations.jsonl.
    """
    c24 = _cutoff(24)

    patterns = _read_jsonl(PATTERNS_LIBRARY)
    trigger_count = _count_since(patterns, c24)

    approach_rows = _read_jsonl(APPROACH_LIBRARY)
    action_count  = _count_since(approach_rows, c24)

    last_approach_ts = _last_ts(approach_rows)

    if len(patterns) == 0:
        status = "broken"  # L1 hasn't produced anything for L3 to consume
    elif len(approach_rows) == 0:
        status = "stalled"  # patterns exist, fleet-lead not reading them
    elif _now() - last_approach_ts > STALLED_THRESHOLD_SECONDS:
        status = "stalled"
    elif action_count == 0:
        status = "stalled"
    else:
        status = "healthy"

    return {
        "name": "outcome-to-threshold",
        "label": "L3: Outcome → Threshold",
        "description": "Pattern library → fleet-lead reads → agent charter parameters tuned",
        "status": status,
        "last_cycle": _iso(last_approach_ts),
        "trigger_count_24h": trigger_count,
        "action_count_24h": action_count,
        "feedback_count_24h": 0,
        "notes": (
            "Stalled: patterns.jsonl has entries but tuning-recommendations.jsonl is empty. "
            "fleet-lead must be wired to read patterns and write proposed tunings."
            if status == "stalled" else
            "Broken: patterns.jsonl empty — L1 must produce patterns before L3 can fire."
            if status == "broken" else ""
        ),
    }


# ── L4: Failure → Redesign ────────────────────────────────────────────────────

def check_l4() -> dict:
    """
    Trigger  : ≥3 consecutive pivot failures in patterns.jsonl
    Action   : escalation message in pulse-messages.jsonl or BOI spec dispatch
    Feedback : structural redesign BOI spec created

    Currently stalled: escalation chain designed but not wired.
    """
    c24 = _cutoff(24)

    patterns   = _read_jsonl(PATTERNS_LIBRARY)
    pulse_msgs = _read_jsonl(PULSE_MESSAGES)

    failure_patterns = [
        p for p in patterns
        if p.get("outcome") in ("failure", "fail", "pivot_fail")
        or (isinstance(p.get("approach_type"), str) and "fail" in p.get("approach_type", "").lower())
    ]
    trigger_count = _count_since(failure_patterns, c24)

    escalations = [
        m for m in pulse_msgs
        if any(kw in str(m.get("text", "")).lower()
               for kw in ("escalat", "redesign", "cascade", "structural"))
        or m.get("type") == "escalation"
    ]
    action_count = _count_since(escalations, c24)

    consecutive = _consecutive_pivot_fails(patterns)

    last_escalation_ts = _last_ts(escalations)

    # L4 is only "broken" if failures that should trigger it exist but didn't
    if consecutive >= 3 and action_count == 0:
        status = "broken"
    elif consecutive >= 3 and action_count > 0:
        status = "healthy"
    else:
        # No consecutive failures yet → loop is "stalled" (designed, not yet activated)
        status = "stalled"

    return {
        "name": "failure-to-redesign",
        "label": "L4: Failure → Redesign",
        "description": "≥3 consecutive pivot failures → escalation → structural redesign spec",
        "status": status,
        "last_cycle": _iso(last_escalation_ts),
        "trigger_count_24h": trigger_count,
        "action_count_24h": action_count,
        "feedback_count_24h": 0,
        "consecutive_pivot_fails": consecutive,
        "notes": (
            f"Broken: {consecutive} consecutive pivot failures detected but no escalation fired. "
            "Escalation chain not yet wired."
            if status == "broken" else
            "Stalled: designed in self-improvement-loop-design-2026-04-25.md but escalation "
            "logic not yet triggered. No consecutive failures have occurred."
            if status == "stalled" else ""
        ),
    }


def _consecutive_pivot_fails(patterns: list) -> int:
    """Count consecutive failure entries at the end of patterns log."""
    count = 0
    for p in reversed(patterns):
        outcome = p.get("outcome", "")
        approach = p.get("approach_type", "")
        if outcome in ("failure", "fail", "pivot_fail") or "fail" in str(approach).lower():
            count += 1
        else:
            break
    return count


# ── Overall ───────────────────────────────────────────────────────────────────

def _overall(loops: list) -> str:
    statuses = {l["status"] for l in loops}
    if "broken" in statuses:
        return "broken"
    if "stalled" in statuses:
        return "degraded"
    return "healthy"


# ── Alert emission ────────────────────────────────────────────────────────────

def _emit_alerts(loops: list) -> None:
    """Emit hex.feedback_loop.broken for each loop that is broken and has been >6h."""
    sys.path.insert(0, TELEMETRY_PATH)
    try:
        from emit import emit  # type: ignore
    except ImportError:
        print("[hex-feedback-loops] WARN: telemetry emit not available", file=sys.stderr)
        return

    for loop in loops:
        if loop["status"] != "broken":
            continue
        last_epoch = _parse_ts(loop.get("last_cycle") or "")
        if last_epoch > 0 and (_now() - last_epoch) < BROKEN_THRESHOLD_SECONDS:
            continue  # broken but recently — not yet past 6h threshold
        emit(
            "hex.feedback_loop.broken",
            {
                "loop_name": loop["name"],
                "loop_label": loop.get("label", loop["name"]),
                "status": loop["status"],
                "trigger_count_24h": loop.get("trigger_count_24h", 0),
                "action_count_24h": loop.get("action_count_24h", 0),
                "last_cycle": loop.get("last_cycle"),
                "notes": loop.get("notes", ""),
            },
            source="hex-feedback-loops",
        )


# ── Pulse integration (importable) ────────────────────────────────────────────

def collect_feedback_loops() -> dict:
    """
    Importable by pulse/server.py as a 'System Health' data source.

    Returns a dict ready to merge into get_all_metrics() output.
    """
    loops = [check_l1(), check_l2(), check_l3(), check_l4()]
    overall = _overall(loops)
    return {
        "overall": overall,
        "loops": loops,
        "summary": {
            "healthy": sum(1 for l in loops if l["status"] == "healthy"),
            "stalled": sum(1 for l in loops if l["status"] == "stalled"),
            "broken":  sum(1 for l in loops if l["status"] == "broken"),
        },
    }


# ── Entry point ───────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Report health of hex feedback loops (L1-L4)."
    )
    parser.add_argument(
        "--alert", action="store_true",
        help="Emit hex-events alerts for any loop broken >6h",
    )
    parser.add_argument(
        "--loop", metavar="NAME",
        help="Check a single loop by name: l1, l2, l3, l4",
    )
    args = parser.parse_args()

    checkers = {"l1": check_l1, "l2": check_l2, "l3": check_l3, "l4": check_l4}

    if args.loop:
        key = args.loop.lower()
        if key not in checkers:
            print(
                f"Unknown loop '{args.loop}'. Valid: {', '.join(checkers)}",
                file=sys.stderr,
            )
            sys.exit(1)
        loops = [checkers[key]()]
    else:
        loops = [fn() for fn in checkers.values()]

    overall = _overall(loops)
    report = {
        "ts": _now_iso(),
        "overall": overall,
        "loops": loops,
        "summary": {
            "healthy": sum(1 for l in loops if l["status"] == "healthy"),
            "stalled": sum(1 for l in loops if l["status"] == "stalled"),
            "broken":  sum(1 for l in loops if l["status"] == "broken"),
        },
    }

    print(json.dumps(report, indent=2))

    if args.alert:
        _emit_alerts(loops)

    if overall == "broken":
        sys.exit(1)


if __name__ == "__main__":
    main()
