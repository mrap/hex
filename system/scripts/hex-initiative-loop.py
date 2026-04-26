#!/usr/bin/env python3
"""hex-initiative-loop — executable initiative operating loop for any agent.

Usage:
  hex-initiative-loop.py --agent <id> [--dry-run] [--initiative <id>]

Runs the 7-step initiative loop for all initiatives owned by the agent:
  1. Measure KR progress (hex initiative measure)
  2. Emit events for newly-met KRs
  3. Run verdicts on MEASURING experiments >= 48h old
  4. Propose experiment YAMLs for KRs with no active experiment
  5. Collect baselines for DRAFT experiments >= 24h old
  6. Emit ready_for_activation for BASELINE experiments
  7. Escalate if initiative horizon <= 14 days with unmet KRs

Outputs a JSON summary of all actions taken. Use --dry-run to preview
without making changes or executing commands.
"""

import argparse
import json
import os
import subprocess
import sys
from datetime import datetime, date, timezone

try:
    import yaml
except ImportError:
    print("ERROR: PyYAML not installed. Run: pip install pyyaml", file=sys.stderr)
    sys.exit(1)

HEX_ROOT = os.environ.get("HEX_ROOT", os.path.expanduser("${AGENT_DIR:-$HOME/hex}"))
INITIATIVES_DIR = os.path.join(HEX_ROOT, "initiatives")
EXPERIMENTS_DIR = os.path.join(HEX_ROOT, "experiments")
SCRIPTS_DIR = os.path.join(HEX_ROOT, ".hex", "scripts")
TELEMETRY_PATH = os.path.join(HEX_ROOT, ".hex", "telemetry")


# ── telemetry ─────────────────────────────────────────────────────────────────

def _emit(event_type, payload, dry_run=False):
    if dry_run:
        return
    sys.path.insert(0, TELEMETRY_PATH)
    try:
        from emit import emit
        emit(event_type, payload, source="hex-initiative-loop")
    except Exception as exc:
        print(f"[initiative-loop] telemetry warn: {exc}", file=sys.stderr)


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
    """Hours elapsed since an ISO datetime string (or YYYY-MM-DD date)."""
    if not dt_str:
        return 0.0
    s = str(dt_str).strip()
    # Bare date: treat as midnight UTC of that day
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

def _run(args, dry_run):
    """Execute a command. Returns (success, output_str). No-ops in dry_run."""
    if dry_run:
        return True, "[dry-run skipped]"
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=120)
        ok = result.returncode == 0
        out = (result.stdout.strip() or result.stderr.strip())[:300]
        return ok, out
    except Exception as exc:
        return False, str(exc)[:200]


# ── initiative / experiment loaders ──────────────────────────────────────────

def _load_initiatives_for_agent(agent_id, filter_id=None):
    """Return list of (filepath, data) for active initiatives owned by agent_id."""
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
    """Return dict {exp_id: (filepath, data)} for all exp-NNN experiments."""
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


# ── experiment proposal generator ────────────────────────────────────────────

def _build_proposed_experiment(init_id, kr, initiative_data):
    """Build a draft experiment YAML dict for a KR that has no active experiment."""
    kr_id = kr.get("id", "kr-?")
    kr_desc = kr.get("description", "")
    target = kr.get("target")
    current = kr.get("current")
    metric = kr.get("metric") or {}
    direction = metric.get("direction", "lower_is_better")
    horizon = initiative_data.get("horizon", "2026-12-31")

    title = f"Improve {kr_id} for {init_id}"

    return {
        "title": title,
        "hypothesis": (
            f"If we make targeted improvements for {kr_id} ({kr_desc[:80].rstrip()}), "
            f"then the metric will move from its current value of {current} toward "
            f"the target of {target} (direction: {direction}) "
            f"within the initiative horizon {horizon}."
        ),
        "owner": initiative_data.get("owner", "unknown"),
        "initiative": init_id,
        "kr_id": kr_id,
        "change_description": (
            f"[TODO: Describe the specific change to improve {kr_id}. "
            f"Current: {current}, target: {target}.]"
        ),
        "time_bound": {
            "measure_by": str(horizon),
            "min_cycles_before_measure": 5,
        },
        "metrics": {
            "primary": {
                "name": kr_id,
                "description": kr_desc,
                "command": metric.get("command", "echo 0"),
                "direction": direction,
            },
        },
        "success_criteria": {
            "primary": {
                "metric": kr_id,
                "type": "absolute_threshold",
                "target": target,
            },
        },
        "rollback_plan": {
            "description": "[TODO: Describe rollback steps if this experiment fails.]",
            "commands": [],
        },
    }


# ── active states ─────────────────────────────────────────────────────────────

