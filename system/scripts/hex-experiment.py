#!/usr/bin/env python3
"""hex experiment — experiment lifecycle CLI.

Usage:
  hex-experiment.py create <file>
  hex-experiment.py baseline <id>
  hex-experiment.py activate <id>
  hex-experiment.py measure <id>
  hex-experiment.py verdict <id>
  hex-experiment.py status [id] [--json]
  hex-experiment.py list
"""

import hashlib
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone

import yaml

HEX_ROOT = os.environ.get("HEX_ROOT", os.path.expanduser("${HEX_DIR:-$HOME/hex}"))
EXPERIMENTS_DIR = os.path.join(HEX_ROOT, "experiments")

# ── telemetry ─────────────────────────────────────────────────────────────────

def _emit(event_type: str, payload: dict) -> None:
    telemetry_path = os.path.join(HEX_ROOT, ".hex", "telemetry")
    sys.path.insert(0, telemetry_path)
    try:
        from emit import emit
        emit(event_type, payload, source="hex-experiment")
    except Exception as exc:
        print(f"[hex-experiment] telemetry warn: {exc}", file=sys.stderr)

# ── YAML I/O ──────────────────────────────────────────────────────────────────

def _load(path: str) -> dict:
    with open(path, "r", encoding="utf-8") as fh:
        return yaml.safe_load(fh)

def _save(data: dict, path: str) -> None:
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        yaml.dump(data, fh, default_flow_style=False, allow_unicode=True,
                  sort_keys=False)
    os.replace(tmp, path)

# ── lock ──────────────────────────────────────────────────────────────────────

