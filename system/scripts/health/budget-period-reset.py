#!/usr/bin/env python3
"""budget-period-reset.py — Auto-reset agent budget periods with tiered safety gate.

Critic-revised 2026-05-05 (T3DED HIGH severity):
  - Zero-budget guard: agents with budget_usd=0 are skipped (no divide-by-zero)
  - Tiered gate replaces binary 5x check:
      ratio ≤ 1.0       → auto-reset (within budget)
      1.0 < ratio ≤ 2.0 → auto-reset with audit log entry
      2.0 < ratio ≤ 5.0 → blocked, emit WARN + digest to configured Slack channel
      ratio > 5.0        → blocked, emit CRITICAL
  - Period length read from charter.yaml budget.period_days (default 7d)
  - Reset appended to agent's own trail so agent sees it on next wake
  - All resets and blocks logged to .hex/audit/actions.jsonl
"""
import fcntl
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

_HEX_DIR = Path(os.environ.get("HEX_DIR", str(Path.home() / "hex")))
PROJECTS = _HEX_DIR / "projects"
AUDIT_DIR = _HEX_DIR / ".hex" / "audit"
ACTIONS_LOG = AUDIT_DIR / "actions.jsonl"
HEX_ALERT = _HEX_DIR / ".hex" / "scripts" / "hex-alert.sh"
HEX_EMIT = Path.home() / ".hex-events/hex_emit.py"
DEFAULT_PERIOD_DAYS = 7
SOURCE = "budget-period-reset"

NOW = datetime.now(timezone.utc)
NOW_ISO = NOW.isoformat()


def log_action(agent_id: str, action: str, detail: dict) -> None:
    entry = json.dumps({"ts": NOW_ISO, "agent": agent_id, "action": action, "detail": detail})
    lock_path = ACTIONS_LOG.with_suffix(".jsonl.lock")
    try:
        with open(lock_path, "w") as lf:
            fcntl.flock(lf, fcntl.LOCK_EX)
            try:
                AUDIT_DIR.mkdir(parents=True, exist_ok=True)
                with open(ACTIONS_LOG, "a") as f:
                    f.write(entry + "\n")
            finally:
                fcntl.flock(lf, fcntl.LOCK_UN)
    except Exception as e:
        print(f"[{agent_id}] WARN: failed to write audit log: {e}", file=sys.stderr)


def emit_alert(severity: str, message: str) -> None:
    if not HEX_ALERT.exists():
        print(f"[ALERT-FALLBACK] {severity} {SOURCE}: {message}", file=sys.stderr)
        return
    try:
        subprocess.run(
            [str(HEX_ALERT), severity, SOURCE, message],
            timeout=30,
            capture_output=True,
        )
    except Exception as e:
        print(f"[{SOURCE}] hex-alert.sh failed: {e}", file=sys.stderr)


def emit_event(event_type: str, payload: dict) -> None:
    if not HEX_EMIT.exists():
        return
    try:
        subprocess.run(
            ["python3", str(HEX_EMIT), event_type, json.dumps(payload), f"hex:{SOURCE}"],
            timeout=15,
            capture_output=True,
        )
    except Exception:
        pass


def read_charter_period_days(project_dir: Path) -> int:
    charter_path = project_dir / "charter.yaml"
    if not charter_path.exists():
        return DEFAULT_PERIOD_DAYS
    try:
        content = charter_path.read_text()
        # Parse budget.period_days without yaml dependency
        in_budget = False
        for line in content.splitlines():
            stripped = line.strip()
            if stripped == "budget:" or stripped.startswith("budget:"):
                in_budget = True
                continue
            if in_budget:
                if stripped.startswith("period_days:"):
                    val = stripped.split(":", 1)[1].strip()
                    return int(val)
                # Exit budget block on next top-level key (no indentation)
                if line and not line[0].isspace() and ":" in line:
                    break
    except Exception:
        pass
    return DEFAULT_PERIOD_DAYS


