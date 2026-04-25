#!/usr/bin/env python3
"""Quality Antagonist — gaming detector for the BOI initiative loop.

Usage:
  python3 quality-check.py --spec q-774
  python3 quality-check.py --sweep
  python3 quality-check.py --kr init-closed-loop-telemetry/kr-1

Environment variables:
  HEX_WORKSPACE   Path to the hex workspace directory (default: ~/hex)
  BOI_QUEUE_DIR   Path to the BOI queue directory (default: ~/.boi/queue)
  HEX_EVENTS_DIR  Path to hex-events directory (default: ~/.hex-events/events)
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone, timedelta
from pathlib import Path
from typing import Optional

BOI_QUEUE = Path(os.environ.get("BOI_QUEUE_DIR", os.path.expanduser("~/.boi/queue")))
WORKSPACE = Path(os.environ.get("HEX_WORKSPACE", os.path.expanduser("~/hex")))
INITIATIVES_DIR = WORKSPACE / "initiatives"
EVENTS_DIR = Path(os.environ.get("HEX_EVENTS_DIR", os.path.expanduser("~/.hex-events/events")))

# --- Gaming detection patterns ---

TRIVIAL_METRIC_PATTERNS = [
    re.compile(r'^\s*echo\s+[\d.]+\s*$'),           # echo <constant>
    re.compile(r'echo\s+"?UNMEASURABLE', re.I),      # echo "UNMEASURABLE..."
    re.compile(r'exit\s+1'),                          # bare exit 1
    re.compile(r'^\s*echo\s+0\s*$'),                  # echo 0
    re.compile(r'^\s*echo\s+1\s*$'),                  # echo 1
    re.compile(r'^\s*echo\s+100\s*$'),                # echo 100
]

FILE_EXISTENCE_ONLY_PATTERN = re.compile(
    r'os\.path\.exists|test\s+-[ef]|if.*exists', re.I
)

MANUAL_VERIFICATION_PATTERN = re.compile(
    r'manual.verif|manual.check|echo.*manual', re.I
)


def is_trivially_gameable(cmd: str) -> tuple[bool, str]:
    """Return (is_gamed, reason)."""
    if not cmd:
        return False, ""
    cmd_stripped = cmd.strip()
    for pat in TRIVIAL_METRIC_PATTERNS:
        if pat.search(cmd_stripped):
            return True, f"constant/trivial metric command: {cmd_stripped[:80]!r}"
    if MANUAL_VERIFICATION_PATTERN.search(cmd_stripped):
        return True, "manual verification placeholder — not a runnable metric"
    return False, ""


def is_file_existence_proxy(cmd: str) -> bool:
    lines = [l.strip() for l in cmd.splitlines() if l.strip() and not l.strip().startswith('#')]
    non_trivial = [l for l in lines if not l.startswith('score') and 'print' not in l and 'if' not in l]
    return bool(FILE_EXISTENCE_ONLY_PATTERN.search(cmd)) and len(lines) < 15


def kr_lower_better_math_error(kr: dict) -> bool:
    """Detect: lower_is_better but current > target → cannot be met."""
    direction = kr.get("metric", {}).get("direction", "higher_is_better")
    if direction != "lower_is_better":
        return False
    current = kr.get("current")
    target = kr.get("target")
    status = kr.get("status", "open")
    if status != "met":
        return False
    try:
        return float(current) > float(target)
    except (TypeError, ValueError):
        return False


# --- Spec file parsing ---

def read_spec(spec_id: str) -> Optional[dict]:
    """Read and parse a spec file."""
    spec_path = BOI_QUEUE / f"{spec_id}.spec.md"
    if not spec_path.exists():
        return None
    content = spec_path.read_text()
    return {
        "id": spec_id,
        "content": content,
        "path": str(spec_path),
        "mtime": spec_path.stat().st_mtime,
    }


def read_telemetry(spec_id: str) -> Optional[dict]:
    tele_path = BOI_QUEUE / f"{spec_id}.telemetry.json"
    if tele_path.exists():
        try:
            return json.loads(tele_path.read_text())
        except Exception:
            pass
    return None


def spec_is_drive_kr(content: str) -> bool:
    return bool(re.search(r'Drive KR to Non-Zero|drive.*kr.*non-zero|highest-leverage action for kr', content, re.I))


def extract_metric_command_from_spec(content: str) -> Optional[str]:
    """Extract the metric command embedded in a spec (pre-run value)."""
    m = re.search(r'Metric command:\s*```\s*\n(.*?)\n```', content, re.DOTALL)
    if m:
        return m.group(1).strip()
    return None


def get_verify_command(content: str) -> Optional[str]:
    """Extract the verify command from a spec."""
    m = re.search(r'\*\*Verify:\*\*\s*`([^`]+)`', content)
    if m:
        return m.group(1)
    m = re.search(r'\*\*Verify:\*\*\s*(.*?)(?:\n\n|\Z)', content, re.DOTALL)
    if m:
        return m.group(1).strip()
    return None


def get_spec_initiative(content: str) -> Optional[str]:
    """Extract initiative ID from spec content."""
    m = re.search(r'\*\*Initiative:\*\*\s*(\S+)', content)
    if m:
        return m.group(1)
    m = re.search(r'initiative[:\s]+(\S+)', content, re.I)
    if m:
        return m.group(1).rstrip('/')
    return None


def get_spec_kr(content: str) -> Optional[str]:
    """Extract KR ID from spec content."""
    m = re.search(r'\b(kr-\d+)\b', content, re.I)
    if m:
        return m.group(1).lower()
    return None


def get_exit_time(spec_id: str) -> Optional[float]:
    """Get the mtime of the .exit file (completion time)."""
    exit_path = BOI_QUEUE / f"{spec_id}.exit"
    if exit_path.exists():
        return exit_path.stat().st_mtime
    return None


def get_spec_duration_seconds(spec_id: str) -> Optional[float]:
    """Estimate duration from telemetry or file timestamps."""
    tele = read_telemetry(spec_id)
    if tele:
        total = tele.get("total_time_seconds")
        if total and total > 0:
            return total
    prompt_path = BOI_QUEUE / f"{spec_id}.prompt.md"
    exit_path = BOI_QUEUE / f"{spec_id}.exit"
    if prompt_path.exists() and exit_path.exists():
        return exit_path.stat().st_mtime - prompt_path.stat().st_mtime
    return None


# --- KR reading ---

def read_initiative(init_id: str) -> Optional[dict]:
    """Read an initiative YAML file."""
    name = init_id.replace("init-", "")
    for candidate in [init_id, name, f"init-{name}"]:
        path = INITIATIVES_DIR / f"{candidate}.yaml"
        if path.exists():
            try:
                content = path.read_text()
                return {"_raw": content, "_path": str(path), "_id": init_id}
            except Exception:
                pass
    return None


def parse_initiative_yaml(raw: str) -> dict:
    """Minimal YAML parser for initiative files (stdlib only)."""
    try:
        result = {}
        lines = raw.splitlines()
        current_kr = None
        key_results = []
        in_krs = False
        in_metric = False
        metric_lines = []
        metric_indent = 0
        i = 0
        while i < len(lines):
            line = lines[i]
            stripped = line.strip()
            indent = len(line) - len(line.lstrip())

            if stripped.startswith("id:") and not in_krs:
                result["id"] = stripped[3:].strip().strip("'\"")
            elif stripped.startswith("status:") and not in_krs and not current_kr:
                result["status"] = stripped[7:].strip().strip("'\"")
            elif stripped == "key_results:":
                in_krs = True
            elif in_krs and stripped.startswith("- id:"):
                if current_kr:
                    key_results.append(current_kr)
                current_kr = {"id": stripped[5:].strip().strip("'\""), "metric": {}}
            elif in_krs and current_kr and stripped.startswith("description:"):
                current_kr["description"] = stripped[12:].strip().strip("'\"")
            elif in_krs and current_kr and stripped.startswith("target:"):
                try:
                    current_kr["target"] = float(stripped[7:].strip())
                except ValueError:
                    current_kr["target"] = stripped[7:].strip()
            elif in_krs and current_kr and stripped.startswith("current:"):
                try:
                    current_kr["current"] = float(stripped[8:].strip())
                except ValueError:
                    current_kr["current"] = stripped[8:].strip()
            elif in_krs and current_kr and stripped.startswith("status:"):
                current_kr["status"] = stripped[7:].strip().strip("'\"")
            elif in_krs and current_kr and stripped == "metric:":
                in_metric = True
                metric_indent = indent
                current_kr["metric"] = {}
            elif in_metric and stripped.startswith("command:"):
                cmd_start = stripped[8:].strip()
                if cmd_start.startswith("'") or cmd_start.startswith('"'):
                    cmd = cmd_start.lstrip("'\"")
                    current_kr["metric"]["command"] = cmd
                else:
                    current_kr["metric"]["command"] = cmd_start
            elif in_metric and stripped.startswith("direction:"):
                current_kr["metric"]["direction"] = stripped[10:].strip().strip("'\"")
                in_metric = False
            i += 1

        if current_kr:
            key_results.append(current_kr)
        result["key_results"] = key_results
        return result
    except Exception as e:
        return {"_parse_error": str(e), "key_results": []}


def find_kr(init_id: str, kr_id: str) -> Optional[dict]:
    """Load a specific KR from an initiative file."""
    init_data = read_initiative(init_id)
    if not init_data:
        return None
    parsed = parse_initiative_yaml(init_data["_raw"])
    for kr in parsed.get("key_results", []):
        if kr.get("id") == kr_id:
            kr["_initiative_id"] = init_id
            kr["_raw_command"] = ""
            raw = init_data["_raw"]
            kr_block_start = raw.find(f"id: {kr_id}")
            if kr_block_start >= 0:
                block = raw[kr_block_start:]
                cmd_match = re.search(r'command:\s*(.*?)(?:\n\s+direction:|\n\s*target:|\n\s*current:|\Z)', block, re.DOTALL)
                if cmd_match:
                    cmd_raw = cmd_match.group(1).strip().strip("'\"")
                    kr["_raw_command"] = cmd_raw
            return kr
    return None


# --- Gaming analysis for a single spec ---

def analyze_spec(spec_id: str) -> dict:
    """Analyze a single spec for gaming patterns."""
    spec = read_spec(spec_id)
    if not spec:
        return {
            "spec_id": spec_id,
            "verdict": "UNKNOWN",
            "evidence": [f"Spec file not found: {BOI_QUEUE / spec_id}.spec.md"],
            "files_changed": [],
            "metric_changes": [],
            "code_changes": [],
        }

    content = spec["content"]
    evidence = []
    metric_changes = []
    code_changes = []
    gaming_signals = 0
    real_signals = 0

    # 1. Is this a "drive KR to non-zero" initiative spec?
    is_drive_kr = spec_is_drive_kr(content)
    if is_drive_kr:
        evidence.append("spec type: Drive KR to Non-Zero (high-risk template)")
        gaming_signals += 1

    # 2. Check the embedded metric command (pre-run value)
    embedded_metric = extract_metric_command_from_spec(content)
    if embedded_metric:
        is_gamed, reason = is_trivially_gameable(embedded_metric)
        if is_gamed:
            evidence.append(f"embedded metric was trivially gameable: {reason}")
            metric_changes.append({"type": "trivial_metric", "command": embedded_metric[:100]})
            gaming_signals += 2

    # 3. Check the verify command — does it just re-run the metric?
    verify_cmd = get_verify_command(content)
    if verify_cmd and embedded_metric:
        if embedded_metric.strip()[:30] in verify_cmd:
            evidence.append("verify command re-runs same metric — can be gamed by metric rewrite")
            gaming_signals += 1

    # 4. Check if verify is trivially passable
    if verify_cmd:
        is_gamed, reason = is_trivially_gameable(verify_cmd)
        if is_gamed:
            evidence.append(f"verify command is trivially passable: {reason}")
            gaming_signals += 2

    # 5. Duration anomaly
    duration = get_spec_duration_seconds(spec_id)
    if duration is not None:
        if duration < 300 and is_drive_kr:  # <5 minutes for a build task
            evidence.append(f"completed in {duration:.0f}s (<5min) for a build spec")
            gaming_signals += 1
        elif duration > 0:
            evidence.append(f"completion time: {duration:.0f}s")
            if duration > 600:
                real_signals += 1

    # 6. Look at what files changed (git diff HEAD for workspace)
    files_changed = []
    try:
        result = subprocess.run(
            ["git", "diff", "--name-only", "HEAD"],
            cwd=str(WORKSPACE), capture_output=True, text=True, timeout=10
        )
        all_changed = [f.strip() for f in result.stdout.splitlines() if f.strip()]

        exit_time = get_exit_time(spec_id)
        prompt_path = BOI_QUEUE / f"{spec_id}.prompt.md"
        start_time = prompt_path.stat().st_mtime if prompt_path.exists() else None

        for f in all_changed:
            fpath = WORKSPACE / f
            if fpath.exists():
                fmtime = fpath.stat().st_mtime
                if start_time and exit_time:
                    window_start = start_time - 60
                    window_end = exit_time + 1800
                    if window_start <= fmtime <= window_end:
                        files_changed.append(f)
                else:
                    files_changed.append(f)

        yaml_only = all([
            f.endswith('.yaml') or f.endswith('.json') or f.endswith('.toml')
            for f in files_changed
        ]) if files_changed else False

        initiative_yaml_changes = [f for f in files_changed if f.startswith('initiatives/') or f.startswith('experiments/')]
        code_files = [f for f in files_changed if f.endswith(('.py', '.rs', '.sh', '.js', '.ts'))]
        doc_files = [f for f in files_changed if f.endswith('.md')]

        if initiative_yaml_changes and not code_files:
            evidence.append(f"only initiative/experiment YAML files changed: {initiative_yaml_changes}")
            metric_changes.extend([{"type": "yaml_only", "file": f} for f in initiative_yaml_changes])
            gaming_signals += 2
        elif code_files:
            evidence.append(f"real code files changed: {code_files}")
            code_changes.extend([{"type": "code", "file": f} for f in code_files])
            real_signals += len(code_files)
        elif doc_files:
            evidence.append(f"doc files changed: {doc_files}")
            real_signals += 1

    except subprocess.TimeoutExpired:
        evidence.append("git diff timed out — could not check file changes")
    except Exception as e:
        evidence.append(f"git diff error: {e}")

    # 7. Determine initiative and check KR state
    init_id = get_spec_initiative(content)
    kr_id = get_spec_kr(content)
    if init_id and kr_id:
        kr = find_kr(init_id, kr_id)
        if kr:
            if kr_lower_better_math_error(kr):
                evidence.append(
                    f"MATH ERROR: {init_id}/{kr_id} is lower_is_better but "
                    f"current={kr.get('current')} > target={kr.get('target')} yet status=met"
                )
                gaming_signals += 3
            cmd = kr.get("_raw_command", "") or kr.get("metric", {}).get("command", "")
            is_gamed, reason = is_trivially_gameable(cmd)
            if is_gamed:
                evidence.append(f"current metric command in initiative YAML is trivially gameable: {reason}")
                metric_changes.append({"type": "gamed_metric_in_yaml", "kr": f"{init_id}/{kr_id}", "reason": reason})
                gaming_signals += 2
            elif cmd:
                evidence.append(f"metric command looks non-trivial (may be legitimate)")
                real_signals += 1

    # 8. Final verdict
    if gaming_signals >= 4:
        verdict = "GAMING"
    elif gaming_signals >= 2:
        verdict = "SUSPECT"
    elif real_signals >= 2:
        verdict = "LEGITIMATE"
    else:
        verdict = "UNKNOWN"

    return {
        "spec_id": spec_id,
        "verdict": verdict,
        "gaming_signals": gaming_signals,
        "real_signals": real_signals,
        "evidence": evidence,
        "files_changed": files_changed,
        "metric_changes": metric_changes,
        "code_changes": code_changes,
        "is_drive_kr": is_drive_kr,
        "duration_seconds": duration,
    }


# --- Sweep mode ---

def find_completed_specs_last_24h() -> list[str]:
    """Return spec IDs whose .exit files are newer than 24h ago."""
    cutoff = time.time() - (24 * 3600)
    spec_ids = []
    for exit_file in BOI_QUEUE.glob("*.exit"):
        if exit_file.stat().st_mtime >= cutoff:
            spec_id = exit_file.stem  # q-NNN
            spec_file = BOI_QUEUE / f"{spec_id}.spec.md"
            if spec_file.exists():
                spec_ids.append(spec_id)
    spec_ids.sort()
    return spec_ids


def sweep() -> dict:
    """Sweep all specs completed in last 24h and aggregate results."""
    spec_ids = find_completed_specs_last_24h()
    results = []
    gaming = 0
    suspect = 0
    legitimate = 0
    unknown = 0

    for sid in spec_ids:
        r = analyze_spec(sid)
        results.append(r)
        v = r["verdict"]
        if v == "GAMING":
            gaming += 1
            emit_gaming_event(r)
        elif v == "SUSPECT":
            suspect += 1
        elif v == "LEGITIMATE":
            legitimate += 1
        else:
            unknown += 1

    summary = {
        "total": len(spec_ids),
        "gaming": gaming,
        "suspect": suspect,
        "legitimate": legitimate,
        "unknown": unknown,
        "gaming_rate_pct": round(gaming / len(spec_ids) * 100, 1) if spec_ids else 0,
        "sweep_time": datetime.now(timezone.utc).isoformat(),
        "specs": results,
    }
    return summary


def emit_gaming_event(result: dict):
    """Emit hex.quality.gaming.detected event."""
    EVENTS_DIR.mkdir(parents=True, exist_ok=True)
    ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")
    event_file = EVENTS_DIR / f"quality-gaming-{result['spec_id']}-{ts}.json"
    event = {
        "event": "hex.quality.gaming.detected",
        "ts": datetime.now(timezone.utc).isoformat(),
        "spec_id": result["spec_id"],
        "evidence": result["evidence"],
        "gaming_signals": result["gaming_signals"],
    }
    event_file.write_text(json.dumps(event, indent=2))


# --- KR reality check ---

def reality_check_kr(kr_ref: str) -> dict:
    """Reality-check a specific KR: init-foo/kr-N."""
    parts = kr_ref.strip("/").split("/")
    if len(parts) != 2:
        return {"kr_id": kr_ref, "error": "format must be <init-id>/<kr-id>"}

    init_id, kr_id = parts
    kr = find_kr(init_id, kr_id)
    if not kr:
        return {"kr_id": kr_ref, "error": f"KR not found: {init_id}/{kr_id}"}

    claimed_value = kr.get("current")
    claimed_status = kr.get("status", "open")
    target = kr.get("target")
    direction = kr.get("metric", {}).get("direction", "higher_is_better")
    description = kr.get("description", "")
    metric_cmd = kr.get("_raw_command", "") or kr.get("metric", {}).get("command", "")

    evidence = []
    independent_check_value = None
    match = None
    fraud_detected = False

    # 1. Check metric command for gaming
    is_gamed, reason = is_trivially_gameable(metric_cmd)
    if is_gamed:
        evidence.append(f"metric command is trivially gameable: {reason}")
        fraud_detected = True

    # 2. Math error check
    if kr_lower_better_math_error(kr):
        evidence.append(
            f"MATH ERROR: lower_is_better but current={claimed_value} > target={target}, yet status=met"
        )
        fraud_detected = True
        independent_check_value = claimed_value
        match = False

    # 3. If metric command looks real, try to run it independently
    if not is_gamed and metric_cmd and not fraud_detected:
        try:
            run_result = subprocess.run(
                ["bash", "-c", f"cd {WORKSPACE} && {metric_cmd}"],
                capture_output=True, text=True, timeout=30
            )
            output = run_result.stdout.strip()
            if output:
                try:
                    independent_check_value = float(output.split()[-1])
                    tolerance = 0.05
                    if claimed_value is not None:
                        diff = abs(independent_check_value - float(claimed_value))
                        relative_diff = diff / max(abs(float(claimed_value)), 1)
                        match = relative_diff <= tolerance
                        if not match:
                            evidence.append(
                                f"independent measurement {independent_check_value} differs from "
                                f"claimed {claimed_value} by {relative_diff:.1%}"
                            )
                        else:
                            evidence.append(
                                f"independent measurement {independent_check_value} matches claimed {claimed_value}"
                            )
                except (ValueError, IndexError):
                    evidence.append(f"could not parse metric output: {output[:100]!r}")
            else:
                stderr = run_result.stderr.strip()
                evidence.append(f"metric command produced no output (exit={run_result.returncode})")
                if stderr:
                    evidence.append(f"stderr: {stderr[:200]}")
                if run_result.returncode != 0:
                    fraud_detected = True
                    evidence.append("metric command failed — claimed value may be stale/false")
        except subprocess.TimeoutExpired:
            evidence.append("metric command timed out (>30s)")
        except Exception as e:
            evidence.append(f"error running metric: {e}")

    # 4. Is the claimed status consistent with math?
    if claimed_status == "met" and not fraud_detected and independent_check_value is not None:
        try:
            val = float(independent_check_value)
            tgt = float(target)
            if direction == "higher_is_better" and val < tgt:
                evidence.append(f"status=met but independent check {val} < target {tgt}")
                fraud_detected = True
            elif direction == "lower_is_better" and val > tgt:
                evidence.append(f"status=met but independent check {val} > target {tgt}")
                fraud_detected = True
        except (TypeError, ValueError):
            pass

    verdict = "SUSPECT" if fraud_detected else ("VERIFIED" if match else "UNVERIFIED")

    return {
        "kr_id": kr_ref,
        "description": description,
        "claimed_value": claimed_value,
        "claimed_status": claimed_status,
        "target": target,
        "direction": direction,
        "independent_check_value": independent_check_value,
        "match": match,
        "fraud_detected": fraud_detected,
        "verdict": verdict,
        "evidence": evidence,
        "metric_command_preview": metric_cmd[:120] if metric_cmd else None,
    }


# --- CLI ---

def main():
    parser = argparse.ArgumentParser(description="Quality Antagonist — gaming detector")
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--spec", metavar="SPEC_ID", help="Check a specific completed spec")
    group.add_argument("--sweep", action="store_true", help="Sweep all specs completed in last 24h")
    group.add_argument("--kr", metavar="INIT/KR", help="Reality-check a specific KR (e.g. init-foo/kr-1)")
    args = parser.parse_args()

    if args.spec:
        result = analyze_spec(args.spec)
        print(json.dumps(result, indent=2))
    elif args.sweep:
        result = sweep()
        print(json.dumps(result, indent=2))
    elif args.kr:
        result = reality_check_kr(args.kr)
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