def _acquire_lock(exp_id: str) -> str:
    lock_path = os.path.join(EXPERIMENTS_DIR, f"{exp_id}.lock")
    deadline = time.time() + 10
    while time.time() < deadline:
        try:
            fd = os.open(lock_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
            os.write(fd, str(os.getpid()).encode())
            os.close(fd)
            return lock_path
        except FileExistsError:
            time.sleep(0.5)
    print(f"ERROR: lock timeout for {exp_id}", file=sys.stderr)
    sys.exit(3)

def _release_lock(lock_path: str) -> None:
    try:
        os.unlink(lock_path)
    except FileNotFoundError:
        pass

# ── helpers ───────────────────────────────────────────────────────────────────

def _find_exp_file(exp_id: str) -> str:
    os.makedirs(EXPERIMENTS_DIR, exist_ok=True)
    for name in os.listdir(EXPERIMENTS_DIR):
        if name.startswith(exp_id) and name.endswith(".yaml"):
            return os.path.join(EXPERIMENTS_DIR, name)
    print(f"ERROR: experiment not found: {exp_id}", file=sys.stderr)
    sys.exit(1)

def _next_id() -> str:
    os.makedirs(EXPERIMENTS_DIR, exist_ok=True)
    nums = []
    for name in os.listdir(EXPERIMENTS_DIR):
        if name.startswith("exp-") and name.endswith(".yaml"):
            try:
                nums.append(int(name[4:7]))
            except ValueError:
                pass
    return f"exp-{(max(nums) + 1 if nums else 1):03d}"

def _slugify(title: str) -> str:
    slug = title.lower()
    slug = "".join(c if c.isalnum() else "-" for c in slug)
    slug = "-".join(p for p in slug.split("-") if p)
    return slug[:40]

def _run_metric(command: str) -> float:
    result = subprocess.run(
        ["bash", "-c", command],
        capture_output=True, text=True, timeout=60
    )
    if result.returncode != 0:
        raise RuntimeError(f"command exited {result.returncode}: {result.stderr.strip()}")
    raw = result.stdout.strip()
    if not raw:
        raise RuntimeError("command returned empty output")
    try:
        return float(raw)
    except ValueError:
        raise RuntimeError(f"non-numeric output: {raw!r}")

def _compute_sha(data: dict) -> str:
    block = {
        "hypothesis": data.get("hypothesis", ""),
        "success_criteria": data.get("success_criteria", {}),
    }
    encoded = json.dumps(block, sort_keys=True, ensure_ascii=True)
    return hashlib.sha256(encoded.encode()).hexdigest()

def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")

def _today() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%d")

# ── validation ────────────────────────────────────────────────────────────────

REQUIRED_FIELDS = ["title", "hypothesis", "owner", "metrics", "success_criteria", "rollback_plan"]

def _validate(data: dict) -> list[str]:
    errors = []
    for f in REQUIRED_FIELDS:
        if not data.get(f):
            errors.append(f"missing required field: {f}")
    if "metrics" in data:
        primary = (data["metrics"] or {}).get("primary", {})
        if not primary.get("command"):
            errors.append("metrics.primary.command is required")
    if data.get("baseline_locked_sha") or data.get("baseline") or data.get("post_change") or data.get("verdict"):
        errors.append("create rejects pre-filled baseline/post_change/verdict fields")
    tb = data.get("time_bound", {}) or {}
    if "measure_by" in tb and tb["measure_by"]:
        try:
            from datetime import date
            d = str(tb["measure_by"])
            parsed = datetime.strptime(d, "%Y-%m-%d").date()
            if parsed <= date.today():
                errors.append("time_bound.measure_by must be in the future")
        except ValueError:
            errors.append("time_bound.measure_by must be YYYY-MM-DD")
    return errors

# ── commands ──────────────────────────────────────────────────────────────────

def cmd_create(args: list[str]) -> int:
    if not args:
        print("Usage: hex-experiment.py create <file>", file=sys.stderr)
        return 1
    src = args[0]
    if not os.path.exists(src):
        print(f"ERROR: file not found: {src}", file=sys.stderr)
        return 1
    data = _load(src)
    errors = _validate(data)
    if errors:
        for e in errors:
            print(f"  INVALID: {e}")
        return 1
    exp_id = _next_id()
    data["id"] = exp_id
    data["state"] = "DRAFT"
    data.setdefault("created", _today())
    data.setdefault("baseline_locked_sha", None)
    data.setdefault("baseline", None)
    data.setdefault("post_change", None)
    data.setdefault("verdict", None)
    slug = _slugify(data.get("title", "experiment"))
    dest = os.path.join(EXPERIMENTS_DIR, f"{exp_id}-{slug}.yaml")
    _save(data, dest)
    print(f"→ {dest} written (state: DRAFT)")
    _emit("experiment.created", {"id": exp_id, "title": data.get("title", "")})
    return 0

def cmd_baseline(args: list[str]) -> int:
    if not args:
        print("Usage: hex-experiment.py baseline <id>", file=sys.stderr)
        return 1
    exp_id = args[0]
    path = _find_exp_file(exp_id)
    lock = _acquire_lock(exp_id)
    try:
        data = _load(path)
        if data.get("state") != "DRAFT":
            print(f"ERROR: experiment is {data.get('state')}, expected DRAFT", file=sys.stderr)
            return 1
        if data.get("baseline"):
            print("ERROR: baseline already collected — create a new experiment to re-baseline", file=sys.stderr)
            return 1
        values = {}
        metrics = data.get("metrics", {}) or {}
        primary = metrics.get("primary", {}) or {}
        print(f"Running primary metric: {primary.get('name')}")
        val = _run_metric(primary["command"])
        values[primary["name"]] = val
        print(f"  → {val}")
        for g in (metrics.get("guardrails") or []):
            print(f"Running guardrail: {g['name']}")
            val = _run_metric(g["command"])
            values[g["name"]] = val
            print(f"  → {val}")
        sha = _compute_sha(data)
        data["baseline_locked_sha"] = sha
        data["baseline"] = {
            "collected_at": _now_iso(),
            "values": values,
        }
        data["state"] = "BASELINE"
        _save(data, path)
        print(f"Baseline locked. SHA: {sha[:8]}...")
        print("State: BASELINE")
        _emit("experiment.baseline_collected", {"id": exp_id, "sha": sha})
    finally:
        _release_lock(lock)
    return 0

def cmd_activate(args: list[str]) -> int:
    if not args:
        print("Usage: hex-experiment.py activate <id>", file=sys.stderr)
        return 1
    exp_id = args[0]
    path = _find_exp_file(exp_id)
    lock = _acquire_lock(exp_id)
    try:
        data = _load(path)
        if data.get("state") != "BASELINE":
            print(f"ERROR: experiment is {data.get('state')}, expected BASELINE", file=sys.stderr)
            return 1
        commit = ""
        try:
            result = subprocess.run(
                ["git", "-C", HEX_ROOT, "rev-parse", "HEAD"],
                capture_output=True, text=True, timeout=5
            )
            commit = result.stdout.strip()
        except Exception:
            commit = "unknown"
        data["activated_commit"] = commit
        data["activated_at"] = _now_iso()
        data["state"] = "ACTIVE"
        _save(data, path)
        tb = (data.get("time_bound") or {})
        measure_by = tb.get("measure_by", "")
        print(f"Records commit: {commit[:8]}...")
        print("State: ACTIVE")
        print(f"→ hex-events: experiment.activated (id: {exp_id}, measure_by: {measure_by})")
        _emit("experiment.activated", {
            "id": exp_id,
            "title": data.get("title", ""),
            "measure_by": str(measure_by),
            "activated_commit": commit,
        })
    finally:
        _release_lock(lock)
    return 0

def cmd_measure(args: list[str]) -> int:
    if not args:
        print("Usage: hex-experiment.py measure <id>", file=sys.stderr)
        return 1
    exp_id = args[0]
    path = _find_exp_file(exp_id)
    lock = _acquire_lock(exp_id)
    try:
        data = _load(path)
        if data.get("state") not in ("ACTIVE",):
            print(f"ERROR: experiment is {data.get('state')}, expected ACTIVE", file=sys.stderr)
            return 1
        current_sha = _compute_sha(data)
        if current_sha != data.get("baseline_locked_sha"):
            print("ERROR: hypothesis or success_criteria modified after baseline (SHA mismatch)", file=sys.stderr)
            return 3
        values = {}
        metrics = data.get("metrics", {}) or {}
        primary = metrics.get("primary", {}) or {}
        print(f"Running primary metric: {primary.get('name')}")
        val = _run_metric(primary["command"])
        values[primary["name"]] = val
        print(f"  → {val}")
        for g in (metrics.get("guardrails") or []):
            print(f"Running guardrail: {g['name']}")
            val = _run_metric(g["command"])
            values[g["name"]] = val
            print(f"  → {val}")
        data["post_change"] = {
            "collected_at": _now_iso(),
            "values": values,
        }
        data["state"] = "MEASURING"
        _save(data, path)
        print("State: MEASURING")
        _emit("experiment.measured", {"id": exp_id})
    finally:
        _release_lock(lock)
    return 0

def cmd_verdict(args: list[str]) -> int:
    if not args:
        print("Usage: hex-experiment.py verdict <id>", file=sys.stderr)
        return 1
    exp_id = args[0]
    path = _find_exp_file(exp_id)
    lock = _acquire_lock(exp_id)
    try:
        data = _load(path)
        state = data.get("state")
        if state in ("VERDICT_PASS", "VERDICT_FAIL", "VERDICT_INCONCLUSIVE"):
            print(f"Already in terminal state: {state}")
            return 0 if state == "VERDICT_PASS" else (1 if state == "VERDICT_FAIL" else 2)

        # Check time_bound expiry — even if ACTIVE (no post_change yet)
        tb = (data.get("time_bound") or {})
        measure_by = tb.get("measure_by")
        if measure_by and not data.get("post_change"):
            from datetime import date
            mb = datetime.strptime(str(measure_by), "%Y-%m-%d").date()
            if date.today() > mb:
                data["state"] = "VERDICT_INCONCLUSIVE"
                data["verdict"] = {
                    "rendered_at": _now_iso(),
                    "result": "inconclusive",
                    "reason": f"measure_by date {measure_by} exceeded with no post_change data",
                }
                _save(data, path)
                print(f"VERDICT: INCONCLUSIVE (measure_by {measure_by} exceeded)")
                _emit("experiment.verdict", {"id": exp_id, "result": "inconclusive"})
                return 2

        if state != "MEASURING":
            print(f"ERROR: experiment is {state}, expected MEASURING", file=sys.stderr)
            return 1

        metrics = data.get("metrics", {}) or {}
        primary = metrics.get("primary", {}) or {}
        criteria = data.get("success_criteria", {}) or {}
        baseline_vals = (data.get("baseline") or {}).get("values", {})
        post_vals = (data.get("post_change") or {}).get("values", {})
        primary_name = primary.get("name")
        direction = primary.get("direction", "lower_is_better")

        pre = baseline_vals.get(primary_name)
        post = post_vals.get(primary_name)

        # Compute primary delta (sign: positive = improvement)
        if direction == "lower_is_better":
            delta_pct = (pre - post) / abs(pre) * 100 if pre else 0
        else:
            delta_pct = (post - pre) / abs(pre) * 100 if pre else 0

        primary_criteria = (criteria.get("primary") or {})
        threshold_pct = float(primary_criteria.get("threshold_pct", 0))
        primary_pass = delta_pct >= threshold_pct

        # Print primary result
        width = 55
        print("EXPERIMENT VERDICT:", exp_id)
        print("─" * width)
        print(f"Title:    {data.get('title', '')}")
        print()
        direction_label = "(lower_is_better)" if direction == "lower_is_better" else "(higher_is_better)"
        print(f"PRIMARY METRIC: {primary_name} {direction_label}")
        print(f"  Baseline:    {pre}")
        print(f"  Post-change: {post}")
        print(f"  Delta:       {delta_pct:+.1f}% improvement")
        print(f"  Threshold:   {threshold_pct:+.1f}% required")
        print(f"  Result:      {'✓ PASS' if primary_pass else '✗ FAIL'}")

        guardrail_results = []
        all_guardrails_pass = True
        for g in (metrics.get("guardrails") or []):
            gname = g["name"]
            gdir = g.get("direction", "lower_is_better")
            gpre = baseline_vals.get(gname)
            gpost = post_vals.get(gname)
            if gdir == "lower_is_better":
                greg_pct = (gpost - gpre) / abs(gpre) * 100 if gpre else 0
            else:
                greg_pct = (gpre - gpost) / abs(gpre) * 100 if gpre else 0
            max_reg = float(g.get("max_regression_pct", 0))
            g_pass = greg_pct <= max_reg
            if not g_pass:
                all_guardrails_pass = False
            print()
            print(f"GUARDRAIL: {gname} ({gdir})")
            print(f"  Baseline:    {gpre}")
            print(f"  Post-change: {gpost}")
            print(f"  Regression:  {greg_pct:+.1f}% (max allowed: +{max_reg:.1f}%)")
            print(f"  Result:      {'✓ PASS' if g_pass else '✗ FAIL'}")
            guardrail_results.append({"name": gname, "pass": g_pass, "regression_pct": greg_pct})

        print()
        print("─" * width)

        if primary_pass and all_guardrails_pass:
            final_state = "VERDICT_PASS"
            result_str = "pass"
            print("VERDICT: ✓ PASS")
            exit_code = 0
        else:
            final_state = "VERDICT_FAIL"
            result_str = "fail"
            print("VERDICT: ✗ FAIL")
            if not primary_pass:
                print(f"\nPrimary metric did not meet threshold.")
                print(f"  Required: {threshold_pct:+.1f}% improvement")
                print(f"  Achieved: {delta_pct:+.1f}% improvement")
            if not all_guardrails_pass:
                print(f"\nOne or more guardrails failed.")
            rollback = (data.get("rollback_plan") or {})
            cmds = rollback.get("commands") or []
            if cmds:
                print(f"\nRollback plan:")
                for c in cmds:
                    print(f"  {c}")
                print("\nRun these commands if you want to revert the change.")
            exit_code = 1

        data["state"] = final_state
        data["verdict"] = {
            "rendered_at": _now_iso(),
            "result": result_str,
            "primary_delta_pct": round(delta_pct, 2),
            "guardrail_results": guardrail_results,
        }
        _save(data, path)
        _emit("experiment.verdict", {"id": exp_id, "result": result_str, "delta_pct": round(delta_pct, 2)})
        if final_state == "VERDICT_FAIL":
            _emit("experiment.verdict_fail", {
                "id": exp_id,
                "title": data.get("title", ""),
                "hypothesis": data.get("hypothesis", ""),
                "primary_delta_pct": round(delta_pct, 2),
                "threshold_pct": threshold_pct,
                "rollback_commands": (data.get("rollback_plan") or {}).get("commands", []),
            })
        return exit_code
    finally:
        _release_lock(lock)

def cmd_status(args: list[str]) -> int:
    as_json = "--json" in args
    args = [a for a in args if a != "--json"]
    os.makedirs(EXPERIMENTS_DIR, exist_ok=True)

    if args:
        exp_id = args[0]
        path = _find_exp_file(exp_id)
        data = _load(path)
        if as_json:
            print(json.dumps(data, default=str, indent=2))
        else:
            print(yaml.dump(data, default_flow_style=False, allow_unicode=True, sort_keys=False))
        return 0

    files = sorted(f for f in os.listdir(EXPERIMENTS_DIR)
                   if f.startswith("exp-") and f.endswith(".yaml"))
    if not files:
        print("No experiments found.")
        return 0

    if as_json:
        results = []
        for fname in files:
            d = _load(os.path.join(EXPERIMENTS_DIR, fname))
            results.append(d)
        print(json.dumps(results, default=str, indent=2))
        return 0

    print(f"{'ID':<12} {'TITLE':<38} {'STATE':<22} {'PRIMARY DELTA'}")
    print("─" * 90)
    for fname in files:
        d = _load(os.path.join(EXPERIMENTS_DIR, fname))
        exp_id = d.get("id", fname[:7])
        title = (d.get("title") or "")[:36]
        state = d.get("state", "UNKNOWN")
        v = d.get("verdict") or {}
        delta = f"{v.get('primary_delta_pct', ''):+.1f}%" if v.get("primary_delta_pct") is not None else "(not measured)"
        print(f"{exp_id:<12} {title:<38} {state:<22} {delta}")
    return 0

def cmd_list(args: list[str]) -> int:
    return cmd_status(args)

# ── main ──────────────────────────────────────────────────────────────────────

COMMANDS = {
    "create": cmd_create,
    "baseline": cmd_baseline,
    "activate": cmd_activate,
    "measure": cmd_measure,
    "verdict": cmd_verdict,
    "status": cmd_status,
    "list": cmd_list,
}

def main() -> int:
    argv = sys.argv[1:]
    if not argv:
        print(__doc__.strip())
        return 1
    sub = argv[0]
    if sub not in COMMANDS:
        print(f"Unknown subcommand: {sub}", file=sys.stderr)
        print(f"Available: {', '.join(COMMANDS)}", file=sys.stderr)
        return 1
    return COMMANDS[sub](argv[1:])

if __name__ == "__main__":
    sys.exit(main())
