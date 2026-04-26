#!/usr/bin/env python3
"""hex initiative — initiative manager CLI.

Usage:
  hex-initiative.py create <file>
  hex-initiative.py measure <id>
  hex-initiative.py status [id] [--json]
  hex-initiative.py review <id>
  hex-initiative.py list
"""

import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone

import yaml

HEX_ROOT = os.environ.get("HEX_ROOT", os.path.expanduser("${AGENT_DIR:-$HOME/hex}"))
INITIATIVES_DIR = os.path.join(HEX_ROOT, "initiatives")
EXPERIMENTS_DIR = os.path.join(HEX_ROOT, "experiments")

# ── telemetry ─────────────────────────────────────────────────────────────────

def _emit(event_type: str, payload: dict) -> None:
    telemetry_path = os.path.join(HEX_ROOT, ".hex", "telemetry")
    sys.path.insert(0, telemetry_path)
    try:
        from emit import emit
        emit(event_type, payload, source="hex-initiative")
    except Exception as exc:
        print(f"[hex-initiative] telemetry warn: {exc}", file=sys.stderr)

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

def _acquire_lock(init_id: str) -> str:
    lock_path = os.path.join(INITIATIVES_DIR, f"{init_id}.lock")
    deadline = time.time() + 10
    while time.time() < deadline:
        try:
            fd = os.open(lock_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
            os.write(fd, str(os.getpid()).encode())
            os.close(fd)
            return lock_path
        except FileExistsError:
            time.sleep(0.5)
    print(f"ERROR: lock timeout for {init_id}", file=sys.stderr)
    sys.exit(3)

def _release_lock(lock_path: str) -> None:
    try:
        os.unlink(lock_path)
    except FileNotFoundError:
        pass

# ── helpers ───────────────────────────────────────────────────────────────────

def _find_init_file(init_id: str) -> str:
    os.makedirs(INITIATIVES_DIR, exist_ok=True)
    for name in os.listdir(INITIATIVES_DIR):
        if name.endswith(".lock") or not name.endswith(".yaml"):
            continue
        if name.startswith(init_id) or name == f"{init_id}.yaml":
            return os.path.join(INITIATIVES_DIR, name)
        # also match by id field prefix
        try:
            data = _load(os.path.join(INITIATIVES_DIR, name))
            if data.get("id") == init_id:
                return os.path.join(INITIATIVES_DIR, name)
        except Exception:
            continue
    print(f"ERROR: initiative not found: {init_id}", file=sys.stderr)
    sys.exit(1)

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

def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")

def _today() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%d")

def _days_until(date_str: str) -> int:
    from datetime import date
    try:
        target = datetime.strptime(str(date_str), "%Y-%m-%d").date()
        return (target - date.today()).days
    except (ValueError, TypeError):
        return 9999

def _list_init_files() -> list[str]:
    os.makedirs(INITIATIVES_DIR, exist_ok=True)
    return sorted(
        f for f in os.listdir(INITIATIVES_DIR)
        if f.endswith(".yaml") and not f.endswith(".lock")
    )

# ── validation ────────────────────────────────────────────────────────────────

REQUIRED_FIELDS = ["id", "goal", "owner", "horizon"]

def _validate(data: dict) -> list[str]:
    errors = []
    for f in REQUIRED_FIELDS:
        if not data.get(f):
            errors.append(f"missing required field: {f}")
    if data.get("horizon"):
        try:
            from datetime import date
            d = str(data["horizon"])
            datetime.strptime(d, "%Y-%m-%d")
        except ValueError:
            errors.append("horizon must be YYYY-MM-DD")
    key_results = data.get("key_results") or []
    for i, kr in enumerate(key_results):
        if not kr.get("description"):
            errors.append(f"key_results[{i}] missing description")
        metric = kr.get("metric") or {}
        if not metric.get("command"):
            errors.append(f"key_results[{i}] missing metric.command")
        if not metric.get("direction"):
            errors.append(f"key_results[{i}] missing metric.direction")
    return errors

# ── commands ──────────────────────────────────────────────────────────────────

def cmd_create(args: list[str]) -> int:
    if not args:
        print("Usage: hex-initiative.py create <file>", file=sys.stderr)
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
    os.makedirs(INITIATIVES_DIR, exist_ok=True)
    init_id = data["id"]
    dest = os.path.join(INITIATIVES_DIR, f"{init_id}.yaml")
    data.setdefault("status", "active")
    data.setdefault("created", _today())
    data.setdefault("experiments", [])
    data.setdefault("specs", [])
    data.setdefault("next_step", None)
    data.setdefault("review_cadence", "weekly")
    data.setdefault("last_reviewed", None)
    for kr in (data.get("key_results") or []):
        kr.setdefault("current", None)
        kr.setdefault("measured_at", None)
        kr.setdefault("status", "open")
    _save(data, dest)
    print(f"→ {dest} written (status: {data['status']})")
    _emit("initiative.created", {"id": init_id, "goal": data.get("goal", ""), "owner": data.get("owner", "")})
    return 0

def cmd_measure(args: list[str]) -> int:
    if not args:
        print("Usage: hex-initiative.py measure <id>", file=sys.stderr)
        return 1
    init_id = args[0]
    path = _find_init_file(init_id)
    lock = _acquire_lock(init_id)
    try:
        data = _load(path)
        key_results = data.get("key_results") or []
        if not key_results:
            print("No key results defined.")
            return 0
        any_met = False
        for kr in key_results:
            kr_id = kr.get("id", "?")
            metric = kr.get("metric") or {}
            cmd = metric.get("command")
            if not cmd:
                print(f"  {kr_id}: no metric command, skipping")
                continue
            print(f"Measuring {kr_id}: {kr.get('description', '')[:60]}")
            try:
                val = _run_metric(cmd)
                kr["current"] = val
                kr["measured_at"] = _now_iso()
                direction = metric.get("direction", "lower_is_better")
                target = kr.get("target")
                if target is not None:
                    if direction == "lower_is_better":
                        met = val <= float(target)
                    else:
                        met = val >= float(target)
                    if met and kr.get("status") != "met":
                        kr["status"] = "met"
                        any_met = True
                        print(f"  → {val} ✓ MET (target: {target})")
                        _emit("initiative.kr_met", {
                            "initiative_id": init_id,
                            "kr_id": kr_id,
                            "value": val,
                            "target": target,
                        })
                    else:
                        print(f"  → {val} (target: {target}, {'↓' if direction == 'lower_is_better' else '↑'} {target})")
                else:
                    print(f"  → {val}")
            except Exception as exc:
                print(f"  ERROR: {exc}")
        horizon = data.get("horizon")
        if horizon:
            days = _days_until(horizon)
            open_krs = [kr for kr in key_results if kr.get("status") != "met"]
            if days <= 7 and open_krs:
                print(f"\nWARNING: horizon {horizon} is {days} days away with {len(open_krs)} KR(s) unmet")
                _emit("initiative.at_risk", {
                    "initiative_id": init_id,
                    "horizon": str(horizon),
                    "days_remaining": days,
                    "unmet_kr_count": len(open_krs),
                })
        _save(data, path)
        print(f"\nMeasurement complete for {init_id}")
        _emit("initiative.measured", {"id": init_id})
    finally:
        _release_lock(lock)
    return 0

def cmd_status(args: list[str]) -> int:
    as_json = "--json" in args
    args = [a for a in args if a != "--json"]
    os.makedirs(INITIATIVES_DIR, exist_ok=True)

    if args:
        init_id = args[0]
        path = _find_init_file(init_id)
        data = _load(path)
        if as_json:
            print(json.dumps(data, default=str, indent=2))
        else:
            print(yaml.dump(data, default_flow_style=False, allow_unicode=True, sort_keys=False))
        return 0

    files = _list_init_files()
    if not files:
        print("No initiatives found.")
        return 0

    if as_json:
        results = []
        for fname in files:
            d = _load(os.path.join(INITIATIVES_DIR, fname))
            results.append(d)
        print(json.dumps(results, default=str, indent=2))
        return 0

    print(f"{'ID':<30} {'OWNER':<16} {'STATUS':<12} {'HORIZON':<12} {'KR PROGRESS'}")
    print("─" * 90)
    for fname in files:
        d = _load(os.path.join(INITIATIVES_DIR, fname))
        init_id = d.get("id", fname.replace(".yaml", ""))
        owner = (d.get("owner") or "")[:14]
        status = d.get("status", "unknown")
        horizon = str(d.get("horizon", ""))
        krs = d.get("key_results") or []
        met = sum(1 for kr in krs if kr.get("status") == "met")
        total = len(krs)
        kr_prog = f"{met}/{total}" if total else "—"
        days = _days_until(horizon) if horizon else 9999
        horizon_label = horizon
        if days <= 7:
            horizon_label = f"{horizon} (!)"
        print(f"{init_id:<30} {owner:<16} {status:<12} {horizon_label:<12} {kr_prog}")
    return 0

def cmd_review(args: list[str]) -> int:
    if not args:
        print("Usage: hex-initiative.py review <id>", file=sys.stderr)
        return 1
    init_id = args[0]
    path = _find_init_file(init_id)
    data = _load(path)

    width = 70
    print("=" * width)
    print(f"INITIATIVE REVIEW: {init_id}")
    print("=" * width)
    print(f"Goal:    {data.get('goal', '')}")
    print(f"Owner:   {data.get('owner', '')}")
    print(f"Status:  {data.get('status', '')}")
    horizon = data.get("horizon")
    if horizon:
        days = _days_until(horizon)
        print(f"Horizon: {horizon} ({days} days remaining)")
    print()

    krs = data.get("key_results") or []
    print(f"KEY RESULTS ({len(krs)} total):")
    print("─" * width)
    met_krs = []
    unmet_krs = []
    for kr in krs:
        kr_id = kr.get("id", "?")
        desc = kr.get("description", "")
        current = kr.get("current")
        target = kr.get("target")
        kr_status = kr.get("status", "open")
        measured_at = kr.get("measured_at", "never")
        icon = "✓" if kr_status == "met" else "✗"
        print(f"  {icon} {kr_id}: {desc}")
        print(f"    current={current}  target={target}  measured={measured_at}")
        if kr_status == "met":
            met_krs.append(kr_id)
        else:
            unmet_krs.append(kr_id)
    print()

    experiments = data.get("experiments") or []
    print(f"EXPERIMENTS ({len(experiments)} linked):")
    print("─" * width)
    if experiments:
        for exp_id in experiments:
            # Try to load from experiments dir
            exp_path = None
            for fname in os.listdir(EXPERIMENTS_DIR) if os.path.exists(EXPERIMENTS_DIR) else []:
                if fname.startswith(exp_id) and fname.endswith(".yaml"):
                    exp_path = os.path.join(EXPERIMENTS_DIR, fname)
                    break
            if exp_path:
                try:
                    exp = _load(exp_path)
                    state = exp.get("state", "UNKNOWN")
                    title = exp.get("title", "")[:45]
                    print(f"  [{state:<22}] {exp_id}: {title}")
                except Exception:
                    print(f"  [UNREADABLE      ] {exp_id}")
            else:
                print(f"  [NOT FOUND        ] {exp_id}")
    else:
        print("  (none)")
    print()

    specs = data.get("specs") or []
    print(f"SPECS IN FLIGHT ({len(specs)}):")
    print("─" * width)
    if specs:
        for spec in specs:
            spec_id = spec.get("id", "?")
            role = spec.get("role", "")[:45]
            spec_status = spec.get("status", "pending")
            print(f"  [{spec_status:<10}] {spec_id}: {role}")
    else:
        print("  (none)")
    print()

    print("NEXT STEPS:")
    print("─" * width)
    if unmet_krs:
        print(f"  Unmet KRs: {', '.join(unmet_krs)}")
    if met_krs:
        print(f"  Met KRs: {', '.join(met_krs)}")
    active_exps = []
    if experiments:
        for exp_id in experiments:
            for fname in os.listdir(EXPERIMENTS_DIR) if os.path.exists(EXPERIMENTS_DIR) else []:
                if fname.startswith(exp_id) and fname.endswith(".yaml"):
                    try:
                        exp = _load(os.path.join(EXPERIMENTS_DIR, fname))
                        if exp.get("state") in ("DRAFT", "BASELINE", "ACTIVE", "MEASURING"):
                            active_exps.append(exp_id)
                    except Exception:
                        pass
                    break
    if unmet_krs and not active_exps:
        print(f"  ACTION: No active experiments for unmet KRs — propose new experiment")
    next_step = data.get("next_step")
    if next_step:
        print(f"\n  Documented next step:")
        for line in str(next_step).strip().splitlines():
            print(f"    {line}")
    if horizon:
        days = _days_until(horizon)
        if days <= 7 and unmet_krs:
            print(f"\n  ⚠ HORIZON AT RISK: {days} days left, {len(unmet_krs)} KR(s) unmet — escalate")
    print()
    print("=" * width)

    _emit("initiative.reviewed", {"id": init_id, "met_krs": met_krs, "unmet_krs": unmet_krs})
    return 0

def cmd_list(args: list[str]) -> int:
    return cmd_status(args)

# ── main ──────────────────────────────────────────────────────────────────────

COMMANDS = {
    "create": cmd_create,
    "measure": cmd_measure,
    "status": cmd_status,
    "review": cmd_review,
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
