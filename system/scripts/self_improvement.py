#!/usr/bin/env python3
"""Self-improvement feedback loop for initiative KRs.

Detect stalls → diagnose cause → pivot to different approach → track pivots.
Called by hex-initiative-loop-v2.py (step 9: self_assess every 5 runs).

Reads:  ~/.hex/audit/kr-snapshots.jsonl  (flat format from initiative-watchdog)
Writes: ~/.hex/audit/pivots.jsonl, ~/.hex/audit/patterns.jsonl
"""

import json
import os
import re
import subprocess
import tempfile
from datetime import datetime, timezone

AUDIT_DIR = os.path.expanduser("~/.hex/audit")
PIVOTS_LOG = os.path.join(AUDIT_DIR, "pivots.jsonl")
PATTERN_LIBRARY = os.path.join(AUDIT_DIR, "patterns.jsonl")
SNAPSHOTS_LOG = os.path.join(AUDIT_DIR, "kr-snapshots.jsonl")

MAX_PIVOTS_BEFORE_ESCALATE = 3
STALL_WINDOW_DAYS = 7


def run_self_assess(agent_id, initiatives, dry_run=False):
    """Entry point called by initiative-loop-v2 step 9 (every 5 runs).

    initiatives: list of initiative dicts (not (path, data) tuples).
    Returns list of action dicts describing what was done.
    """
    snapshots = _load_jsonl(SNAPSHOTS_LOG)
    pivots = _load_jsonl(PIVOTS_LOG)
    pattern_library = _load_jsonl(PATTERN_LIBRARY)
    actions = []

    for initiative in initiatives:
        init_id = initiative.get("id", "unknown")
        for kr in initiative.get("key_results") or []:
            if kr.get("status") == "met":
                continue
            kr_id = kr.get("id", "kr-?")
            if not is_stalled(kr_id, init_id, snapshots):
                _maybe_log_success(initiative, kr, pattern_library, dry_run)
                continue

            kr_pivots = [p for p in pivots
                         if p.get("initiative_id") == init_id and p.get("kr_id") == kr_id]
            pivot_num = len(kr_pivots)

            if pivot_num >= MAX_PIVOTS_BEFORE_ESCALATE:
                if not any(p.get("escalated") for p in kr_pivots):
                    action = _escalate_kr(initiative, kr, kr_pivots, dry_run)
                    actions.append(action)
                continue

            category, reason = diagnose(kr, initiative)
            applicable_pattern = find_applicable_patterns(kr, pattern_library)

            spec_content = generate_pivot_spec(
                kr, initiative,
                {"category": category, "reason": reason},
                kr_pivots, pivot_num + 1, applicable_pattern,
            )

            if not dry_run:
                queue_id = _dispatch_spec(spec_content, init_id, kr_id)
                _append_pivot(init_id, kr_id, category, kr_pivots,
                              spec_content[:200], queue_id, reason, pivot_num + 1)
            else:
                queue_id = f"q-DRY-{init_id}-{kr_id}"

            actions.append({
                "action": "pivot_dispatch",
                "initiative_id": init_id,
                "kr_id": kr_id,
                "stall_category": category,
                "pivot_num": pivot_num + 1,
                "spec_id": queue_id,
                "dry_run": dry_run,
            })

    return actions


def is_stalled(kr_id, initiative_id, snapshots, window_days=STALL_WINDOW_DAYS):
    """True if KR value hasn't changed in window_days.

    Reads the flat snapshot format: {ts, snapshot: {"init/kr": value}}.
    """
    key = f"{initiative_id}/{kr_id}"
    window_hours = window_days * 24
    recent_values = [
        s["snapshot"][key]
        for s in snapshots
        if isinstance(s.get("snapshot"), dict)
        and key in s["snapshot"]
        and _age_hours(s.get("ts", "")) <= window_hours
    ]
    if len(recent_values) < 2:
        return False
    try:
        vals = [float(v) for v in recent_values if v is not None]
    except (TypeError, ValueError):
        return False
    return len(vals) >= 2 and max(vals) == min(vals)


