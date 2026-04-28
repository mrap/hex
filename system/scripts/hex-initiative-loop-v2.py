#!/usr/bin/env python3
"""hex-initiative-loop-v2 — execution engine initiative loop for any agent.

Usage:
  hex-initiative-loop-v2.py --agent <id> [--dry-run] [--initiative <id>]

Runs the 8-step initiative execution loop for all initiatives owned by the agent.
Unlike v1, every step COMPLETES its action — no proposals that wait for someone else.

  1. Measure KRs — run metric commands, reload data.
  1.5. Measure experiments — run hex-experiment.py measure on all ACTIVE experiments.
     If ACTIVE > 48h: also run verdict immediately (pass/fail/inconclusive).
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
SNAPSHOTS_LOG = os.path.join(AUDIT_DIR, "kr-snapshots.jsonl")
PATTERN_LIBRARY = os.path.join(AUDIT_DIR, "patterns.jsonl")
PIVOTS_LOG = os.path.join(AUDIT_DIR, "pivots.jsonl")


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


# ── pivot tracking ────────────────────────────────────────────────────────────

def _count_kr_pivots(kr_id, exp_lookup):
    """Count total pivots recorded across all experiments for a given KR."""
    count = 0
    for _, (_, exp_data) in exp_lookup.items():
        linked = exp_data.get("kr_id") or exp_data.get("linked_kr")
        if linked == kr_id:
            count += len(exp_data.get("pivots") or [])
    return count


def _record_pivot(exp_path, exp_data, reason, new_hypothesis, dry_run):
    """Append a pivot entry to the failing experiment's YAML under `pivots:`.

    Returns the new pivot count for this experiment.
    """
    if dry_run:
        return len(exp_data.get("pivots") or []) + 1
    pivots = list(exp_data.get("pivots") or [])
    pivots.append({
        "from": exp_data.get("id"),
        "reason": reason,
        "new_hypothesis": new_hypothesis,
        "date": _now_iso()[:10],
    })
    exp_data["pivots"] = pivots
    _save_yaml(exp_data, exp_path)
    return len(pivots)


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

_behavioral_memory_initialized = False


def _init_behavioral_memory():
    """Ensure behavioral_patterns schema exists and is bootstrapped from feedback files.

    Called once per process. Idempotent — bootstrap() uses INSERT OR IGNORE.
    """
    global _behavioral_memory_initialized
    if _behavioral_memory_initialized:
        return
    try:
        sys.path.insert(0, SCRIPTS_DIR)
        from behavioral_memory import BehavioralMemory  # noqa: PLC0415
        bm = BehavioralMemory()
        bm.ensure_schema()
        result = bm.load_feedback_from_files()
        if result.get("imported", 0) > 0:
            print(
                f"[behavioral_memory] Bootstrapped {result['imported']} patterns "
                f"(skipped={result.get('skipped', 0)}, errors={result.get('errors', 0)})",
                file=sys.stderr,
            )
        bm.close()
    except Exception as exc:
        print(f"[behavioral_memory] init warn: {exc}", file=sys.stderr)
    _behavioral_memory_initialized = True


def _check_behavioral_patterns(agent_id: str, context: str) -> dict:
    """Query behavioral_patterns before the loop runs — error-to-lesson loop hook.

    Returns the check_behavior result so callers can log it or gate on risk_level.
    HIGH risk patterns are logged to stderr so the agent can see them.
    """
    try:
        sys.path.insert(0, SCRIPTS_DIR)
        from behavioral_memory import check_behavior  # noqa: PLC0415
        result = check_behavior(context)
        if result.get("risk_level") in ("HIGH", "MEDIUM") and result.get("matches"):
            top = result["matches"][0]
            print(
                f"[behavioral_pattern] {result['risk_level']} risk for '{context[:60]}': "
                f"{top['pattern'][:80]} (×{top['correction_count']})",
                file=sys.stderr,
            )
        return result
    except Exception as exc:
        return {"risk_level": "NONE", "matches": [], "error": str(exc)}


def _store_frustration_corrections(agent_id: str, signals: list[dict]):
    """Store detected frustration signals as behavioral corrections."""
    try:
        sys.path.insert(0, SCRIPTS_DIR)
        from behavioral_memory import BehavioralMemory  # noqa: PLC0415
        bm = BehavioralMemory()
        for sig in signals:
            pattern = sig.get("pattern", sig.get("signal", ""))
            rule = sig.get("rule", sig.get("correction", pattern))
            if pattern:
                bm.store_correction(
                    pattern_text=pattern,
                    rule_text=rule,
                    agent_id=agent_id,
                    source_file=sig.get("source_file", ""),
                    detection_type="transcript_scan",
                )
        bm.close()
    except Exception as exc:
        print(f"[behavioral_memory] store_frustration warn: {exc}", file=sys.stderr)


def run_loop(agent_id, dry_run=False, filter_initiative=None):
    summary = {
        "agent": agent_id,
        "timestamp": _now_iso(),
        "dry_run": dry_run,
        "version": "v2",
        "initiatives_checked": 0,
        "actions": [],
    }

    # Ensure feedback loop data files exist (L1, L3, L4)
    os.makedirs(AUDIT_DIR, exist_ok=True)
    for _path in (SNAPSHOTS_LOG, PATTERN_LIBRARY, PIVOTS_LOG):
        if not os.path.exists(_path):
            open(_path, "a").close()

    # Initialize behavioral memory (schema + bootstrap from feedback files) once per process
    _init_behavioral_memory()

    # Error-to-lesson loop: check behavioral_patterns before dispatching work
    behavior_check = _check_behavioral_patterns(
        agent_id, f"agent {agent_id} dispatching initiative specs"
    )
    summary["behavioral_check"] = {
        "risk_level": behavior_check.get("risk_level", "NONE"),
        "match_count": len(behavior_check.get("matches", [])),
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

        # ── Step 1.5: Measure ACTIVE experiments ─────────────────────────────
        for exp_ref in (init_data.get("experiments") or []):
            exp_id = _exp_id_str(exp_ref)
            if not exp_id:
                continue
            exp_path, exp_data = exp_lookup.get(exp_id, (None, None))
            if exp_data is None:
                continue
            if exp_data.get("state") != "ACTIVE":
                continue

            ok, out = _run(
                [sys.executable, os.path.join(SCRIPTS_DIR, "hex-experiment.py"), "measure", exp_id],
                dry_run,
            )
            baseline_vals = (exp_data.get("baseline") or {}).get("values") or {}
            primary_name = ((exp_data.get("metrics") or {}).get("primary") or {}).get("name", "primary")
            summary["actions"].append({
                "initiative": init_id, "step": "1.5", "action": "measure_experiment",
                "experiment": exp_id, "title": exp_data.get("title", ""),
                "baseline": baseline_vals.get(primary_name, "N/A"),
                "success": ok, "output": out,
            })
            if ok and not dry_run:
                try:
                    exp_data = _load_yaml(exp_path)
                    exp_lookup[exp_id] = (exp_path, exp_data)
                except Exception:
                    pass

            # If ACTIVE >48h, run verdict immediately after measuring
            activated_at = exp_data.get("activated_at", "")
            if ok and _age_hours(activated_at) >= 48:
                ok2, out2 = _run(
                    [sys.executable, os.path.join(SCRIPTS_DIR, "hex-experiment.py"), "verdict", exp_id],
                    dry_run,
                )
                verdict_result = "dry-run"
                if ok2 and not dry_run:
                    try:
                        refreshed = _load_yaml(exp_path)
                        vd = refreshed.get("verdict", {})
                        verdict_result = vd.get("result", "unknown") if isinstance(vd, dict) else str(vd or "unknown")
                        exp_lookup[exp_id] = (exp_path, refreshed)
                    except Exception:
                        pass
                summary["actions"].append({
                    "initiative": init_id, "step": "1.5", "action": "run_verdict",
                    "experiment": exp_id, "verdict": verdict_result,
                    "age_hours": round(_age_hours(activated_at), 1),
                    "success": ok2, "output": out2,
                })

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
            refreshed_exp = None
            if ok and not dry_run:
                try:
                    refreshed_exp = _load_yaml(exp_path)
                    vd = refreshed_exp.get("verdict") or {}
                    verdict_result = vd.get("result", "unknown") if isinstance(vd, dict) else str(vd or "unknown")
                    exp_lookup[exp_id] = (exp_path, refreshed_exp)
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

            if not ok or verdict_result == "fail":
                # Dispatch a new-approach spec for the KR this experiment targeted
                linked_kr_id = exp_data.get("kr_id") or exp_data.get("linked_kr")
                kr_obj = next(
                    (k for k in (init_data.get("key_results") or []) if k.get("id") == linked_kr_id),
                    None,
                )

                # Extract failure details for pivot record
                fresh = refreshed_exp or exp_data
                vd = (fresh.get("verdict") or {}) if isinstance(fresh.get("verdict"), dict) else {}
                delta_pct = vd.get("primary_delta_pct", "N/A")
                fail_reason = (
                    f"Experiment {exp_id} didn't move {linked_kr_id or 'KR'}: "
                    f"achieved {delta_pct}% improvement. "
                    f"Failed hypothesis: {str(exp_data.get('hypothesis', 'N/A'))[:120]}"
                )
                new_hypothesis = (
                    f"Previous approach ({exp_id}) failed. "
                    f"Need a fundamentally different mechanism to move {linked_kr_id or 'KR'}."
                )

                # Record pivot in experiment YAML under `pivots:` field
                this_pivot_count = _record_pivot(
                    exp_path, fresh, fail_reason, new_hypothesis, dry_run
                )

                # Count total pivots across all experiments for this KR
                total_pivot_count = max(this_pivot_count, _count_kr_pivots(linked_kr_id or "", exp_lookup))

                if total_pivot_count >= 3 and linked_kr_id:
                    # 3+ pivots with no KR movement — escalate to Mike
                    msg = (
                        f"PIVOT ESCALATION: {init_id}/{linked_kr_id} has had {total_pivot_count} "
                        f"pivot(s) across experiments with no movement. "
                        f"Latest failed experiment: {exp_id}. "
                        f"Full pivot history tracked in experiments/*.yaml under 'pivots:' field."
                    )
                    summary["actions"].append({
                        "initiative": init_id, "step": 3, "action": "escalate_pivot",
                        "experiment": exp_id, "kr_id": linked_kr_id,
                        "pivot_count": total_pivot_count, "message": msg,
                    })
                    _emit("initiative.pivot.escalation", {
                        "initiative_id": init_id, "kr_id": linked_kr_id,
                        "experiment_id": exp_id, "pivot_count": total_pivot_count,
                        "channel": "#from-hex", "message": msg,
                    }, dry_run=dry_run)
                elif kr_obj:
                    pivot_spec_data = {
                        "title": f"New Approach for {init_id}/{linked_kr_id} After Experiment {exp_id} Failed",
                        "mode": "execute",
                        "initiative": init_id,
                        "context": (
                            f"Experiment {exp_id} failed to move KR {linked_kr_id}.\n"
                            f"Failure reason: {fail_reason}\n"
                            f"This is pivot #{total_pivot_count} for {linked_kr_id}. "
                            f"3 pivots will trigger escalation to Mike."
                        ),
                        "tasks": [{
                            "id": "t-1",
                            "title": f"Create new experiment for {linked_kr_id} (pivot #{total_pivot_count})",
                            "status": "PENDING",
                            "spec": (
                                f"Experiment {exp_id} (\"{exp_data.get('title', '')}\") failed to move KR {linked_kr_id}.\n\n"
                                f"Failure reason: {fail_reason}\n\n"
                                f"KR: {kr_obj.get('description', '')}\n"
                                f"Current: {kr_obj.get('current')}, Target: {kr_obj.get('target')}\n\n"
                                f"This is pivot #{total_pivot_count}. "
                                f"Design a DIFFERENT approach (different mechanism, different lever, different metric).\n\n"
                                f"Steps:\n"
                                f"1. Analyze WHY {exp_id} failed — read experiments/{exp_id}-*.yaml for context\n"
                                f"2. Write a new experiment YAML with a different hypothesis\n"
                                f"3. Run: python3 .hex/scripts/hex-experiment.py create <new-exp.yaml>\n"
                                f"4. Run: python3 .hex/scripts/hex-experiment.py baseline <new-exp-id>\n"
                                f"5. Run: python3 .hex/scripts/hex-experiment.py activate <new-exp-id>\n"
                                f"6. Add the new experiment ID to the initiative's experiments list"
                            ),
                            "verify": (
                                f"python3 .hex/scripts/hex-experiment.py list 2>&1 | "
                                f"grep -c ACTIVE | xargs test 1 -le"
                            ),
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
                        "pivot_count": total_pivot_count,
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

    # Record this run's KR snapshot (flat format matching kr-snapshots.jsonl)
    kr_snapshot_after = {}
    for _, init_data in (_load_initiatives_for_agent(agent_id) if not dry_run else []):
        init_id = init_data.get("id", "unknown")
        for kr in (init_data.get("key_results") or []):
            kr_snapshot_after[f"{init_id}/{kr.get('id')}"] = kr.get("current")

    # Always write structured per-initiative KR snapshots (L1 feedback loop)
    _write_kr_snapshots_per_initiative(agent_id, initiatives)

    if not dry_run:
        _record_run(agent_id, kr_snapshot_after)
        # Also write flat snapshot to SNAPSHOTS_LOG for self_improvement stall detection
        os.makedirs(AUDIT_DIR, exist_ok=True)
        with open(SNAPSHOTS_LOG, "a", encoding="utf-8") as _fh:
            _fh.write(json.dumps({"ts": _now_iso(), "agent": agent_id,
                                   "snapshot": kr_snapshot_after}) + "\n")

    if run_count >= 5 and len(recent_runs) >= 4:
        oldest_snapshot = recent_runs[0].get("kr_snapshot", {})
        any_kr_moved = any(
            oldest_snapshot.get(k) != kr_snapshot_before.get(k)
            for k in kr_snapshot_before
        )

        if any_kr_moved:
            # KRs moved — log successful approaches to pattern_library
            if not dry_run:
                _log_moved_krs_to_pattern_library(
                    agent_id, initiatives, oldest_snapshot, kr_snapshot_before
                )
                # Cross-seed: apply successful patterns to stalled KRs in other initiatives
                _cross_seed_successful_patterns(
                    agent_id, initiatives, oldest_snapshot, kr_snapshot_before, summary
                )
        else:
            # No KR moved — call self_assess via self_improvement module
            try:
                sys.path.insert(0, SCRIPTS_DIR)
                from self_improvement import run_self_assess  # noqa: PLC0415
                si_initiatives = [d for _, d in initiatives]
                si_actions = run_self_assess(agent_id, si_initiatives, dry_run=dry_run)
                for action in si_actions:
                    summary["actions"].append({
                        "step": 9, "action": action.get("action"),
                        "spec_type": "self_assess_pivot",
                        "initiative": action.get("initiative_id"),
                        "kr_id": action.get("kr_id"),
                        "stall_category": action.get("stall_category"),
                        "pivot_num": action.get("pivot_num"),
                        "queue_id": action.get("spec_id"),
                        "dry_run": action.get("dry_run", dry_run),
                    })
                    print(
                        f"[self_assess] {action.get('action')}: "
                        f"{action.get('initiative_id')}/{action.get('kr_id')} "
                        f"pivot #{action.get('pivot_num', '?')} → {action.get('spec_id', 'escalated')}",
                        file=sys.stderr,
                    )
            except ImportError:
                # self_improvement.py not on path — fall back to basic pivot spec
                stalled_krs = []
                for _, init_data in initiatives:
                    init_id = init_data.get("id", "unknown")
                    for kr in (init_data.get("key_results") or []):
                        if kr.get("status") != "met":
                            stalled_krs.append({**kr, "_init_id": init_id})

                if stalled_krs:
                    from collections import defaultdict  # noqa: PLC0415
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


def _append_jsonl(path, entry):
    """Append a single JSON object as a line to a .jsonl file."""
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(entry, default=str) + "\n")


def _write_kr_snapshots_per_initiative(agent_id, initiatives):
    """Write one structured snapshot line per initiative to kr-snapshots.jsonl.

    Format: {"ts": ISO, "agent": "cos", "initiative": "init-xxx",
             "krs": [{"id": "kr-1", "current": 0.5, "target": 1.0}, ...]}
    """
    ts = _now_iso()
    for _, init_data in initiatives:
        init_id = init_data.get("id", "unknown")
        krs = []
        for kr in (init_data.get("key_results") or []):
            krs.append({
                "id": kr.get("id"),
                "current": kr.get("current"),
                "target": kr.get("target"),
            })
        if krs:
            _append_jsonl(SNAPSHOTS_LOG, {
                "ts": ts,
                "agent": agent_id,
                "initiative": init_id,
                "krs": krs,
            })


def _get_spec_modified_files(spec_id):
    """Return a set of file paths modified by the given spec (via git log on the worktree)."""
    if not spec_id:
        return set()
    try:
        # BOI worktrees are named after the queue id; look for the worktree path
        boi_root = os.path.expanduser("~/.boi")
        worktrees_dir = os.path.join(boi_root, "worktrees")
        # Try the canonical name pattern: <spec_id> maps to a worktree directory
        candidate = os.path.join(worktrees_dir, spec_id)
        if not os.path.isdir(candidate):
            # Also try boi-worker-N pattern by looking for recent git commits
            return set()
        result = subprocess.run(
            ["git", "-C", candidate, "diff", "--name-only", "HEAD~1", "HEAD"],
            capture_output=True, text=True, timeout=10,
        )
        if result.returncode == 0:
            return {line.strip() for line in result.stdout.splitlines() if line.strip()}
    except Exception:
        pass
    return set()


def _verify_pattern_claim(kr, old_val, new_val, latest_spec):
    """Re-measure the KR and assess causation confidence.

    Returns (confirmed: bool, confidence: str, re_measure_val: float|None, notes: list[str]).
    confirmed=False means skip logging entirely.
    confidence is 'high' or 'low'.
    """
    notes = []
    metric_cmd = (kr.get("metric") or {}).get("command", "")

    # Step 1: re-measure the KR right now
    re_measure_val = None
    if metric_cmd:
        ok, re_val, _ = _run_metric_command(metric_cmd, dry_run=False)
        if ok and re_val is not None:
            re_measure_val = re_val
            try:
                re_delta = float(re_val) - float(old_val)
                if re_delta <= 0:
                    notes.append(f"re-measure={re_val} did not confirm movement from {old_val}")
                    return False, "low", re_measure_val, notes
            except (TypeError, ValueError):
                notes.append("re-measure value not numeric; skipping pattern log")
                return False, "low", re_measure_val, notes
        else:
            notes.append("re-measure failed; skipping pattern log")
            return False, "low", re_measure_val, notes

    # Step 2: assess causation — do spec's modified files overlap with metric inputs?
    confidence = "high"
    if latest_spec and metric_cmd:
        spec_files = _get_spec_modified_files(latest_spec)
        if spec_files:
            # Heuristic: extract path-like tokens from the metric command
            metric_tokens = set()
            for token in metric_cmd.split():
                if "/" in token or token.endswith(".py") or token.endswith(".sh"):
                    metric_tokens.add(token.lstrip("~").lstrip("/"))
            overlap = any(
                any(mtoken in sf or sf.endswith(mtoken) for mtoken in metric_tokens)
                for sf in spec_files
            ) if metric_tokens else False
            if not overlap:
                confidence = "low"
                notes.append(
                    f"spec {latest_spec} modified files {sorted(spec_files)[:5]} "
                    f"have no overlap with metric command inputs — correlation may be coincidental"
                )
        else:
            notes.append(f"could not retrieve modified files for spec {latest_spec}; defaulting confidence=low")
            confidence = "low"

    return True, confidence, re_measure_val, notes


def _log_moved_krs_to_pattern_library(agent_id, initiatives, oldest_snapshot, current_snapshot):
    """Write successful KR movements to the pattern_library (patterns.jsonl).

    Before logging, re-measure the KR and verify causation. Patterns with
    confidence:low are logged for reference but not auto-applied in cross-seeding.
    """
    os.makedirs(AUDIT_DIR, exist_ok=True)
    ts = _now_iso()
    for _, init_data in initiatives:
        init_id = init_data.get("id", "unknown")
        for kr in (init_data.get("key_results") or []):
            key = f"{init_id}/{kr.get('id')}"
            old_val = oldest_snapshot.get(key)
            new_val = current_snapshot.get(key)
            if old_val is None or new_val is None:
                continue
            try:
                delta = float(new_val) - float(old_val)
            except (TypeError, ValueError):
                continue
            if delta <= 0:
                continue
            desc = kr.get("description", "")
            kr_type = _classify_kr_type(desc)
            spec_ids = init_data.get("specs") or []
            latest_spec = spec_ids[-1] if spec_ids else None

            # Verify pattern claim before logging: re-measure and assess causation
            confirmed, confidence, re_measure_val, verify_notes = _verify_pattern_claim(
                kr, old_val, new_val, latest_spec
            )
            if not confirmed:
                # Re-measurement didn't confirm movement — skip logging
                print(
                    f"[pattern-library] SKIP {key}: {'; '.join(verify_notes)}",
                    file=sys.stderr,
                )
                continue

            entry = {
                "ts": ts,
                "agent": agent_id,
                "initiative": init_id,
                "initiative_id": init_id,  # kept for backward compat
                "kr_id": kr.get("id"),
                "kr_type": kr_type,
                "metric_command": (kr.get("metric") or {}).get("command", ""),
                "outcome": "success",
                "confidence": confidence,
                "re_measure_val": re_measure_val,
                "verify_notes": verify_notes,
                "approach": (
                    f"Spec {latest_spec} moved KR from {old_val} to {new_val}"
                    if latest_spec else f"KR moved from {old_val} to {new_val}"
                ),
                "approach_type": "dispatch_spec",
                "approach_summary": (
                    f"Spec {latest_spec} moved KR from {old_val} to {new_val}"
                    if latest_spec else f"KR moved from {old_val} to {new_val}"
                ),
                "spec_id": latest_spec,
                "applied_from": key,
                "delta": delta,
            }
            _append_jsonl(PATTERN_LIBRARY, entry)


def _classify_kr_type(description):
    """Map KR description to a type for pattern_library cross-matching."""
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


def _cross_seed_successful_patterns(agent_id, initiatives, oldest_snapshot, current_snapshot, summary):
    """After logging successful KR movements, seed the pattern to stalled KRs elsewhere.

    For each KR that moved, build a pattern dict and call seed_cross_initiative()
    from self_improvement.py. Any cross-seed proposals are logged to patterns.jsonl
    and appended to the loop summary.
    """
    try:
        sys.path.insert(0, SCRIPTS_DIR)
        from self_improvement import seed_cross_initiative  # noqa: PLC0415
    except ImportError:
        return

    all_init_dicts = [d for _, d in initiatives]
    # Also load initiatives from other agents (cross-seeding is cross-initiative)
    try:
        all_yamls = []
        if os.path.isdir(INITIATIVES_DIR):
            for fname in sorted(os.listdir(INITIATIVES_DIR)):
                if not fname.endswith(".yaml") or fname.endswith(".lock"):
                    continue
                path = os.path.join(INITIATIVES_DIR, fname)
                try:
                    data = _load_yaml(path)
                    if data.get("status", "active") == "active":
                        all_yamls.append(data)
                except Exception:
                    continue
        if all_yamls:
            all_init_dicts = all_yamls
    except Exception:
        pass

    for _, init_data in initiatives:
        init_id = init_data.get("id", "unknown")
        for kr in (init_data.get("key_results") or []):
            key = f"{init_id}/{kr.get('id')}"
            old_val = oldest_snapshot.get(key)
            new_val = current_snapshot.get(key)
            if old_val is None or new_val is None:
                continue
            try:
                delta = float(new_val) - float(old_val)
            except (TypeError, ValueError):
                continue
            if delta <= 0:
                continue

            # Build successful pattern dict for cross-seeding
            desc = kr.get("description", "")
            kr_type = _classify_kr_type(desc)
            spec_ids = init_data.get("specs") or []
            latest_spec = spec_ids[-1] if spec_ids else None

            # Skip cross-seeding patterns with confidence:low — they may be coincidental
            _, confidence, _, _ = _verify_pattern_claim(kr, old_val, new_val, latest_spec)
            if confidence == "low":
                continue

            pattern = {
                "initiative_id": init_id,
                "kr_id": kr.get("id"),
                "kr_type": kr_type,
                "kr_description": desc,
                "approach": (
                    f"Spec {latest_spec} moved KR from {old_val} to {new_val}"
                    if latest_spec else f"KR moved from {old_val} to {new_val}"
                ),
                "approach_summary": (
                    f"Spec {latest_spec} moved KR from {old_val} to {new_val}"
                    if latest_spec else f"KR moved from {old_val} to {new_val}"
                ),
                "delta": delta,
            }

            seeds = seed_cross_initiative(pattern, all_init_dicts)
            for seed in seeds:
                summary["actions"].append({
                    "step": 9,
                    "action": "cross_seed",
                    "from_initiative": seed.get("from_initiative"),
                    "from_kr": seed.get("from_kr"),
                    "to_initiative": seed.get("to_initiative"),
                    "to_kr": seed.get("to_kr"),
                    "kr_type": seed.get("kr_type"),
                    "proposed_experiment": seed.get("proposed_experiment"),
                })


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
