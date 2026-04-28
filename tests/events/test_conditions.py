# tests/test_conditions.py
"""TDD tests for conditions.py v2 features: duration strings in count()."""
import os
import tempfile

import pytest

from db import EventsDB
from conditions import evaluate_conditions
from recipe import Condition


@pytest.fixture
def db():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    d = EventsDB(path)
    yield d
    d.close()
    os.unlink(path)


# ---------------------------------------------------------------------------
# Existing condition ops (regression guard)
# ---------------------------------------------------------------------------

def test_simple_eq():
    conds = [Condition(field="status", op="eq", value="completed")]
    assert evaluate_conditions(conds, {"status": "completed"}, db=None) is True

def test_simple_neq_fails():
    conds = [Condition(field="status", op="eq", value="completed")]
    assert evaluate_conditions(conds, {"status": "failed"}, db=None) is False

def test_gt():
    conds = [Condition(field="count", op="gt", value=3)]
    assert evaluate_conditions(conds, {"count": 5}, db=None) is True

def test_no_conditions_always_true():
    assert evaluate_conditions([], {}, db=None) is True


# ---------------------------------------------------------------------------
# count() with duration strings
# ---------------------------------------------------------------------------

def test_count_with_minutes_duration(db):
    """count(event_type, 10m) works and returns correct count."""
    for _ in range(4):
        db.insert_event("boi.spec.failed", "{}", "src")
    conds = [Condition(field="count(boi.spec.failed, 10m)", op="gte", value=4)]
    assert evaluate_conditions(conds, {}, db=db) is True

def test_count_with_minutes_not_met(db):
    db.insert_event("boi.spec.failed", "{}", "src")
    conds = [Condition(field="count(boi.spec.failed, 10m)", op="gte", value=3)]
    assert evaluate_conditions(conds, {}, db=db) is False

def test_count_with_hours_duration_string(db):
    """count(event_type, 1h) works."""
    for _ in range(2):
        db.insert_event("test.event", "{}", "src")
    conds = [Condition(field="count(test.event, 1h)", op="eq", value=2)]
    assert evaluate_conditions(conds, {}, db=db) is True

def test_count_with_days_duration_string(db):
    """count(event_type, 168h) for 7-day window."""
    for _ in range(5):
        db.insert_event("policy.violation", "{}", "src")
    conds = [Condition(field="count(policy.violation, 168h)", op="gte", value=3)]
    assert evaluate_conditions(conds, {}, db=db) is True

def test_count_with_seconds_duration_string(db):
    """count(event_type, 30s) works."""
    db.insert_event("fast.event", "{}", "src")
    conds = [Condition(field="count(fast.event, 30s)", op="gte", value=1)]
    assert evaluate_conditions(conds, {}, db=db) is True

def test_count_bare_int_backwards_compat(db):
    """count(event_type, 1) still works (treated as hours, backwards compat)."""
    for _ in range(3):
        db.insert_event("boi.spec.failed", "{}", "src")
    conds = [Condition(field="count(boi.spec.failed, 1)", op="gte", value=3)]
    assert evaluate_conditions(conds, {}, db=db) is True

def test_count_zero_when_no_events(db):
    conds = [Condition(field="count(missing.event, 10m)", op="eq", value=0)]
    assert evaluate_conditions(conds, {}, db=db) is True

def test_count_with_mixed_conditions(db):
    """count() condition alongside payload condition."""
    for _ in range(2):
        db.insert_event("alert.fired", "{}", "src")
    conds = [
        Condition(field="status", op="eq", value="active"),
        Condition(field="count(alert.fired, 1h)", op="gte", value=2),
    ]
    assert evaluate_conditions(conds, {"status": "active"}, db=db) is True


# ---------------------------------------------------------------------------
# Shell conditions
# ---------------------------------------------------------------------------

def test_shell_condition_exit_0_passes():
    """Shell command that exits 0 → condition passes."""
    conds = [Condition(type="shell", command="true")]
    assert evaluate_conditions(conds, {}, db=None) is True


def test_shell_condition_exit_1_fails():
    """Shell command that exits 1 → condition fails."""
    conds = [Condition(type="shell", command="false")]
    assert evaluate_conditions(conds, {}, db=None) is False


def test_shell_condition_jinja2_template_rendered():
    """Jinja2 {{ event.* }} templates in shell commands are rendered before execution."""
    # Command uses event.status — if rendered, it becomes "true", which exits 0
    conds = [Condition(type="shell", command="test '{{ event.status }}' = 'ok'")]
    assert evaluate_conditions(conds, {"status": "ok"}, db=None) is True


def test_shell_condition_timeout_fails_gracefully():
    """Shell condition that times out → condition fails without crashing."""
    import unittest.mock as mock
    import subprocess
    with mock.patch("conditions.subprocess.run", side_effect=subprocess.TimeoutExpired("sleep", 30)):
        conds = [Condition(type="shell", command="sleep 9999")]
        result = evaluate_conditions(conds, {}, db=None)
    assert result is False


def test_field_condition_regression_with_shell_present():
    """Field-based conditions still work when shell conditions are also in use."""
    conds = [
        Condition(type="shell", command="true"),
        Condition(field="status", op="eq", value="done"),
    ]
    assert evaluate_conditions(conds, {"status": "done"}, db=None) is True

    conds2 = [
        Condition(type="shell", command="true"),
        Condition(field="status", op="eq", value="done"),
    ]
    assert evaluate_conditions(conds2, {"status": "failed"}, db=None) is False