def diagnose(kr, initiative):
    """Return (category, reason) for why this KR is stalled."""
    cmd = (kr.get("metric") or {}).get("command", "")
    if re.match(r"^\s*echo\s+\d+\.?\d*\s*$", cmd.strip()):
        return "FAKE_METRIC", f"Metric '{cmd.strip()}' is hardcoded — will never reflect real state"

    if cmd:
        try:
            result = subprocess.run(
                cmd, shell=True, capture_output=True, text=True, timeout=15,
            )
            if result.returncode != 0:
                return "BROKEN_METRIC", f"Exit {result.returncode}: {result.stderr[:200]}"
            val = result.stdout.strip()
            if not val or val.lower() in ("none", "null", ""):
                return "BROKEN_METRIC", f"Command returned empty/None: '{val}'"
        except subprocess.TimeoutExpired:
            return "BROKEN_METRIC", "Metric command timed out after 15s"
        except Exception as exc:
            return "BROKEN_METRIC", f"Command error: {exc}"

    spec_ids = initiative.get("specs") or []
    if not spec_ids:
        return "NO_SPECS", "No specs have been dispatched for this initiative"

    try:
        boi_out = subprocess.check_output(
            ["boi", "status", "--json"], stderr=subprocess.DEVNULL, timeout=15,
        )
        boi_status = json.loads(boi_out)
        spec_statuses = {s["id"]: s["status"] for s in boi_status if s.get("id") in spec_ids}
        all_failed = spec_ids and all(
            spec_statuses.get(s, "unknown") in ("failed", "cancelled") for s in spec_ids
        )
        if all_failed:
            return "SPEC_FAILED", f"All dispatched specs failed: {list(spec_statuses.keys())}"
    except Exception:
        pass

    return (
        "WRONG_HYPOTHESIS",
        f"Specs dispatched and completed but metric unchanged (current={kr.get('current')})",
    )


def generate_pivot_spec(kr, initiative, diagnosis, past_pivots, pivot_num, pattern_hint=None):
    """Generate a pivot spec string for a stalled KR."""
    category = diagnosis["category"]
    why_failed = diagnosis["reason"]
    past_spec_ids = [p.get("new_approach") for p in past_pivots if p.get("new_approach")]

    avoid_clause = (
        f"\nDo NOT repeat what {', '.join(past_spec_ids)} did. These approaches failed.\n"
        if past_spec_ids else ""
    )
    pattern_clause = (
        f"\nRecommended approach from similar successful KR: {pattern_hint}\n"
        if pattern_hint else ""
    )
    strategy = {
        "FAKE_METRIC": (
            "The metric command is hardcoded. Replace it with a script that reads "
            "actual system state and returns a number reflecting the KR description."
        ),
        "BROKEN_METRIC": (
            "The metric command is broken. Fix it so it runs cleanly and returns "
            "a numeric value proportional to real progress on the KR."
        ),
        "WRONG_HYPOTHESIS": (
            "Previous specs did not move this metric. Attack the KR description directly — "
            "the single action that causes the metric command to return a higher number."
        ),
        "SPEC_FAILED": (
            "Previous specs failed during execution. Diagnose the failure root cause "
            "(missing file, wrong path, env issue) and fix it before retrying."
        ),
        "NO_SPECS": (
            "No work has been done on this KR. Dispatch the first spec now, "
            "directly targeting the KR description."
        ),
    }.get(category, "Move the KR metric from current to target.")

    metric_cmd = (kr.get("metric") or {}).get("command", "echo 0")
    return f"""# KR Pivot: {initiative.get('id')}/{kr.get('id')} (attempt {pivot_num})

**Workspace:** {os.environ.get('HEX_ROOT', os.path.expanduser('~/hex'))}
**Mode:** execute

## Context

Initiative: {initiative.get('goal', initiative.get('id'))}
KR: {kr.get('description')}
Current: {kr.get('current', 0)} / {kr.get('target', '?')} target
Metric: `{metric_cmd}`
Stall category: {category}
Stall reason: {why_failed}
{avoid_clause}{pattern_clause}
## Strategy

{strategy}

## Tasks

### t-1: Measure baseline
PENDING

**Spec:** Run `{metric_cmd}` and record the current output.
If the command fails, diagnose why — the metric must work before any other step.

**Verify:** Command exits 0 and returns a numeric value.

### t-2: Move {kr.get('id')} toward target
PENDING

**Spec:** {kr.get('description')}

Take the single most direct action that increases the value returned by:
`{metric_cmd}`
{avoid_clause}
**Verify:** Run the metric command again. Value must be > {kr.get('current', 0)}.

### t-3: Update initiative YAML with new measurement
PENDING

**Spec:** After t-2 passes, update `initiatives/{initiative.get('id')}.yaml`:
- Set `current` on KR {kr.get('id')} to the measured value
- Set `measured_at` to now (ISO 8601)
- Add this spec's queue ID to the `specs` list

**Verify:** python3 -c "import yaml; d=yaml.safe_load(open('initiatives/{initiative.get('id')}.yaml')); kr=[k for k in d['key_results'] if k['id']=='{kr.get('id')}'][0]; assert kr['current'] > {kr.get('current', 0)}"
"""


