#!/usr/bin/env python3
"""
backlog-promote.py — Wake-side helper for backlog.md → active queue promotion.

Reads projects/<agent-id>/backlog.md and promotes 1-2 unchecked items into
the agent's active queue (state.json) when conditions are met.

Critic-reviewed guards (all required per 2026-05-05 critic pass):
  Guard 1: promoted_this_wake mutex — one promotion path per wake cycle
  Guard 2: [~] in-progress items count as active; no promote if any in-progress
  Guard 3: per-period promote counter capped at 4
  Guard 4: quality_gate must be ≥10 chars and not a placeholder (TBD, -, etc.)
  Guard 5: last_act timestamp must be >2h ago (uses trail entry ts = act-end time)

Usage:
  python3 backlog-promote.py <agent-id> [--dry-run]

Returns:
  0 if items were promoted (or dry-run found eligible items)
  1 if no promotion occurred (all gates blocked or nothing eligible)
  2 on error
"""

import json
import re
import sys
import os
import time
from datetime import datetime, timezone
from pathlib import Path


HEX_DIR = Path(os.environ.get("HEX_DIR", Path.home() / "mrap-hex"))
PROJECTS_DIR = HEX_DIR / "projects"
AUDIT_DIR = HEX_DIR / ".hex" / "audit"

# Guard thresholds
ACT_COOLDOWN_SECONDS = 7200       # Guard 5: 2h since last act
BUDGET_CEILING_RATIO = 0.80       # Guard budget: stop if >80% spent
PERIOD_PROMOTE_CAP = 4            # Guard 3: max backlog promotes per period
WAKE_CEILING = 2                  # max items promoted per wake
MIN_QUALITY_GATE_LEN = 10         # Guard 4: minimum quality_gate content length
PLACEHOLDER_PATTERNS = re.compile(
    r"^\s*(tbd|todo|n/?a|\-|none|placeholder|improves things)\s*$", re.IGNORECASE
)


def parse_backlog(backlog_path: Path):
    """Parse backlog.md and return lists of unchecked, in-progress, and done items."""
    unchecked = []
    in_progress = []
    if not backlog_path.exists():
        return unchecked, in_progress

    for lineno, raw in enumerate(backlog_path.read_text().splitlines(), 1):
        line = raw.strip()
        # In-progress: - [~]
        if re.match(r"^- \[~\]", line):
            in_progress.append({"line": lineno, "raw": raw, "text": line})
        # Unchecked: - [ ]
        elif re.match(r"^- \[ \]", line):
            unchecked.append({"line": lineno, "raw": raw, "text": line})

    return unchecked, in_progress


def extract_quality_gate(item_text: str) -> str:
    """Extract quality_gate value from backlog item text."""
    m = re.search(r"\|\s*quality_gate:\s*(.+?)(?:\s*\|.*)?$", item_text, re.IGNORECASE)
    if m:
        return m.group(1).strip()
    return ""


def is_valid_quality_gate(gate: str) -> bool:
    """Guard 4: quality_gate must be substantive, not a placeholder."""
    if len(gate) < MIN_QUALITY_GATE_LEN:
        return False
    if PLACEHOLDER_PATTERNS.match(gate):
        return False
    return True


def last_act_age_seconds(trail: list) -> float:
    """Guard 5: return seconds since last type=act trail entry. inf if none."""
    now = datetime.now(timezone.utc)
    for entry in reversed(trail):
        if entry.get("type") == "act":
            try:
                ts_str = entry["ts"]
                # Handle both Z and +00:00 suffixes
                ts_str = ts_str.replace("Z", "+00:00")
                ts = datetime.fromisoformat(ts_str)
                age = (now - ts).total_seconds()
                return age
            except Exception:
                pass
    return float("inf")


def mark_in_progress(backlog_path: Path, items: list):
    """Mark selected items as [~] in-progress in backlog.md."""
    lines = backlog_path.read_text().splitlines()
    target_linenos = {item["line"] for item in items}
    for i, raw in enumerate(lines, 1):
        if i in target_linenos:
            # Replace - [ ] with - [~]
            lines[i - 1] = raw.replace("- [ ]", "- [~]", 1)
    tmp = backlog_path.with_suffix(".md.tmp")
    tmp.write_text("\n".join(lines) + "\n")
    tmp.rename(backlog_path)


def load_state(state_path: Path) -> dict:
    if not state_path.exists():
        return {}
    return json.loads(state_path.read_text())


def save_state(state_path: Path, state: dict):
    tmp = state_path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(state, indent=2))
    tmp.rename(state_path)


def log_audit(agent_id: str, action: str, payload: dict):
    AUDIT_DIR.mkdir(parents=True, exist_ok=True)
    audit_file = AUDIT_DIR / "actions.jsonl"
    entry = {
        "ts": datetime.now(timezone.utc).isoformat(),
        "agent_id": agent_id,
        "action": action,
        **payload,
    }
    with open(audit_file, "a") as f:
        f.write(json.dumps(entry) + "\n")