ACTIVE_EXP_STATES = {"DRAFT", "BASELINE", "ACTIVE", "MEASURING"}


# ── main loop ─────────────────────────────────────────────────────────────────

def run_loop(agent_id, dry_run=False, filter_initiative=None):
    summary = {
        "agent": agent_id,
        "timestamp": _now_iso(),
        "dry_run": dry_run,
        "initiatives_checked": 0,
        "actions": [],
    }

    initiatives = _load_initiatives_for_agent(agent_id, filter_id=filter_initiative)
    if not initiatives:
        summary["note"] = f"No active initiatives found for agent '{agent_id}'"
        return summary

    exp_lookup = _load_all_experiments()

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
        # Reload so we see updated KR values
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
            # Only flag as "newly met" if measurement happened in last 2h
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

        # ── Step 3: Verdicts on MEASURING experiments >= 48h ─────────────────
        for exp_id in (init_data.get("experiments") or []):
            _, exp_data = exp_lookup.get(exp_id, (None, None))
            if exp_data is None:
                continue
            if exp_data.get("state") != "MEASURING":
                continue
            post = exp_data.get("post_change") or {}
            collected_at = post.get("collected_at", "")
            if _age_hours(collected_at) >= 48:
                ok, out = _run(
                    [sys.executable, os.path.join(SCRIPTS_DIR, "hex-experiment.py"), "verdict", exp_id],
                    dry_run,
                )
                summary["actions"].append({
                    "initiative": init_id, "step": 3, "action": "run_verdict",
                    "experiment": exp_id, "age_hours": round(_age_hours(collected_at), 1),
                    "success": ok, "output": out,
                })

        # ── Step 4: Propose experiments for KRs with no active experiment ─────
        # Determine which KRs are already covered by an active experiment
        covered_krs = set()
        for exp_id in (init_data.get("experiments") or []):
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

            proposed = _build_proposed_experiment(init_id, kr, init_data)
            if not dry_run:
                new_id = _next_exp_id()
                slug = _slugify(proposed["title"])
                exp_path = os.path.join(EXPERIMENTS_DIR, f"{new_id}-{slug}.yaml")
                _save_yaml(proposed, exp_path)
                _emit("experiment.proposed", {
                    "initiative_id": init_id, "kr_id": kr_id, "exp_path": exp_path,
                }, dry_run=False)
                exp_id_label = new_id
            else:
                exp_id_label = "[dry-run-new]"

            summary["actions"].append({
                "initiative": init_id, "step": 4, "action": "propose_experiment",
                "kr_id": kr_id, "proposed_id": exp_id_label,
                "hypothesis_preview": proposed["hypothesis"][:120],
            })

        # ── Step 5: Baseline DRAFT experiments >= 24h ─────────────────────────
        for exp_id in (init_data.get("experiments") or []):
            _, exp_data = exp_lookup.get(exp_id, (None, None))
            if exp_data is None:
                continue
            if exp_data.get("state") != "DRAFT":
                continue
            created = str(exp_data.get("created", ""))
            if _age_hours(created) >= 24:
                ok, out = _run(
                    [sys.executable, os.path.join(SCRIPTS_DIR, "hex-experiment.py"), "baseline", exp_id],
                    dry_run,
                )
                summary["actions"].append({
                    "initiative": init_id, "step": 5, "action": "collect_baseline",
                    "experiment": exp_id, "age_hours": round(_age_hours(created), 1),
                    "success": ok, "output": out,
                })

        # ── Step 6: Ready-for-activation (BASELINE state) ─────────────────────
        for exp_id in (init_data.get("experiments") or []):
            _, exp_data = exp_lookup.get(exp_id, (None, None))
            if exp_data is None:
                continue
            if exp_data.get("state") != "BASELINE":
                continue
            summary["actions"].append({
                "initiative": init_id, "step": 6, "action": "ready_for_activation",
                "experiment": exp_id, "title": exp_data.get("title", ""),
            })
            _emit("experiment.ready_for_activation", {
                "initiative_id": init_id, "experiment_id": exp_id,
                "title": exp_data.get("title", ""),
            }, dry_run=dry_run)

        # ── Step 7: Escalate if horizon <= 14 days with unmet KRs ────────────
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
                    "initiative": init_id, "step": 7, "action": "escalate",
                    "days_remaining": days_left, "unmet_krs": unmet, "message": msg,
                })
                _emit("initiative.at_risk", {
                    "initiative_id": init_id, "days_remaining": days_left,
                    "unmet_krs": unmet, "channel": os.environ.get("HEX_ESCALATION_CHANNEL", "#from-hex"),
                }, dry_run=dry_run)

    return summary


# ── CLI ───────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Run the initiative operating loop for an agent.",
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
