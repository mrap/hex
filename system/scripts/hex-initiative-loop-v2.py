#!/usr/bin/env python3
"""hex-initiative-loop-v2 — execution engine initiative loop for any agent.

Usage:
  hex-initiative-loop-v2.py --agent <id> [--dry-run] [--initiative <id>]

Runs the 8-step initiative execution loop for all initiatives owned by the agent.
Unlike v1, every step COMPLETES its action — no proposals that wait for someone else.

  1. Measure KRs — run metric commands, reload data.
  2. Verdict — run verdict on ACTIVE/MEASURING experiments >= 48h old.
     PASS: activate/adopt. FAIL: log failure, dispatch new-approach spec.
  3. Activate — transition BASELINE experiments to ACTIVE immediately.
  4. Baseline — baseline DRAFT experiments >= 1h old immediately.
  5. Dispatch — for each KR at current=0 with no ACTIVE experiment:
     write a targeted BOI spec and dispatch it NOW.
  6. Fix broken metrics — if a metric command fails or returns None:
     dispatch a fix spec for the broken metric.
  7. Escalate budget — if dispatch fails due to budget, emit hex.budget.escalation.
  8. Self-assess — every 5 runs, check if any KR moved. If not, dispatch a
     pivot spec that tries a different approach.

Outputs a JSON summary of all actions. Use --dry-run to preview without side effects.
In dry-run, dispatch_spec actions appear in output so the caller can verify the loop
is not passive.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
from datetime import datetime, date, timezone

try:
    import yaml
except ImportError:
    print("ERROR: PyYAML not installed. Run: pip install pyyaml", file=sys.stderr)
    sys.exit(1)

HEX_ROOT = os.environ.get("HEX_ROOT", os.path.expanduser("~/hex"))
INITIATIVES_DIR = os.path.join(HEX_ROOT, "initiatives")
EXPERIMENTS_DIR = os.path.join(HEX_ROOT, "experiments")
SCRIPTS_DIR = os.path.join(HEX_ROOT, ".hex", "scripts")
TELEMETRY_PATH = os.path.join(HEX_ROOT, ".hex", "telemetry")
AUDIT_DIR = os.path.expanduser("~/.hex/audit")
LOOP_HISTORY = os.path.join(AUDIT_DIR, "initiative-loop-history.jsonl")


# ── telemetry ─────────────────────────────────────────────────────────────────

def _emit(event_type, payload, dry_run=False):
    if dry_run:
        return
    sys.path.insert(0, TELEMETRY_PATH)
    try:
        from emit import emit
        emit(event_type, payload, source="hex-initiative-loop-v2")
    except Exception as exc:
        print(f"[initiative-loop-v2] telemetry warn: {exc}", file=sys.stderr)


# ── YAML I/O ──────────────────────────────────────────────────────────────────

def _load_yaml(path):
    with open(path, "r", encoding="utf-8") as fh:
        return yaml.safe_load(fh)

def _save_yaml(data, path):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        yaml.dump(data, fh, default_flow_style=False, allow_unicode=True, sort_keys=False)
    os.replace(tmp, path)


# ── time helpers ──────────────────────────────────────────────────────────────

def _now():
    return datetime.now(timezone.utc)

def _now_iso():
    return _now().isoformat(timespec="seconds")

def _age_hours(dt_str):
    if not dt_str:
        return 0.0
    s = str(dt_str).strip()
    if len(s) == 10 and "T" not in s:
        s = s + "T00:00:00+00:00"
    s = s.replace("Z", "+00:00")
    try:
        dt = datetime.fromisoformat(s)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return (_now() - dt).total_seconds() / 3600
    except (ValueError, TypeError):
        return 0.0

def _days_until(date_str):
    try:
        target = datetime.strptime(str(date_str), "%Y-%m-%d").date()
        return (target - date.today()).days
    except (ValueError, TypeError):
        return 9999


# ── subprocess helper ─────────────────────────────────────────────────────────

def _run(args, dry_run, timeout=120):
    """Execute a command. Returns (success, output_str)."""
    if dry_run:
        return True, "[dry-run skipped]"
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
        ok = result.returncode == 0
        out = (result.stdout.strip() or result.stderr.strip())[:500]
        return ok, out
    except Exception as exc:
        return False, str(exc)[:300]

def _run_metric_command(cmd_str, dry_run):
    """Run a metric command string via shell. Returns (success, value_or_None, raw_out)."""
    if dry_run:
        return True, 0.0, "[dry-run]"
    try:
        result = subprocess.run(
            cmd_str, shell=True, capture_output=True, text=True, timeout=60
        )
        raw = result.stdout.strip()
        if not raw or result.returncode != 0:
            return False, None, (result.stderr.strip() or raw)[:200]
        try:
            val = float(raw.split("\n")[-1].strip())
            return True, val, raw[:200]
        except (ValueError, IndexError):
            return False, None, raw[:200]
    except Exception as exc:
        return False, None, str(exc)[:200]


# ── initiative / experiment loaders ──────────────────────────────────────────

def _load_initiatives_for_agent(agent_id, filter_id=None):
    results = []
    if not os.path.isdir(INITIATIVES_DIR):
        return results
    for fname in sorted(os.listdir(INITIATIVES_DIR)):
        if not fname.endswith(".yaml") or fname.endswith(".lock"):
            continue
        path = os.path.join(INITIATIVES_DIR, fname)
        try:
            data = _load_yaml(path)
        except Exception:
            continue
        if data.get("owner") != agent_id:
            continue
        if data.get("status", "active") != "active":
            continue
        if filter_id and data.get("id") != filter_id:
            continue
        results.append((path, data))
    return results

def _load_all_experiments():
    lookup = {}
    if not os.path.isdir(EXPERIMENTS_DIR):
        return lookup
    for fname in sorted(os.listdir(EXPERIMENTS_DIR)):
        if not fname.startswith("exp-") or not fname.endswith(".yaml") or fname.endswith(".lock"):
            continue
        path = os.path.join(EXPERIMENTS_DIR, fname)
        try:
            data = _load_yaml(path)
            exp_id = data.get("id")
            if exp_id:
                lookup[exp_id] = (path, data)
        except Exception:
            pass
    return lookup

def _next_exp_id():
    os.makedirs(EXPERIMENTS_DIR, exist_ok=True)
    nums = []
    for name in os.listdir(EXPERIMENTS_DIR):
        if name.startswith("exp-") and name.endswith(".yaml") and not name.endswith(".lock"):
            try:
                nums.append(int(name[4:7]))
            except ValueError:
                pass
    return f"exp-{(max(nums) + 1 if nums else 1):03d}"

def _slugify(title):
    slug = title.lower()
    slug = "".join(c if c.isalnum() else "-" for c in slug)
    slug = "-".join(p for p in slug.split("-") if p)
    return slug[:40]

def _exp_id_str(exp_ref):
    """Normalize an experiment reference to a string ID.
    Initiatives may store experiments as plain string IDs or as dicts with an 'id' key."""
    if isinstance(exp_ref, dict):
        return exp_ref.get("id")
    return exp_ref


# ── BOI spec dispatch ─────────────────────────────────────────────────────────

def _write_and_dispatch_spec(spec_content, label, dry_run):
    """Write spec to a temp file and dispatch via `boi dispatch`. Returns (success, queue_id, output)."""
    if dry_run:
        return True, "[dry-run-q-XXX]", "[dry-run skipped]"

    os.makedirs(AUDIT_DIR, exist_ok=True)
    fd, tmp_path = tempfile.mkstemp(suffix=".yaml", prefix="initiative-loop-", dir=AUDIT_DIR)
    try:
        with os.fdopen(fd, "w") as fh:
            fh.write(spec_content)
        result = subprocess.run(
            ["boi", "dispatch", "--spec", tmp_path, "--mode", "execute", "--no-critic"],
            capture_output=True, text=True, timeout=30
        )
        out = (result.stdout.strip() + result.stderr.strip())[:500]
        ok = result.returncode == 0
        queue_id = None
        for token in out.split():
            if token.startswith("q-") and token[2:].isdigit():
                queue_id = token
                break
        # Budget detection
        if not ok and ("budget" in out.lower() or "cost" in out.lower()):
            return False, None, "BUDGET_EXHAUSTED: " + out
        return ok, queue_id, out
    finally:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass


def _build_kr_fix_spec(init_id, kr, initiative_data):
    """Build a minimal BOI spec to fix a broken metric command for a KR."""
    kr_id = kr.get("id", "kr-?")
    kr_desc = kr.get("description", "")
    metric = kr.get("metric") or {}
    broken_cmd = metric.get("command", "")
    init_file = os.path.basename(initiative_data.get("_path", init_id + ".yaml"))

    spec_data = {
        "title": f"Fix Broken Metric: {init_id} / {kr_id}",
        "mode": "execute",
        "initiative": init_id,
        "context": (
            f"The metric command for {kr_id} in initiative {init_id} is broken or returns None.\n"
            f"KR description: {kr_desc}"
        ),
        "tasks": [{
            "id": "t-1",
            "title": f"Fix broken metric command for {kr_id}",
            "status": "PENDING",
            "spec": (
                f"The metric command for {kr_id} in initiative {init_id} is broken or returns None.\n\n"
                f"KR description: {kr_desc}\n\n"
                f"Current (broken) metric command:\n{broken_cmd}\n\n"
                f"Fix the metric command so it returns a valid numeric value. The command must:\n"
                f"1. Run successfully (exit code 0)\n"
                f"2. Print a single number on stdout\n"
                f"3. Reflect the actual current state of: {kr_desc}\n\n"
                f"Update the metric.command field in initiatives/{init_file}."
            ),
            "verify": (
                "bash -c 'NEW_CMD' | python3 -c "
                "\"import sys; float(sys.stdin.read().strip().split()[-1])\" && echo PASS"
            ),
        }],
    }
    return yaml.dump(spec_data, default_flow_style=False, allow_unicode=True, sort_keys=False)


def _build_kr_dispatch_spec(init_id, kr, initiative_data):
    """Build a minimal BOI spec to drive a KR from 0 to >0."""
    kr_id = kr.get("id", "kr-?")
    kr_desc = kr.get("description", "")
    target = kr.get("target", "N/A")
    metric = kr.get("metric") or {}
    metric_cmd = metric.get("command", "")
    direction = metric.get("direction", "lower_is_better")
    horizon = initiative_data.get("horizon", "2026-12-31")
    owner = initiative_data.get("owner", "unknown")

    spec_data = {
        "title": f"Drive KR to Non-Zero: {init_id} / {kr_id}",
        "mode": "execute",
        "initiative": init_id,
        "context": (
            f"KR {kr_id} in initiative {init_id} is at current=0.\n"
            f"Description: {kr_desc}\n"
            f"Target: {target} ({direction})\n"
            f"Horizon: {horizon}\n"
            f"Owner: {owner}"
        ),
        "tasks": [{
            "id": "t-1",
            "title": f"Identify and execute the highest-leverage action for {kr_id}",
            "status": "PENDING",
            "spec": (
                f"KR {kr_id} in initiative {init_id} is at current=0.\n\n"
                f"- Description: {kr_desc}\n"
                f"- Target: {target} ({direction})\n"
                f"- Horizon: {horizon}\n"
                f"- Owner agent: {owner}\n\n"
                f"Metric command:\n{metric_cmd}\n\n"
                f"Your job: Take the single highest-leverage action that moves this metric from 0 to non-zero.\n\n"
                f"Do NOT just analyze or propose — act. Examples:\n"
                f"- If the metric command exists but data isn't being collected: fix the data collection.\n"
                f"- If the feature/behavior being measured doesn't exist yet: implement the smallest version that works.\n"
                f"- If a process needs to run first: run it.\n"
                f"- If configuration is missing: add it.\n\n"
                f"Verify the metric moves: run the metric command before and after your change and confirm current > 0."
            ),
            "verify": (
                f"bash -c '{metric_cmd}' | python3 -c "
                f"\"import sys; v=float(sys.stdin.read().strip().split()[-1]); "
                f"assert v > 0, f'KR still at {{v}}'; print(f'KR moved to {{v}}')\""
            ),
        }],
    }
    return yaml.dump(spec_data, default_flow_style=False, allow_unicode=True, sort_keys=False)


def _build_pivot_spec(init_id, stalled_krs, initiative_data, run_count):
    """Build a spec to try a different approach when KRs haven't moved in 5 runs."""
    kr_list = ", ".join(k.get("id", "?") for k in stalled_krs)
    kr_details = "\n".join(
        f"- {k.get('id')}: {k.get('description', '')} (current={k.get('current')}, target={k.get('target')})"
        for k in stalled_krs
    )

    spec_data = {
        "title": f"Initiative Pivot: {init_id} — {run_count} Runs, No KR Movement",
        "mode": "execute",
        "initiative": init_id,
        "context": (
            f"Initiative {init_id} has run the initiative loop {run_count} times but no KR has moved.\n"
            f"Stalled KRs: {kr_list}"
        ),
        "tasks": [{
            "id": "t-1",
            "title": f"Diagnose why {kr_list} haven't moved after {run_count} loop runs",
            "status": "PENDING",
            "spec": (
                f"Initiative {init_id} has run the initiative loop {run_count} times but no KR has moved.\n\n"
                f"Stalled KRs:\n{kr_details}\n\n"
                f"Diagnose WHY the current approach isn't working. Read:\n"
                f"- The initiative file: initiatives/*.yaml for {init_id}\n"
                f"- Any linked experiments in experiments/\n"
                f"- Recent BOI specs linked to this initiative\n\n"
                f"Then take a DIFFERENT approach. Not the same thing again. If we tried experiments, try direct "
                f"implementation. If we tried implementation, try fixing measurement. If measurement is broken, "
                f"fix the infrastructure.\n\n"
                f"Write a concrete action plan AND execute the first step."
            ),
            "verify": (
                "bash -c 'METRIC_CMD' | python3 -c "
                "\"import sys; v=float(sys.stdin.read().strip().split()[-1]); "
                "assert v > 0, f'KR still at {v}'; print(f'KR moved to {v}')\""
            ),
        }],
    }
    return yaml.dump(spec_data, default_flow_style=False, allow_unicode=True, sort_keys=False)


