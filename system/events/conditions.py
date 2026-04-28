# conditions.py
"""Condition evaluator for hex-events recipes."""
import fnmatch
import logging
import re
import subprocess
from policy import Condition
from db import parse_duration

log = logging.getLogger("hex-events")

# Matches count(event_type, duration) where duration is like 10m, 1h, 2d, 30s, or bare int
COUNT_RE = re.compile(r"^count\(([^,]+),\s*(\d+[smhd]?)(?:,\s*(\w+)=([^)]+))?\)$")


def evaluate_conditions(conditions: list[Condition], payload: dict, db) -> bool:
    """Evaluate all conditions (AND logic). Returns True if all pass."""
    passed, _ = evaluate_conditions_with_details(conditions, payload, db)
    return passed


def evaluate_conditions_with_details(
    conditions: list[Condition], payload: dict, db
) -> tuple[bool, list[dict]]:
    """Evaluate all conditions (AND logic) and return per-condition details.

    Returns:
        (all_passed: bool, details: list[dict])

    Each detail dict has keys: field, op, expected, actual, passed.
    Conditions after the first failure are marked passed="not_evaluated"
    (short-circuit AND logic).
    """
    if not conditions:
        return True, []

    details = []
    for cond in conditions:
        actual, passed = _evaluate_one_with_actual(cond, payload, db)
        if cond.type == "shell":
            details.append({
                "field": None,
                "op": "shell",
                "expected": cond.command,
                "actual": actual,
                "passed": passed,
            })
        else:
            details.append({
                "field": cond.field,
                "op": cond.op,
                "expected": cond.value,
                "actual": actual,
                "passed": passed,
            })
        if not passed:
            # Short-circuit: mark remaining conditions as not evaluated
            for remaining in conditions[len(details):]:
                if remaining.type == "shell":
                    details.append({
                        "field": None,
                        "op": "shell",
                        "expected": remaining.command,
                        "actual": None,
                        "passed": "not_evaluated",
                    })
                else:
                    details.append({
                        "field": remaining.field,
                        "op": remaining.op,
                        "expected": remaining.value,
                        "actual": None,
                        "passed": "not_evaluated",
                    })
            return False, details

    return True, details


def _resolve_field(field: str, payload: dict):
    """Resolve a field path against the payload.

    Paths starting with 'payload.' traverse the payload dict using dot-notation.
    E.g. 'payload.spec_id' → payload['spec_id']
         'payload.tasks.completed' → payload['tasks']['completed']
    Other field names are looked up directly in the payload.
    Returns None if any step in the path is missing.
    """
    if field.startswith("payload."):
        parts = field[len("payload."):].split(".")
    else:
        parts = [field]

    current = payload
    for part in parts:
        if not isinstance(current, dict):
            return None
        if part not in current:
            return None
        current = current[part]
    return current


def _evaluate_shell_condition(command: str, payload: dict) -> tuple:
    """Evaluate a shell condition. Returns (exit_code_str, passed: bool)."""
    # Render Jinja2 templates in the command
    if "{{" in command:
        try:
            from jinja2 import Template
            from datetime import datetime
            command = Template(command).render(event=payload, now=datetime.utcnow())
        except Exception as e:
            import sys
            msg = f"[CONDITION ERROR] Shell condition template render failed: {e}"
            log.error(msg)
            print(msg, file=sys.stderr)
            return None, False

    log.debug("Shell condition: %s", command)
    try:
        result = subprocess.run(
            command,
            shell=True,
            timeout=30,
            capture_output=True,
        )
        passed = result.returncode == 0
        log.debug("Shell condition exit=%d passed=%s cmd=%s", result.returncode, passed, command)
        return result.returncode, passed
    except subprocess.TimeoutExpired:
        import sys
        msg = f"[CONDITION ERROR] Shell condition timed out: {command}"
        log.error(msg)
        print(msg, file=sys.stderr)
        return None, False
    except Exception as e:
        import sys
        msg = f"[CONDITION ERROR] Shell condition failed: {command} — {e}"
        log.error(msg)
        print(msg, file=sys.stderr)
        return None, False


def _evaluate_one_with_actual(cond: Condition, payload: dict, db) -> tuple:
    """Return (actual_value, passed: bool) for a single condition."""
    # Handle shell conditions
    if cond.type == "shell":
        return _evaluate_shell_condition(cond.command, payload)

    # Check for count() function
    m = COUNT_RE.match(cond.field)
    if m:
        event_type, duration_str = m.group(1), m.group(2)
        filter_field, filter_value = m.group(3), m.group(4)
        if db is None:
            return None, False
        seconds = parse_duration(duration_str)
        payload_filter = (filter_field, filter_value) if filter_field else None
        actual = db.count_events(event_type, seconds=seconds,
                                 payload_filter=payload_filter)
    else:
        actual = _resolve_field(cond.field, payload)
        if actual is None:
            return None, False

    op = cond.op
    expected = cond.value

    if op == "eq":
        passed = actual == expected
    elif op == "neq":
        passed = actual != expected
    elif op == "gt":
        passed = actual > expected
    elif op == "gte":
        passed = actual >= expected
    elif op == "lt":
        passed = actual < expected
    elif op == "lte":
        passed = actual <= expected
    elif op == "contains":
        passed = str(expected) in str(actual)
    elif op == "glob":
        passed = fnmatch.fnmatch(str(actual), str(expected))
    elif op == "regex":
        passed = bool(re.search(str(expected), str(actual)))
    else:
        passed = False

    return actual, passed