def run(agent_id: str, dry_run: bool) -> int:
    project_dir = PROJECTS_DIR / agent_id
    backlog_path = project_dir / "backlog.md"
    state_path = project_dir / "state.json"

    state = load_state(state_path)
    if not state:
        print(f"[{agent_id}] No state.json found — skipping", file=sys.stderr)
        return 1

    # --- Guard 1: promoted_this_wake mutex ---
    # Clear stale flag if wake_count advanced since it was set
    bp_meta = state.get("backlog_promote_meta", {})
    flag_wake = bp_meta.get("promoted_wake_count", -1)
    current_wake = state.get("wake_count", 0)
    if bp_meta.get("promoted_this_wake") and flag_wake < current_wake:
        bp_meta["promoted_this_wake"] = False
    if bp_meta.get("promoted_this_wake"):
        print(f"[{agent_id}] Guard 1: already promoted this wake — skipping")
        return 1

    # --- Parse backlog ---
    unchecked, in_progress = parse_backlog(backlog_path)

    # --- Guard 2: in-progress items count as active ---
    active_queue = state.get("queue", {}).get("active", [])
    if in_progress:
        print(
            f"[{agent_id}] Guard 2: {len(in_progress)} in-progress backlog items — "
            f"treating as active; skipping promotion"
        )
        return 1
    if active_queue:
        print(f"[{agent_id}] Guard 2: active queue has {len(active_queue)} items — skipping")
        return 1

    # --- Guard 3: per-period promote counter ---
    current_period_start = (
        state.get("cost", {}).get("current_period", {}).get("start", "")
    )
    period_key = bp_meta.get("period_start", "")
    if current_period_start != period_key:
        # New period — reset counter
        bp_meta["period_start"] = current_period_start
        bp_meta["period_promotes"] = 0
    period_promotes = bp_meta.get("period_promotes", 0)
    if period_promotes >= PERIOD_PROMOTE_CAP:
        print(
            f"[{agent_id}] Guard 3: period promote cap reached "
            f"({period_promotes}/{PERIOD_PROMOTE_CAP}) — skipping"
        )
        return 1

    # --- Budget check ---
    cost = state.get("cost", {}).get("current_period", {})
    spent = cost.get("spent_usd", 0.0)
    budget = cost.get("budget_usd", 0.0)
    if budget > 0 and spent > budget * BUDGET_CEILING_RATIO:
        print(
            f"[{agent_id}] Budget gate: {spent:.4f}/{budget:.4f} USD "
            f"(>{BUDGET_CEILING_RATIO*100:.0f}%) — skipping"
        )
        return 1

    # --- Guard 5: last_act_entry_age > 2h ---
    trail = state.get("trail", [])
    act_age = last_act_age_seconds(trail)
    if act_age < ACT_COOLDOWN_SECONDS:
        print(
            f"[{agent_id}] Guard 5: last act {act_age/3600:.1f}h ago "
            f"(< {ACT_COOLDOWN_SECONDS//3600}h cooldown) — skipping"
        )
        return 1

    # --- Guard 4: filter items with valid quality_gate ---
    eligible = []
    for item in unchecked:
        gate = extract_quality_gate(item["text"])
        if is_valid_quality_gate(gate):
            eligible.append(item)
        else:
            print(f"[{agent_id}] Guard 4: skipping item (invalid quality_gate: '{gate}'): "
                  f"{item['text'][:80]}")

    if not eligible:
        print(f"[{agent_id}] No eligible backlog items after quality gate filter")
        return 1

    # --- Select items to promote ---
    slots_remaining = PERIOD_PROMOTE_CAP - period_promotes
    to_promote = eligible[: min(WAKE_CEILING, slots_remaining)]

    if dry_run:
        print(f"[{agent_id}] DRY-RUN: would promote {len(to_promote)} item(s):")
        for item in to_promote:
            print(f"  {item['text'][:100]}")
        return 0

    # --- Apply promotion ---
    now_iso = datetime.now(timezone.utc).isoformat()
    mark_in_progress(backlog_path, to_promote)

    for item in to_promote:
        active_queue.append({
            "id": f"backlog-md-{item['line']}",
            "summary": re.sub(r"\s*\|.*$", "", item["text"].replace("- [~]", "").strip()),
            "priority": 10,
            "created": now_iso,
            "source": "backlog-md-auto-promote",
        })

    state.setdefault("queue", {})["active"] = active_queue
    bp_meta["promoted_this_wake"] = True
    bp_meta["promoted_wake_count"] = current_wake
    bp_meta["period_promotes"] = period_promotes + len(to_promote)
    state["backlog_promote_meta"] = bp_meta
    save_state(state_path, state)

    log_audit(agent_id, "backlog-md-auto-promoted", {
        "promoted": len(to_promote),
        "period_promotes_total": bp_meta["period_promotes"],
        "last_act_age_h": round(act_age / 3600, 2),
        "items": [i["text"][:80] for i in to_promote],
    })

    print(f"[{agent_id}] Promoted {len(to_promote)} backlog item(s) to active queue")
    for item in to_promote:
        print(f"  + {item['text'][:100]}")
    return 0


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <agent-id> [--dry-run]", file=sys.stderr)
        sys.exit(2)

    agent_id = sys.argv[1]
    dry_run = "--dry-run" in sys.argv

    try:
        rc = run(agent_id, dry_run)
        sys.exit(rc)
    except Exception as e:
        print(f"[{agent_id}] ERROR: {e}", file=sys.stderr)
        import traceback
        traceback.print_exc(file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