def find_applicable_patterns(kr, pattern_library):
    """Find a successful approach from the pattern_library for a similar KR."""
    kr_type = _classify_kr(kr.get("description", ""))
    successes = [
        e for e in pattern_library
        if e.get("kr_type") == kr_type and e.get("outcome") == "success"
    ]
    if not successes:
        return None
    best = max(successes, key=lambda e: e.get("delta", 0))
    return f"[from {best.get('applied_from', '?')}] {best.get('approach_summary', '')}"


def seed_cross_initiative(successful_pattern, all_initiatives, snapshots=None, dry_run=False):
    """Seed a successful pattern to stalled KRs in other initiatives.

    When something works for one initiative, find similar stalled KRs in other
    initiatives and propose experiments that apply the same approach.

    Args:
        successful_pattern: dict with keys like approach, kr_description, delta,
                            initiative_id, kr_id, kr_type, approach_summary.
        all_initiatives: list of initiative dicts (each with id, key_results, etc.)
        snapshots: optional pre-loaded snapshots; loaded from disk if None.
        dry_run: if True, return proposals without writing to patterns.jsonl.

    Returns:
        list of cross-seed entry dicts proposed/written.
    """
    if snapshots is None:
        snapshots = _load_jsonl(SNAPSHOTS_LOG)

    source_init = successful_pattern.get("initiative_id", "")
    source_kr = successful_pattern.get("kr_id", "")
    source_kr_type = successful_pattern.get("kr_type") or _classify_kr(
        successful_pattern.get("kr_description", "")
    )
    approach = (
        successful_pattern.get("approach")
        or successful_pattern.get("approach_summary", "")
    )
    delta = successful_pattern.get("delta", 0)

    seeds = []
    for initiative in all_initiatives:
        init_id = initiative.get("id", "unknown")
        if init_id == source_init:
            continue
        for kr in initiative.get("key_results") or []:
            if kr.get("status") == "met":
                continue
            kr_id = kr.get("id", "kr-?")
            target_kr_type = _classify_kr(kr.get("description", ""))

            # Match on same kr_type (semantic similarity proxy via classification)
            if target_kr_type != source_kr_type:
                continue

            # Check stalled: current=0 or no movement in STALL_WINDOW_DAYS
            current = kr.get("current")
            stalled = False
            if current is not None:
                try:
                    if float(current) == 0:
                        stalled = True
                except (TypeError, ValueError):
                    pass
            if not stalled:
                stalled = is_stalled(kr_id, init_id, snapshots)

            if not stalled:
                continue

            entry = {
                "ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "type": "cross-seed",
                "from_initiative": source_init,
                "from_kr": source_kr,
                "to_initiative": init_id,
                "to_kr": kr_id,
                "kr_type": source_kr_type,
                "source_approach": approach,
                "source_delta": delta,
                "proposed_experiment": (
                    f"Apply approach from {source_init}/{source_kr} "
                    f"(delta={delta}): {approach}"
                ),
                "target_kr_description": kr.get("description", ""),
                "target_current": current,
                "target_target": kr.get("target"),
            }
            seeds.append(entry)

    if not dry_run and seeds:
        os.makedirs(AUDIT_DIR, exist_ok=True)
        with open(PATTERN_LIBRARY, "a") as fh:
            for entry in seeds:
                fh.write(json.dumps(entry) + "\n")

    return seeds