def append_trail_entry(state: dict, agent_id: str, detail: dict) -> dict:
    trail = state.get("trail", [])
    trail.append({
        "ts": NOW_ISO,
        "type": "budget_reset",
        "detail": detail,
        "queue_item": None,
    })
    state["trail"] = trail
    return state


def process_agent(project_dir: Path) -> str:
    """Process one agent. Returns status string for summary."""
    agent_id = project_dir.name
    state_path = project_dir / "state.json"
    if not state_path.exists():
        return f"{agent_id}: skip (no state.json)"

    period_days = read_charter_period_days(project_dir)
    period_seconds = period_days * 86400
    lock_path = project_dir / "state.json.lock"

    try:
        with open(lock_path, "w") as lf:
            fcntl.flock(lf, fcntl.LOCK_EX)
            try:
                with open(state_path) as f:
                    state = json.load(f)

                cost = state.get("cost", {})
                period = cost.get("current_period", {})
                start_str = period.get("start", "")
                if not start_str:
                    return f"{agent_id}: skip (no period.start)"

                try:
                    start = datetime.fromisoformat(start_str.replace("Z", "+00:00"))
                except Exception:
                    return f"{agent_id}: skip (unparseable period.start: {start_str!r})"

                age_seconds = (NOW - start).total_seconds()
                if age_seconds < period_seconds:
                    age_h = age_seconds / 3600
                    return f"{agent_id}: skip (period age {age_h:.1f}h < {period_days}d)"

                budget_usd = float(period.get("budget_usd", 0.0))
                spent_usd = float(period.get("spent_usd", 0.0))

                # Zero-budget guard — critic HIGH: avoid divide-by-zero, require manual assignment
                if budget_usd == 0:
                    emit_alert(
                        "WARN",
                        f"agent {agent_id}: budget_usd=0, cannot auto-reset — manual budget assignment required",
                    )
                    emit_event("hex.budget.no_budget", {"agent": agent_id, "spent_usd": spent_usd})
                    log_action(agent_id, "budget-reset-skipped", {
                        "reason": "zero_budget",
                        "spent_usd": spent_usd,
                        "period_age_h": age_seconds / 3600,
                    })
                    return f"{agent_id}: SKIP zero-budget (spent=${spent_usd:.2f})"

                ratio = spent_usd / budget_usd

                # >5x overage → CRITICAL, no reset
                if ratio > 5.0:
                    msg = (
                        f"agent {agent_id}: {ratio:.1f}x runaway "
                        f"(${spent_usd:.2f}/${budget_usd:.2f}) — manual review required"
                    )
                    emit_alert("CRITICAL", msg)
                    emit_event("hex.budget.runaway", {
                        "agent": agent_id,
                        "spent_usd": spent_usd,
                        "budget_usd": budget_usd,
                        "ratio": ratio,
                        "severity": "CRITICAL",
                    })
                    log_action(agent_id, "budget-reset-blocked", {
                        "reason": "5x_runaway",
                        "spent_usd": spent_usd,
                        "budget_usd": budget_usd,
                        "ratio": ratio,
                    })
                    return f"{agent_id}: BLOCKED CRITICAL {ratio:.1f}x (${spent_usd:.2f}/${budget_usd:.2f})"

                # 2x-5x overage → WARN, no reset
                if ratio > 2.0:
                    msg = (
                        f"agent {agent_id}: {ratio:.1f}x overage "
                        f"(${spent_usd:.2f}/${budget_usd:.2f}) — reset blocked, review spending"
                    )
                    emit_alert("WARN", msg)
                    emit_event("hex.budget.overage", {
                        "agent": agent_id,
                        "spent_usd": spent_usd,
                        "budget_usd": budget_usd,
                        "ratio": ratio,
                        "severity": "WARN",
                    })
                    log_action(agent_id, "budget-reset-blocked", {
                        "reason": "2x_5x_overage",
                        "spent_usd": spent_usd,
                        "budget_usd": budget_usd,
                        "ratio": ratio,
                    })
                    return f"{agent_id}: BLOCKED WARN {ratio:.1f}x (${spent_usd:.2f}/${budget_usd:.2f})"

                # 1x-2x overage → auto-reset with explicit audit flag
                overage_tag = "minor_overage" if ratio > 1.0 else "within_budget"

                # Perform the reset
                old_start = start_str
                period["start"] = NOW_ISO
                period["spent_usd"] = 0.0
                cost["current_period"] = period
                state["cost"] = cost

                # Write reset event to agent's own trail
                trail_detail = {
                    "reason": "auto_period_reset",
                    "old_start": old_start,
                    "new_start": NOW_ISO,
                    "spent_at_reset": spent_usd,
                    "budget_usd": budget_usd,
                    "ratio_at_reset": ratio,
                    "period_days": period_days,
                    "overage_tag": overage_tag,
                }
                state = append_trail_entry(state, agent_id, trail_detail)

                # Atomic write
                tmp = state_path.with_suffix(".json.tmp")
                with open(tmp, "w") as f:
                    json.dump(state, f, indent=2)
                os.replace(tmp, state_path)

                log_action(agent_id, "budget-reset", {
                    "reason": "auto-period-reset",
                    "old_start": old_start,
                    "new_start": NOW_ISO,
                    "spent_at_reset": spent_usd,
                    "budget_usd": budget_usd,
                    "ratio": ratio,
                    "overage_tag": overage_tag,
                    "period_days": period_days,
                })
                emit_event("hex.budget.period_reset", {
                    "agent": agent_id,
                    "old_start": old_start,
                    "new_start": NOW_ISO,
                    "spent_at_reset": spent_usd,
                    "budget_usd": budget_usd,
                    "ratio": ratio,
                })

                tag = f" [{overage_tag}]" if overage_tag != "within_budget" else ""
                return (
                    f"{agent_id}: RESET{tag} "
                    f"age={age_seconds/3600:.1f}h spent=${spent_usd:.2f}/${budget_usd:.2f} ({ratio:.1f}x)"
                )

            finally:
                fcntl.flock(lf, fcntl.LOCK_UN)

    except Exception as e:
        print(f"[{agent_id}] ERROR: {e}", file=sys.stderr)
        return f"{agent_id}: ERROR {e}"


