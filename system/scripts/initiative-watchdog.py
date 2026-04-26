#!/usr/bin/env python3
"""initiative-watchdog.py — standalone watchdog for the autonomous initiative execution system.

Usage:
  initiative-watchdog.py [--dry-run] [--full] [--help]

Flags:
  --dry-run   Run all checks but write nothing to disk. Output JSON to stdout.
  --full      Run all checks including experiment progress (same as 6h + 12h checks).

Checks:
  1. Velocity check — did any KR move in the last 24h?
  2. Loop execution check — did initiative loop run and produce actions?
  3. Experiment progress check — are experiments stuck in DRAFT/ACTIVE?
  4. Spec completion check — did dispatched specs complete?
  5. Watchdog self-check — write heartbeat so the watchdog's watchdog can detect failure.

Each run appends a JSON record to ~/.hex/audit/initiative-watchdog.jsonl.
Heartbeat appended to ~/.hex/audit/watchdog-heartbeat.jsonl.
KR snapshots stored in ~/.hex/audit/kr-snapshots.jsonl.

Output format (stdout when --dry-run):
  {
    "ts": "...",
    "velocity_status": "ok|stalled|partial_stall",
    "loop_status": "ok|no_runs|passive",
    "experiment_status": "ok|stuck|warning",
    "spec_status": "ok|failures|wrong_target",
    "alerts_emitted": [...],
    "summary": "..."
  }
"""

import argparse
import glob
import json
import os
import sys
from datetime import datetime, timedelta, timezone

try:
    import yaml
    _YAML_AVAILABLE = True
except ImportError:
    _YAML_AVAILABLE = False

HEX_ROOT = os.environ.get("HEX_ROOT", os.path.expanduser("~/hex"))
HOME_HEX = os.path.expanduser("~/.hex")
AUDIT_DIR = os.path.join(HOME_HEX, "audit")
INITIATIVES_DIR = os.path.join(HEX_ROOT, "initiatives")
EXPERIMENTS_DIR = os.path.join(HEX_ROOT, "experiments")

WATCHDOG_LOG = os.path.join(AUDIT_DIR, "initiative-watchdog.jsonl")
HEARTBEAT_LOG = os.path.join(AUDIT_DIR, "watchdog-heartbeat.jsonl")
KR_SNAPSHOTS = os.path.join(AUDIT_DIR, "kr-snapshots.jsonl")
LOOP_HISTORY = os.path.join(AUDIT_DIR, "initiative-loop-history.jsonl")

BOI_QUEUE_DB = os.path.expanduser("~/.boi/push_queue.db")


def _now():
    return datetime.now(timezone.utc)


def _parse_ts(ts_str):
    if not ts_str:
        return None
    for fmt in ("%Y-%m-%dT%H:%M:%S%z", "%Y-%m-%dT%H:%M:%S.%f%z", "%Y-%m-%dT%H:%M:%SZ"):
        try:
            dt = datetime.strptime(ts_str.replace("+00:00", "Z").replace("Z", "+00:00"), "%Y-%m-%dT%H:%M:%S%z")
            return dt
        except ValueError:
            pass
    # Try ISO with colon-timezone
    try:
        import re
        ts_clean = re.sub(r'(\d{2}):(\d{2})$', r'\1\2', ts_str)
        return datetime.strptime(ts_clean, "%Y-%m-%dT%H:%M:%S%z")
    except Exception:
        return None