class SelfImprovement:
    """Object-oriented wrapper around self_improvement module functions.

    Provides the same functionality as the module-level functions but
    accessible via an instance for callers that prefer class-based access.
    """

    def __init__(self):
        self._snapshots = None
        self._pattern_library = None

    def _load_snapshots(self):
        if self._snapshots is None:
            self._snapshots = _load_jsonl(SNAPSHOTS_LOG)
        return self._snapshots

    def _load_patterns(self):
        if self._pattern_library is None:
            self._pattern_library = _load_jsonl(PATTERN_LIBRARY)
        return self._pattern_library

    def find_applicable_patterns(self, kr, pattern_library=None):
        """Delegate to module-level find_applicable_patterns."""
        if pattern_library is None:
            pattern_library = self._load_patterns()
        return find_applicable_patterns(kr, pattern_library)

    def seed_cross_initiative(self, successful_pattern, all_initiatives=None,
                              snapshots=None, dry_run=False):
        """Delegate to module-level seed_cross_initiative.

        If all_initiatives is None, loads all active initiatives from disk.
        """
        if all_initiatives is None:
            all_initiatives = _load_all_active_initiatives()
        if snapshots is None:
            snapshots = self._load_snapshots()
        return seed_cross_initiative(
            successful_pattern, all_initiatives,
            snapshots=snapshots, dry_run=dry_run,
        )


def _load_all_active_initiatives():
    """Load all active initiative YAML files from the initiatives directory."""
    try:
        import yaml as _yaml
    except ImportError:
        return []
    initiatives_dir = os.path.join(
        os.environ.get("HEX_ROOT", os.path.expanduser("~/hex")),
        "initiatives",
    )
    results = []
    if not os.path.isdir(initiatives_dir):
        return results
    for fname in sorted(os.listdir(initiatives_dir)):
        if not fname.endswith(".yaml") or fname.endswith(".lock"):
            continue
        path = os.path.join(initiatives_dir, fname)
        try:
            with open(path, "r", encoding="utf-8") as fh:
                data = _yaml.safe_load(fh)
            if data.get("status", "active") == "active":
                results.append(data)
        except Exception:
            continue
    return results


def log_pattern_library_success(initiative, kr, dry_run=False):
    """When a KR moves, log what worked to the pattern_library for cross-initiative reuse."""
    delta = (kr.get("current") or 0) - (kr.get("previous_current") or 0)
    if delta <= 0:
        return
    spec_ids = initiative.get("specs") or []
    latest_spec = spec_ids[-1] if spec_ids else None
    kr_type = _classify_kr(kr.get("description", ""))
    entry = {
        "ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "initiative": initiative.get("id"),
        "initiative_id": initiative.get("id"),
        "kr_id": kr.get("id"),
        "kr_type": kr_type,
        "metric_command": (kr.get("metric") or {}).get("command", ""),
        "outcome": "success",
        "approach": (
            f"Spec {latest_spec} moved KR from {kr.get('previous_current')} to {kr.get('current')}"
            if latest_spec else f"KR moved to {kr.get('current')}"
        ),
        "approach_type": "dispatch_spec",
        "approach_summary": (
            f"Spec {latest_spec} moved KR from {kr.get('previous_current')} to {kr.get('current')}"
            if latest_spec else f"KR moved to {kr.get('current')}"
        ),
        "spec_id": latest_spec,
        "applied_from": f"{initiative.get('id')}/{kr.get('id')}",
        "delta": delta,
    }
    if not dry_run:
        _append_jsonl(PATTERN_LIBRARY, entry)


# ── internal helpers ──────────────────────────────────────────────────────────