# ── run history for self-assess ───────────────────────────────────────────────

def _record_run(agent_id, kr_snapshot):
    """Append a run snapshot to LOOP_HISTORY."""
    os.makedirs(AUDIT_DIR, exist_ok=True)
    entry = {
        "ts": _now_iso(),
        "agent": agent_id,
        "kr_snapshot": kr_snapshot,
    }
    with open(LOOP_HISTORY, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(entry) + "\n")

def _load_recent_runs(agent_id, count=5):
    """Load last N run entries for this agent."""
    if not os.path.exists(LOOP_HISTORY):
        return []
    entries = []
    try:
        with open(LOOP_HISTORY, "r", encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    e = json.loads(line)
                    if e.get("agent") == agent_id:
                        entries.append(e)
                except (json.JSONDecodeError, KeyError):
                    pass
    except OSError:
        pass
    return entries[-count:]


ACTIVE_EXP_STATES = {"DRAFT", "BASELINE", "ACTIVE", "MEASURING"}


# ── main loop ─────────────────────────────────────────────────────────────────

def run_loop(agent_id, dry_run=False, filter_initiative=None):
    summary = {
        "agent": agent_id,
        "timestamp": _now_iso(),
        "dry_run": dry_run,
        "version": "v2",
        "initiatives_checked": 0,
        "actions": [],
    }

    initiatives = _load_initiatives_for_agent(agent_id, filter_id=filter_initiative)
    if not initiatives:
        summary["note"] = f"No active initiatives found for agent '{agent_id}'"
        return summary

    exp_lookup = _load_all_experiments()
    budget_exhausted = False

    # Snapshot KR values before the run (for self-assess and history)
    kr_snapshot_before = {}

    for _, init_data in initiatives:
        init_id = init_data.get("id", "unknown")
        for kr in (init_data.get("key_results") or []):
            kr_snapshot_before[f"{init_id}/{kr.get('id')}"] = kr.get("current")

    for init_path, init_data in initiatives:
        init_id = init_data.get("id", "unknown")
        summary["initiatives_checked"] += 1

        # ── Step 1: Measure KR progress ───────────────────────────────────────
        ok, out = _run(
            [sys.executable, os.path.join(SCRIPTS_DIR, "hex-initiative.py"), "measure", init_id],
            dry_run,
        )
        summary["actions"].append({
            "initiative": init_id, "step": 1, "action": "measure_krs",
            "success": ok, "output": out,
        })
        if ok and not dry_run:
            try:
                init_data = _load_yaml(init_path)
            except Exception:
                pass

        # ── Step 2: Newly-met KRs ─────────────────────────────────────────────
        for kr in (init_data.get("key_results") or []):
            if kr.get("status") != "met":
                continue
            measured_at = kr.get("measured_at", "")
            if measured_at and _age_hours(measured_at) <= 2.0:
                kr_id = kr.get("id")
                summary["actions"].append({
                    "initiative": init_id, "step": 2, "action": "kr_newly_met",
                    "kr_id": kr_id,
                })
                _emit("initiative.kr.met", {
                    "initiative_id": init_id, "kr_id": kr_id,
                    "value": kr.get("current"), "target": kr.get("target"),
                }, dry_run=dry_run)

        # ── Step 3: Verdicts on ACTIVE/MEASURING experiments >= 48h ──────────
        # On PASS: activate/adopt. On FAIL: dispatch a new-approach spec.
        for exp_ref in (init_data.get("experiments") or []):
            exp_id = _exp_id_str(exp_ref)
            if not exp_id:
                continue
            exp_path, exp_data = exp_lookup.get(exp_id, (None, None))
            if exp_data is None:
                continue
            state = exp_data.get("state", "")
            if state not in ("ACTIVE", "MEASURING"):
                continue
            check_ts = (exp_data.get("post_change") or {}).get("collected_at") or exp_data.get("activated_at", "")
            if _age_hours(check_ts) < 48:
                continue

            ok, out = _run(
                [sys.executable, os.path.join(SCRIPTS_DIR, "hex-experiment.py"), "verdict", exp_id],
                dry_run,
            )
            verdict_result = "unknown"
            if ok and not dry_run:
                try:
                    refreshed = _load_yaml(exp_path)
                    verdict_result = refreshed.get("verdict", "unknown")
                except Exception:
                    pass
            else:
                verdict_result = "dry-run"

            action_entry = {
                "initiative": init_id, "step": 3, "action": "run_verdict",
                "experiment": exp_id, "verdict": verdict_result,
                "age_hours": round(_age_hours(check_ts), 1),
                "success": ok, "output": out,
            }

            if not ok or verdict_result == "FAIL":
                # Dispatch a new-approach spec for the KR this experiment targeted
                linked_kr_id = exp_data.get("kr_id") or exp_data.get("linked_kr")
                kr_obj = next(
                    (k for k in (init_data.get("key_results") or []) if k.get("id") == linked_kr_id),
                    None,
                )
                if kr_obj:
                    pivot_spec_data = {
                        "title": f"New Approach for {init_id}/{linked_kr_id} After Experiment {exp_id} Failed",
                        "mode": "execute",
                        "initiative": init_id,
                        "context": (
                            f"Experiment {exp_id} failed to move KR {linked_kr_id}.\n"
                            f"Hypothesis that failed: {str(exp_data.get('hypothesis', 'N/A'))[:200]}"
                        ),
                        "tasks": [{
                            "id": "t-1",
                            "title": f"Try a different approach for {linked_kr_id} (experiment {exp_id} failed)",
                            "status": "PENDING",
                            "spec": (
                                f"Experiment {exp_id} (\"{exp_data.get('title', '')}\") failed to move KR {linked_kr_id}.\n\n"
                                f"Hypothesis that failed: {str(exp_data.get('hypothesis', 'N/A'))[:200]}\n\n"
                                f"KR: {kr_obj.get('description', '')}\n"
                                f"Current: {kr_obj.get('current')}, Target: {kr_obj.get('target')}\n\n"
                                f"Design and execute a DIFFERENT approach. Analyze why the failed experiment didn't work, "
                                f"then take a fundamentally different action (different mechanism, different lever, different metric)."
                            ),
                            "verify": "bash -c 'METRIC_CMD' | python3 -c \"import sys; v=float(sys.stdin.read().strip()); assert v > 0, f'KR still at {v}'; print(f'KR moved to {v}')\"",
                        }],
                    }
                    pivot_spec = yaml.dump(pivot_spec_data, default_flow_style=False, allow_unicode=True, sort_keys=False)
                    ok2, qid, out2 = _write_and_dispatch_spec(pivot_spec, f"pivot-{exp_id}", dry_run)
                    action_entry["pivot_dispatched"] = qid or "[dry-run]"
                    action_entry["pivot_output"] = out2[:200]
                    summary["actions"].append({
                        "initiative": init_id, "step": 3, "action": "dispatch_spec",
                        "spec_type": "pivot_after_verdict_fail", "format": "yaml",
                        "experiment": exp_id, "kr_id": linked_kr_id,
                        "queue_id": qid or "[dry-run-q-XXX]", "success": ok2,
                    })
                    if not ok2 and out2.startswith("BUDGET_EXHAUSTED"):
                        budget_exhausted = True

            summary["actions"].append(action_entry)

        # ── Step 3b: Adopt PASS verdicts ──────────────────────────────────────
        # (reload to see fresh verdict state)
        if not dry_run:
            exp_lookup = _load_all_experiments()

        # ── Step 4: Activate BASELINE experiments immediately ─────────────────
        for exp_ref in (init_data.get("experiments") or []):
            exp_id = _exp_id_str(exp_ref)
            if not exp_id:
                continue
            _, exp_data = exp_lookup.get(exp_id, (None, None))
            if exp_data is None:
                continue
            if exp_data.get("state") != "BASELINE":
                continue
            ok, out = _run(
                [sys.executable, os.path.join(SCRIPTS_DIR, "hex-experiment.py"), "activate", exp_id],
                dry_run,
            )
            summary["actions"].append({
                "initiative": init_id, "step": 4, "action": "activate_experiment",
                "experiment": exp_id, "title": exp_data.get("title", ""),
                "success": ok, "output": out,
            })

        # ── Step 5: Baseline DRAFT experiments >= 1h ──────────────────────────
        for exp_ref in (init_data.get("experiments") or []):
            exp_id = _exp_id_str(exp_ref)
            if not exp_id:
                continue
            _, exp_data = exp_lookup.get(exp_id, (None, None))
            if exp_data is None:
                continue
            if exp_data.get("state") != "DRAFT":
                continue
            created = str(exp_data.get("created", ""))
            if _age_hours(created) >= 1.0:
                ok, out = _run(
                    [sys.executable, os.path.join(SCRIPTS_DIR, "hex-experiment.py"), "baseline", exp_id],
                    dry_run,
                )
                summary["actions"].append({
                    "initiative": init_id, "step": 5, "action": "collect_baseline",
                    "experiment": exp_id, "age_hours": round(_age_hours(created), 1),
                    "success": ok, "output": out,
                })

        # ── Step 6: Dispatch specs for uncovered KRs at current=0 ────────────
        covered_krs = set()
        for exp_ref in (init_data.get("experiments") or []):
            exp_id = _exp_id_str(exp_ref)
            if not exp_id:
                continue
            _, exp_data = exp_lookup.get(exp_id, (None, None))
            if exp_data is None:
                continue
            if exp_data.get("state", "") not in ACTIVE_EXP_STATES:
                continue
            linked_kr = exp_data.get("kr_id") or exp_data.get("linked_kr")
            if linked_kr:
                covered_krs.add(linked_kr)

        for kr in (init_data.get("key_results") or []):
            if kr.get("status") == "met":
                continue
            kr_id = kr.get("id")
            if kr_id in covered_krs:
                continue

            current = kr.get("current")
            target = kr.get("target")

            # Check for broken metric first (step 7)
            metric_cmd = (kr.get("metric") or {}).get("command", "")
            if metric_cmd and current is None:
                # Metric command likely broken
                init_data["_path"] = init_path
                fix_spec = _build_kr_fix_spec(init_id, kr, init_data)
                ok, qid, out = _write_and_dispatch_spec(fix_spec, f"fix-metric-{kr_id}", dry_run)
                summary["actions"].append({
                    "initiative": init_id, "step": 6, "action": "dispatch_spec",
                    "spec_type": "fix_broken_metric", "format": "yaml",
                    "kr_id": kr_id, "queue_id": qid or "[dry-run-q-XXX]",
                    "success": ok, "output": out[:200],
                })
                if not ok and out.startswith("BUDGET_EXHAUSTED"):
                    budget_exhausted = True
                continue

            # KR at zero with no active experiment — dispatch a drive spec
            if (current is not None and float(current) == 0.0) or (current == 0):
                if target is None:
                    # Also broken — dispatch metric fix
                    init_data["_path"] = init_path
                    fix_spec = _build_kr_fix_spec(init_id, kr, init_data)
                    ok, qid, out = _write_and_dispatch_spec(fix_spec, f"fix-metric-{kr_id}", dry_run)
                    summary["actions"].append({
                        "initiative": init_id, "step": 6, "action": "dispatch_spec",
                        "spec_type": "fix_missing_target", "format": "yaml",
                        "kr_id": kr_id, "queue_id": qid or "[dry-run-q-XXX]",
                        "success": ok, "output": out[:200],
                    })
                    if not ok and out.startswith("BUDGET_EXHAUSTED"):
                        budget_exhausted = True
                    continue

                init_data["_path"] = init_path
                drive_spec = _build_kr_dispatch_spec(init_id, kr, init_data)
                ok, qid, out = _write_and_dispatch_spec(drive_spec, f"drive-{kr_id}", dry_run)
                summary["actions"].append({
                    "initiative": init_id, "step": 6, "action": "dispatch_spec",
                    "spec_type": "drive_kr_to_nonzero", "format": "yaml",
                    "kr_id": kr_id, "queue_id": qid or "[dry-run-q-XXX]",
                    "success": ok, "output": out[:200],
                })
                if not ok and out.startswith("BUDGET_EXHAUSTED"):
                    budget_exhausted = True

        # ── Step 7 (horizon escalation) ───────────────────────────────────────
        horizon = init_data.get("horizon")
        if horizon:
            days_left = _days_until(horizon)
            unmet = [kr.get("id") for kr in (init_data.get("key_results") or [])
                     if kr.get("status") != "met"]
            if days_left <= 14 and unmet:
                msg = (
                    f"ESCALATION: {init_id} horizon in {days_left} days "
                    f"with {len(unmet)} unmet KR(s): {', '.join(str(k) for k in unmet)}"
                )
                summary["actions"].append({
                    "initiative": init_id, "step": 7, "action": "escalate_horizon",
                    "days_remaining": days_left, "unmet_krs": unmet, "message": msg,
                })
                _emit("initiative.at_risk", {
                    "initiative_id": init_id, "days_remaining": days_left,
                    "unmet_krs": unmet, "channel": "#from-hex",
                }, dry_run=dry_run)

    # ── Step 8: Budget escalation ─────────────────────────────────────────────
    if budget_exhausted:
        summary["actions"].append({
            "step": 8, "action": "escalate_budget",
            "message": "Budget exhausted — one or more dispatch attempts failed due to budget.",
        })
        _emit("hex.budget.escalation", {
            "agent": agent_id,
            "channel": "#from-hex",
            "message": f"Initiative loop for {agent_id} hit budget limit. Dispatches blocked.",
        }, dry_run=dry_run)

    # ── Step 9: Self-assess — every 5 runs, pivot if no KR moved ─────────────
    recent_runs = _load_recent_runs(agent_id, count=5)
    run_count = len(recent_runs) + 1

    # Record this run's KR snapshot
    kr_snapshot_after = {}
    for _, init_data in _load_initiatives_for_agent(agent_id) if not dry_run else []:
        init_id = init_data.get("id", "unknown")
        for kr in (init_data.get("key_results") or []):
            kr_snapshot_after[f"{init_id}/{kr.get('id')}"] = kr.get("current")

    if not dry_run:
        _record_run(agent_id, kr_snapshot_after)

    if run_count >= 5 and len(recent_runs) >= 4:
        oldest_snapshot = recent_runs[0].get("kr_snapshot", {})
        any_kr_moved = any(
            oldest_snapshot.get(k) != kr_snapshot_before.get(k)
            for k in kr_snapshot_before
        )
        if not any_kr_moved:
            # Collect still-stalled KRs
            stalled_krs = []
            for _, init_data in initiatives:
                init_id = init_data.get("id", "unknown")
                for kr in (init_data.get("key_results") or []):
                    if kr.get("status") != "met":
                        stalled_krs.append({**kr, "_init_id": init_id})

            if stalled_krs:
                # Group by initiative for one pivot spec per initiative
                from collections import defaultdict
                by_init = defaultdict(list)
                for kr in stalled_krs:
                    by_init[kr["_init_id"]].append(kr)

                for init_id, krs in by_init.items():
                    _, init_data = next(
                        ((p, d) for p, d in initiatives if d.get("id") == init_id),
                        (None, {})
                    )
                    pivot_spec = _build_pivot_spec(init_id, krs, init_data or {}, run_count)
                    ok, qid, out = _write_and_dispatch_spec(
                        pivot_spec, f"self-assess-pivot-{init_id}", dry_run
                    )
                    summary["actions"].append({
                        "step": 9, "action": "dispatch_spec",
                        "spec_type": "self_assess_pivot", "format": "yaml",
                        "initiative": init_id,
                        "reason": f"{run_count} consecutive runs with zero KR movement",
                        "queue_id": qid or "[dry-run-q-XXX]",
                        "success": ok,
                    })

    summary["runs_tracked"] = run_count
    return summary


# ── CLI ───────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Run the initiative execution loop (v2) for an agent.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("--agent", required=True,
                        help="Agent ID (must match owner field in initiative YAML)")
    parser.add_argument("--dry-run", action="store_true",
                        help="Preview actions without executing commands or writing files")
    parser.add_argument("--initiative",
                        help="Restrict loop to a single initiative ID")
    args = parser.parse_args()

    summary = run_loop(
        agent_id=args.agent,
        dry_run=args.dry_run,
        filter_initiative=args.initiative,
    )
    print(json.dumps(summary, indent=2, default=str))
    return 0


if __name__ == "__main__":
    sys.exit(main())