def _load_jsonl(path):
    records = []
    if not os.path.exists(path):
        return records
    with open(path, "r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                try:
                    records.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    return records


def _append_jsonl(path, record, dry_run=False):
    if dry_run:
        return
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        fh.write(json.dumps(record) + "\n")
    # Append mode: read existing + append
    existing = ""
    if os.path.exists(path):
        with open(path, "r", encoding="utf-8") as fh:
            existing = fh.read()
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(existing)
        fh.write(json.dumps(record) + "\n")
    if os.path.exists(tmp):
        os.remove(tmp)


def _load_initiatives():
    """Load all initiative YAML files. Returns list of dicts."""
    initiatives = []
    if not _YAML_AVAILABLE:
        return initiatives
    for path in glob.glob(os.path.join(INITIATIVES_DIR, "*.yaml")):
        try:
            with open(path, "r", encoding="utf-8") as fh:
                data = yaml.safe_load(fh)
            if data:
                initiatives.append(data)
        except Exception:
            pass
    return initiatives


def _load_experiments():
    """Load all experiment YAML files. Returns list of dicts."""
    experiments = []
    if not _YAML_AVAILABLE:
        return experiments
    for path in glob.glob(os.path.join(EXPERIMENTS_DIR, "*.yaml")):
        try:
            with open(path, "r", encoding="utf-8") as fh:
                data = yaml.safe_load(fh)
            if data:
                experiments.append(data)
        except Exception:
            pass
    return experiments


def _current_kr_snapshot(initiatives):
    """Extract {kr_id: current_value} from loaded initiatives."""
    snapshot = {}
    for init_data in initiatives:
        init_id = init_data.get("id", "unknown")
        for kr in init_data.get("key_results", []):
            kr_id = f"{init_id}/{kr.get('id', 'unknown')}"
            current = kr.get("current")
            snapshot[kr_id] = current
    return snapshot


# ── Check 1: Velocity ──────────────────────────────────────────────────────────

def check_velocity(initiatives, dry_run=False):
    """Compare current KR values to 24h-ago snapshot. Store new snapshot."""
    now = _now()
    cutoff_24h = now - timedelta(hours=24)
    cutoff_72h = now - timedelta(hours=72)

    current_snapshot = _current_kr_snapshot(initiatives)

    # Load historical snapshots
    historical = _load_jsonl(KR_SNAPSHOTS)

    # Find snapshot from ~24h ago (newest one older than 24h)
    snapshot_24h = None
    snapshot_72h = None
    for rec in reversed(historical):
        ts = _parse_ts(rec.get("ts"))
        if ts and ts < cutoff_24h and snapshot_24h is None:
            snapshot_24h = rec.get("snapshot", {})
        if ts and ts < cutoff_72h and snapshot_72h is None:
            snapshot_72h = rec.get("snapshot", {})

    alerts = []
    stalled_initiatives = []

    # Save current snapshot
    snap_record = {"ts": now.isoformat(), "snapshot": current_snapshot}
    _append_jsonl(KR_SNAPSHOTS, snap_record, dry_run=dry_run)

    total_krs = len(current_snapshot)
    moved_24h = 0

    if snapshot_24h:
        for kr_id, current_val in current_snapshot.items():
            old_val = snapshot_24h.get(kr_id)
            if old_val is None or current_val is None:
                continue
            if current_val != old_val:
                moved_24h += 1

        if moved_24h == 0 and total_krs > 0:
            msg = (
                f"SYSTEM STALL: No KR moved in 24h across {len(initiatives)} initiatives. "
                f"Total KRs tracked: {total_krs}. Last check: {now.isoformat()}"
            )
            alerts.append({"type": "hex.initiatives.stalled", "message": msg})
            status = "stalled"
        else:
            status = "ok"
    else:
        # No 24h snapshot yet — this is the first run, establish baseline
        status = "ok"
        moved_24h = -1  # sentinel: no baseline yet

    # Check per-initiative 72h stall
    if snapshot_72h:
        # Group KRs by initiative
        init_krs = {}
        for kr_id, current_val in current_snapshot.items():
            init_id = kr_id.split("/")[0]
            init_krs.setdefault(init_id, []).append((kr_id, current_val))

        for init_id, krs in init_krs.items():
            any_moved = False
            for kr_id, current_val in krs:
                old_val = snapshot_72h.get(kr_id)
                if old_val is None or current_val is None:
                    continue
                if current_val != old_val:
                    any_moved = True
                    break
            if not any_moved:
                stalled_initiatives.append(init_id)
                alerts.append({
                    "type": "hex.initiative.stalled",
                    "initiative": init_id,
                    "message": f"Initiative {init_id}: no KR moved in 72h.",
                })

    # Refine status
    if status == "ok" and stalled_initiatives:
        status = "partial_stall"

    return {
        "status": status,
        "total_krs": total_krs,
        "moved_24h": moved_24h,
        "stalled_initiatives_72h": stalled_initiatives,
        "alerts": alerts,
    }


# ── Check 2: Loop execution ────────────────────────────────────────────────────

def check_loop_execution():
    """Check that initiative-loop ran and produced dispatch/activate actions."""
    now = _now()
    cutoff_6h = now - timedelta(hours=6)
    cutoff_12h = now - timedelta(hours=12)

    history = _load_jsonl(LOOP_HISTORY)

    alerts = []
    # Map agent -> list of runs in last 12h
    agent_runs = {}
    for rec in history:
        ts = _parse_ts(rec.get("ts"))
        if ts and ts > cutoff_12h:
            agent = rec.get("agent", "unknown")
            agent_runs.setdefault(agent, []).append(rec)

    # Identify agents that own initiatives
    agents_with_initiatives = set()
    initiatives = _load_initiatives()
    for init_data in initiatives:
        owner = init_data.get("owner")
        if owner:
            agents_with_initiatives.add(owner)

    no_run_agents = []
    passive_agents = []

    for agent in agents_with_initiatives:
        runs = agent_runs.get(agent, [])
        if not runs:
            no_run_agents.append(agent)
            alerts.append({
                "type": "hex.initiative.agent.stalled",
                "agent": agent,
                "message": f"Agent {agent} hasn't run its initiative loop in 12h.",
            })
            continue

        # Check if last 3 runs were all passive (no dispatch/activate/measure)
        if len(runs) >= 3:
            last_3 = runs[-3:]
            active_runs = 0
            for run in last_3:
                actions = run.get("actions", [])
                has_action = any(
                    a.get("action") in ("dispatch_spec", "activate_experiment", "baseline_experiment", "run_verdict", "fix_metric")
                    for a in actions
                )
                if has_action:
                    active_runs += 1
            if active_runs == 0:
                passive_agents.append(agent)
                alerts.append({
                    "type": "hex.initiative.agent.stalled",
                    "agent": agent,
                    "message": f"Agent {agent} ran loop 3x with zero dispatch/activate/measure actions.",
                })

    if no_run_agents or passive_agents:
        status = "passive" if passive_agents else "no_runs"
        if no_run_agents and passive_agents:
            status = "no_runs"
    else:
        status = "ok"

    return {
        "status": status,
        "agents_with_initiatives": sorted(agents_with_initiatives),
        "agents_with_recent_runs": sorted(agent_runs.keys()),
        "no_run_agents": no_run_agents,
        "passive_agents": passive_agents,
        "alerts": alerts,
    }


# ── Check 3: Experiment progress ───────────────────────────────────────────────

def check_experiment_progress():
    """Count experiments by state and flag stuck ones."""
    now = _now()
    cutoff_7d = now - timedelta(days=7)
    cutoff_48h = now - timedelta(hours=48)

    experiments = _load_experiments()

    by_state = {}
    stuck_active = []
    stuck_draft = []
    alerts = []

    for exp in experiments:
        state = exp.get("state", "UNKNOWN")
        by_state[state] = by_state.get(state, 0) + 1

        created_str = str(exp.get("created", ""))
        created_dt = None
        try:
            from datetime import date as dateclass
            if created_str:
                created_dt = datetime.fromisoformat(created_str.replace("Z", "+00:00"))
                if created_dt.tzinfo is None:
                    created_dt = created_dt.replace(tzinfo=timezone.utc)
        except Exception:
            pass

        # ACTIVE for >7 days with no measurement
        if state == "ACTIVE":
            last_measured = exp.get("last_measured_at") or exp.get("time_bound", {}).get("last_measured_at")
            if not last_measured and created_dt and created_dt < cutoff_7d:
                stuck_active.append(exp.get("id", "?"))

        # DRAFT for >48h with no baseline
        if state == "DRAFT":
            if created_dt and created_dt < cutoff_48h:
                stuck_draft.append(exp.get("id", "?"))

    total = len(experiments)
    stuck_count = len(stuck_active) + len(stuck_draft)

    if total > 0 and stuck_count / total > 0.5:
        status = "stuck"
        alerts.append({
            "type": "hex.experiments.stuck",
            "message": f"{stuck_count}/{total} experiments are stuck (>50% threshold).",
            "stuck_active": stuck_active,
            "stuck_draft": stuck_draft,
        })
    elif stuck_active or stuck_draft:
        status = "warning"
    else:
        status = "ok"

    return {
        "status": status,
        "total": total,
        "by_state": by_state,
        "stuck_active_7d": stuck_active,
        "stuck_draft_48h": stuck_draft,
        "alerts": alerts,
    }


# ── Check 4: Spec completion ───────────────────────────────────────────────────

def check_spec_completion():
    """Check BOI spec completion for initiative-linked specs."""
    now = _now()
    cutoff_7d = now - timedelta(days=7)

    alerts = []
    status = "ok"
    dispatched = 0
    failed = 0
    completed = 0
    wrong_target = []

    # Read from loop history: actions that dispatched specs
    history = _load_jsonl(LOOP_HISTORY)
    dispatched_specs = []
    for rec in history:
        ts = _parse_ts(rec.get("ts"))
        if ts and ts > cutoff_7d:
            for action in rec.get("actions", []):
                if action.get("action") == "dispatch_spec":
                    spec_id = action.get("spec_id") or action.get("queue_id")
                    kr_id = action.get("kr_id")
                    if spec_id:
                        dispatched_specs.append({
                            "spec_id": spec_id,
                            "kr_id": kr_id,
                            "dispatched_at": rec.get("ts"),
                            "agent": rec.get("agent"),
                        })

    dispatched = len(dispatched_specs)

    # Try to check BOI status via sqlite3
    if dispatched > 0 and os.path.exists(BOI_QUEUE_DB):
        try:
            import sqlite3
            conn = sqlite3.connect(BOI_QUEUE_DB)
            conn.row_factory = sqlite3.Row
            cur = conn.cursor()
            for spec_info in dispatched_specs:
                spec_id = spec_info["spec_id"]
                try:
                    cur.execute(
                        "SELECT status FROM specs WHERE id = ? OR queue_id = ?",
                        (spec_id, spec_id)
                    )
                    row = cur.fetchone()
                    if row:
                        spec_status = row["status"]
                        if spec_status == "failed":
                            failed += 1
                        elif spec_status in ("completed", "done"):
                            completed += 1
                except Exception:
                    pass
            conn.close()
        except Exception:
            pass

    if dispatched > 0 and failed / max(dispatched, 1) > 0.3:
        status = "failures"
        alerts.append({
            "type": "hex.specs.high_failure_rate",
            "message": f"{failed}/{dispatched} initiative-linked specs failed (>{30}% threshold).",
        })

    return {
        "status": status,
        "dispatched_7d": dispatched,
        "completed": completed,
        "failed": failed,
        "wrong_target": wrong_target,
        "alerts": alerts,
    }


# ── Check 5: Heartbeat ─────────────────────────────────────────────────────────

def write_heartbeat(dry_run=False):
    now = _now()
    record = {"ts": now.isoformat(), "status": "alive"}
    _append_jsonl(HEARTBEAT_LOG, record, dry_run=dry_run)


# ── Main ───────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="initiative-watchdog — autonomous initiative health monitor")
    parser.add_argument("--dry-run", action="store_true", help="Run checks but write nothing to disk; output JSON to stdout")
    parser.add_argument("--full", action="store_true", help="Run all checks (default: all checks anyway)")
    args = parser.parse_args()

    if not _YAML_AVAILABLE:
        result = {
            "ts": _now().isoformat(),
            "velocity_status": "error",
            "loop_status": "error",
            "experiment_status": "error",
            "spec_status": "error",
            "alerts_emitted": [{"type": "error", "message": "PyYAML not available"}],
            "summary": "ERROR: PyYAML not installed. Run: pip install pyyaml",
        }
        print(json.dumps(result))
        sys.exit(1)

    now = _now()
    all_alerts = []

    # Load initiatives once
    initiatives = _load_initiatives()

    # Run checks
    velocity = check_velocity(initiatives, dry_run=args.dry_run)
    loop_exec = check_loop_execution()
    experiment = check_experiment_progress()
    spec_comp = check_spec_completion()

    all_alerts.extend(velocity.get("alerts", []))
    all_alerts.extend(loop_exec.get("alerts", []))
    all_alerts.extend(experiment.get("alerts", []))
    all_alerts.extend(spec_comp.get("alerts", []))

    # Emit events for alerts (in non-dry-run mode)
    if not args.dry_run:
        _emit_alerts(all_alerts)

    # Build summary
    issues = [a["type"] for a in all_alerts]
    if not issues:
        summary = "All checks passed. System is healthy."
    else:
        summary = f"{len(issues)} alert(s): {', '.join(set(issues))}"

    result = {
        "ts": now.isoformat(),
        "velocity_status": velocity["status"],
        "loop_status": loop_exec["status"],
        "experiment_status": experiment["status"],
        "spec_status": spec_comp["status"],
        "alerts_emitted": all_alerts,
        "summary": summary,
        "detail": {
            "velocity": velocity,
            "loop_execution": loop_exec,
            "experiment_progress": experiment,
            "spec_completion": spec_comp,
        },
    }

    # Write to audit log
    _append_jsonl(WATCHDOG_LOG, result, dry_run=args.dry_run)

    # Write heartbeat
    write_heartbeat(dry_run=args.dry_run)

    # Always print result to stdout
    print(json.dumps(result))


def _emit_alerts(alerts):
    """Emit hex-events for each alert (best-effort)."""
    telemetry_path = os.path.join(HEX_ROOT, ".hex", "telemetry")
    sys.path.insert(0, telemetry_path)
    for alert in alerts:
        try:
            from emit import emit
            emit(alert["type"], alert, source="initiative-watchdog")
        except Exception:
            pass


if __name__ == "__main__":
    main()