def _classify_kr(description):
    """Map KR description to a type for cross-initiative pattern matching."""
    desc = description.lower()
    if any(w in desc for w in ("publish", "post", "content", "piece")):
        return "content_count"
    if any(w in desc for w in ("ratio", "rate", "percentage", "%")):
        return "ratio"
    if any(w in desc for w in ("cover", "instrument", "track", "monitor")):
        return "coverage"
    if any(w in desc for w in ("latency", "speed", "time", "ms")):
        return "performance"
    if any(w in desc for w in ("user", "member", "follower", "subscriber")):
        return "audience"
    return "generic"


def _dispatch_spec(spec_content, init_id, kr_id):
    os.makedirs(os.path.expanduser("~/.boi/queue"), exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".md",
        prefix=f"pivot-{init_id}-{kr_id}-",
        dir=os.path.expanduser("~/.boi/queue"),
        delete=False,
    ) as fh:
        fh.write(spec_content)
        spec_path = fh.name
    tmp_spec = spec_path + ".tmp.md"
    os.rename(spec_path, tmp_spec)
    result = subprocess.run(
        ["boi", "dispatch", "--spec", tmp_spec, "--mode", "execute", "--no-critic"],
        capture_output=True, text=True, timeout=30,
    )
    try:
        os.unlink(tmp_spec)
    except OSError:
        pass
    for token in (result.stdout + result.stderr).split():
        if token.startswith("q-") and token[2:].isdigit():
            return token
    return "q-unknown"


def _append_jsonl(path, entry):
    """Append a single JSON object as a line to a .jsonl file."""
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(entry, default=str) + "\n")


def _append_pivot(init_id, kr_id, category, past_pivots, spec_summary, queue_id, reason, pivot_num):
    os.makedirs(AUDIT_DIR, exist_ok=True)
    ts = datetime.now(timezone.utc).isoformat(timespec="seconds")
    from_exp = past_pivots[-1].get("new_approach") if past_pivots else None
    entry = {
        "ts": ts,
        "initiative": init_id,
        "initiative_id": init_id,  # backward compat
        "kr_id": kr_id,
        "from_exp": from_exp,
        "reason": reason,
        "new_hypothesis": spec_summary,
        "stall_category": category,
        "old_approach": from_exp,
        "why_it_failed": reason,
        "new_approach": queue_id,
        "new_approach_summary": spec_summary,
        "pivot_date": datetime.now(timezone.utc).date().isoformat(),
        "pivot_num": pivot_num,
    }
    _append_jsonl(PIVOTS_LOG, entry)


def _dispatch_redesign_spec(initiative, kr, pivot_history):
    """Generate a structural redesign BOI spec after 3 failed pivots and dispatch it."""
    init_id = initiative.get("id", "unknown")
    kr_id = kr.get("id", "kr-unknown")
    ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")

    # Build the failed-pivots context block
    pivot_lines = []
    for p in pivot_history:
        pivot_lines.append(
            f"  - Pivot {p.get('pivot_num', '?')}: "
            f"spec={p.get('new_approach', 'unknown')} | "
            f"category={p.get('stall_category', '?')} | "
            f"why_failed={p.get('why_it_failed', 'unknown')}"
        )
    pivots_block = "\n".join(pivot_lines) if pivot_lines else "  (no pivot details recorded)"

    metric_cmd = (kr.get("metric") or {}).get("command", "echo 0")
    spec_content = f"""title: "Redesign: {init_id} / {kr_id} — 3 approaches failed"
mode: generate
context: |
  Initiative: {initiative.get('goal', init_id)}
  KR: {kr.get('description', kr_id)}
  Current: {kr.get('current', 0)} / {kr.get('target', '?')} target
  Metric: `{metric_cmd}`

  3 pivot attempts have failed to move this KR. The feedback loop is escalating
  to a structural redesign — not another variation, but a fundamentally different
  approach.

  Failed pivots:
{pivots_block}

tasks:
  - id: t-1
    title: Analyze the 3 failed approaches
    status: PENDING
    spec: |
      Review the 3 failed pivot attempts listed above.
      What do they have in common?
      What structural assumption are they all making that might be wrong?
      Write findings to a short analysis section.
    verify: "true"

  - id: t-2
    title: Propose a fundamentally different approach
    status: PENDING
    depends: [t-1]
    spec: |
      Based on the t-1 analysis, propose a fundamentally different approach.
      This must NOT be a variation of the previous attempts — it must challenge
      the structural assumption identified in t-1.
      Describe the new approach, why it avoids the shared failure mode, and
      what metric movement it targets.
    verify: "true"

  - id: t-3
    title: Implement the smallest version of the new approach
    status: PENDING
    depends: [t-2]
    spec: |
      Implement the smallest possible version of the approach from t-2.
      Target: move the metric `{metric_cmd}` from {kr.get('current', 0)}
      toward {kr.get('target', '?')}.
      After implementation, run the metric command and record the result.
      Update initiatives/{init_id}.yaml with the new measurement.
    verify: "bash -c 'val=$({metric_cmd}); [ \\"$val\\" != \\"{kr.get('current', 0)}\\" ]'"
"""

    # Write spec to specs/ directory
    hex_root = os.environ.get("HEX_ROOT", os.path.expanduser("~/hex"))
    specs_dir = os.path.join(hex_root, "specs")
    os.makedirs(specs_dir, exist_ok=True)
    spec_filename = f"redesign-{init_id}-{kr_id}-{ts}.yaml"
    spec_path = os.path.join(specs_dir, spec_filename)
    with open(spec_path, "w") as fh:
        fh.write(spec_content)

    # Dispatch via BOI
    boi_path = os.path.expanduser("~/.boi/boi")
    try:
        result = subprocess.run(
            ["bash", boi_path, "dispatch", spec_path],
            capture_output=True, text=True, timeout=30,
        )
        for token in (result.stdout + result.stderr).split():
            if token.startswith("q-") and token[2:].isdigit():
                return token
    except (FileNotFoundError, subprocess.TimeoutExpired, OSError):
        pass
    return f"q-redesign-{init_id}-{kr_id}"