def main() -> int:
    dry_run = "--dry-run" in sys.argv
    if dry_run:
        print("[budget-period-reset] DRY RUN — no writes will occur")
        print(f"[budget-period-reset] Projects dir: {PROJECTS}")
        print(f"[budget-period-reset] Default period: {DEFAULT_PERIOD_DAYS}d")
        count = sum(1 for p in PROJECTS.iterdir() if (p / "state.json").exists())
        print(f"[budget-period-reset] Would inspect {count} agents with state.json")
        return 0

    if not PROJECTS.exists():
        print(f"[budget-period-reset] ERROR: {PROJECTS} not found", file=sys.stderr)
        return 1

    results: list[str] = []
    reset_count = 0
    blocked_count = 0
    skip_count = 0
    error_count = 0

    for project_dir in sorted(PROJECTS.iterdir()):
        if not project_dir.is_dir():
            continue
        if project_dir.name.startswith("_"):
            continue
        result = process_agent(project_dir)
        results.append(result)
        if "RESET" in result:
            reset_count += 1
        elif "BLOCKED" in result:
            blocked_count += 1
        elif "ERROR" in result:
            error_count += 1
        else:
            skip_count += 1

    print(f"\n[budget-period-reset] Summary: {reset_count} reset | {blocked_count} blocked | {skip_count} skip | {error_count} error")
    for r in results:
        if not r.endswith("skip (no state.json)") and "skip (period age" not in r:
            print(f"  {r}")

    return 1 if error_count > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