def _escalate_kr(initiative, kr, pivot_history, dry_run):
    init_id = initiative.get("id", "unknown")
    kr_id = kr.get("id", "kr-unknown")
    msg = (
        f"ESCALATION: {init_id}/{kr_id} — "
        f"{len(pivot_history)} pivots, zero movement.\n"
        + "\n".join(
            f"  Pivot {p.get('pivot_num')}: {p.get('new_approach')} — {p.get('why_it_failed', '')}"
            for p in pivot_history
        )
    )
    spec_queue_id = None
    if not dry_run:
        try:
            subprocess.run(
                ["hex-notify", "--channel", "#from-hex", "--message", msg],
                capture_output=True, timeout=10,
            )
        except (FileNotFoundError, subprocess.TimeoutExpired):
            pass
        last_exp = pivot_history[-1].get("new_approach") if pivot_history else None
        _append_jsonl(PIVOTS_LOG, {
            "ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "initiative": init_id,
            "initiative_id": init_id,  # backward compat
            "kr_id": kr_id,
            "from_exp": last_exp,
            "reason": f"{len(pivot_history)} pivots with no movement — escalating",
            "new_hypothesis": "Structural redesign required",
            "escalated": True,
            "pivot_num": len(pivot_history),
            "message": msg,
        })

        # Generate and dispatch a structural redesign spec
        spec_queue_id = _dispatch_redesign_spec(initiative, kr, pivot_history)

    return {
        "action": "escalate_kr",
        "initiative_id": init_id,
        "kr_id": kr_id,
        "pivot_count": len(pivot_history),
        "redesign_spec": spec_queue_id,
        "dry_run": dry_run,
    }


def _maybe_log_success(initiative, kr, pattern_library, dry_run):
    """Log approach to pattern_library when a KR is moving."""
    delta = (kr.get("current") or 0) - (kr.get("previous_current") or 0)
    if delta <= 0:
        return
    log_pattern_library_success(initiative, kr, dry_run=dry_run)


def _load_jsonl(path):
    if not os.path.exists(path):
        return []
    entries = []
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                entries.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    return entries


def _age_hours(dt_str):
    if not dt_str:
        return float("inf")
    s = str(dt_str).replace("Z", "+00:00")
    try:
        dt = datetime.fromisoformat(s)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return (datetime.now(timezone.utc) - dt).total_seconds() / 3600
    except (ValueError, TypeError):
        return float("inf")
