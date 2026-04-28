"""Tests for condition evaluation detail recording (t-3).

Covers:
1. Condition pass records actual value and passed=True
2. Condition fail records actual value and passed=False
3. Short-circuited conditions show "not_evaluated"
4. count() expressions record the count result
5. Inspect output includes condition detail lines
"""
import os
import tempfile

import pytest

from conditions import evaluate_conditions_with_details
from db import EventsDB
from policy import Condition


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def db():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    d = EventsDB(path)
    yield d
    d.close()
    os.unlink(path)


# ---------------------------------------------------------------------------
# 1. Condition pass records actual value and passed=True
# ---------------------------------------------------------------------------

def test_condition_pass_records_actual_and_passed_true():
    conds = [Condition(field="status", op="eq", value="completed")]
    passed, details = evaluate_conditions_with_details(conds, {"status": "completed"}, db=None)

    assert passed is True
    assert len(details) == 1
    d = details[0]
    assert d["field"] == "status"
    assert d["op"] == "eq"
    assert d["expected"] == "completed"
    assert d["actual"] == "completed"
    assert d["passed"] is True


def test_condition_pass_records_correct_actual_for_numeric():
    conds = [Condition(field="count", op="gt", value=3)]
    passed, details = evaluate_conditions_with_details(conds, {"count": 5}, db=None)

    assert passed is True
    d = details[0]
    assert d["actual"] == 5
    assert d["passed"] is True


# ---------------------------------------------------------------------------
# 2. Condition fail records actual value and passed=False
# ---------------------------------------------------------------------------

def test_condition_fail_records_actual_and_passed_false():
    conds = [Condition(field="status", op="eq", value="completed")]
    passed, details = evaluate_conditions_with_details(conds, {"status": "running"}, db=None)

    assert passed is False
    assert len(details) == 1
    d = details[0]
    assert d["field"] == "status"
    assert d["op"] == "eq"
    assert d["expected"] == "completed"
    assert d["actual"] == "running"
    assert d["passed"] is False


def test_condition_fail_records_actual_for_gte():
    conds = [Condition(field="score", op="gte", value=10)]
    passed, details = evaluate_conditions_with_details(conds, {"score": 3}, db=None)

    assert passed is False
    d = details[0]
    assert d["actual"] == 3
    assert d["expected"] == 10
    assert d["passed"] is False


# ---------------------------------------------------------------------------
# 3. Short-circuited conditions show "not_evaluated"
# ---------------------------------------------------------------------------

def test_short_circuit_second_condition_not_evaluated():
    conds = [
        Condition(field="status", op="eq", value="completed"),
        Condition(field="score", op="gte", value=10),
    ]
    passed, details = evaluate_conditions_with_details(conds, {"status": "running", "score": 15}, db=None)

    assert passed is False
    assert len(details) == 2

    # First condition evaluated and failed
    assert details[0]["field"] == "status"
    assert details[0]["passed"] is False
    assert details[0]["actual"] == "running"

    # Second condition short-circuited
    assert details[1]["field"] == "score"
    assert details[1]["passed"] == "not_evaluated"
    assert details[1]["actual"] is None


def test_short_circuit_third_condition_when_second_fails():
    conds = [
        Condition(field="a", op="eq", value="x"),
        Condition(field="b", op="eq", value="y"),
        Condition(field="c", op="eq", value="z"),
    ]
    passed, details = evaluate_conditions_with_details(
        conds, {"a": "x", "b": "wrong", "c": "z"}, db=None
    )

    assert passed is False
    assert len(details) == 3

    assert details[0]["passed"] is True   # a passes
    assert details[1]["passed"] is False  # b fails
    assert details[2]["passed"] == "not_evaluated"  # c short-circuited


def test_no_short_circuit_when_all_pass():
    conds = [
        Condition(field="a", op="eq", value="x"),
        Condition(field="b", op="eq", value="y"),
    ]
    passed, details = evaluate_conditions_with_details(conds, {"a": "x", "b": "y"}, db=None)

    assert passed is True
    assert len(details) == 2
    assert all(d["passed"] is True for d in details)
    assert all(d["passed"] != "not_evaluated" for d in details)


def test_empty_conditions_returns_empty_details():
    passed, details = evaluate_conditions_with_details([], {}, db=None)
    assert passed is True
    assert details == []


# ---------------------------------------------------------------------------
# 4. count() expressions record the count result
# ---------------------------------------------------------------------------

def test_count_pass_records_actual_count(db):
    for _ in range(3):
        db.insert_event("boi.spec.failed", "{}", "src")

    conds = [Condition(field="count(boi.spec.failed, 1h)", op="gte", value=3)]
    passed, details = evaluate_conditions_with_details(conds, {}, db=db)

    assert passed is True
    d = details[0]
    assert d["field"] == "count(boi.spec.failed, 1h)"
    assert d["actual"] == 3
    assert d["expected"] == 3
    assert d["passed"] is True


def test_count_fail_records_actual_count(db):
    db.insert_event("boi.spec.failed", "{}", "src")

    conds = [Condition(field="count(boi.spec.failed, 1h)", op="gte", value=3)]
    passed, details = evaluate_conditions_with_details(conds, {}, db=db)

    assert passed is False
    d = details[0]
    assert d["field"] == "count(boi.spec.failed, 1h)"
    assert d["actual"] == 1
    assert d["expected"] == 3
    assert d["passed"] is False


def test_count_zero_records_zero_actual(db):
    conds = [Condition(field="count(missing.event, 10m)", op="eq", value=0)]
    passed, details = evaluate_conditions_with_details(conds, {}, db=db)

    assert passed is True
    d = details[0]
    assert d["actual"] == 0
    assert d["passed"] is True


def test_count_short_circuits_when_fails(db):
    """count() failure stops evaluation of subsequent conditions."""
    db.insert_event("boi.spec.failed", "{}", "src")

    conds = [
        Condition(field="count(boi.spec.failed, 1h)", op="gte", value=5),
        Condition(field="status", op="eq", value="done"),
    ]
    passed, details = evaluate_conditions_with_details(conds, {"status": "done"}, db=db)

    assert passed is False
    assert len(details) == 2
    assert details[0]["actual"] == 1
    assert details[0]["passed"] is False
    assert details[1]["passed"] == "not_evaluated"


# ---------------------------------------------------------------------------
# 5. Inspect output includes condition detail lines
# ---------------------------------------------------------------------------

def test_format_condition_detail_pass():
    from hex_events_cli import _format_condition_detail

    detail = {"field": "status", "op": "eq", "expected": "completed", "actual": "completed", "passed": True}
    line = _format_condition_detail(1, detail)

    assert "Condition 1" in line
    assert "status" in line
    assert "eq" in line
    assert "completed" in line
    assert "actual: completed" in line
    assert "✓" in line


def test_format_condition_detail_fail():
    from hex_events_cli import _format_condition_detail

    detail = {"field": "status", "op": "eq", "expected": "completed", "actual": "running", "passed": False}
    line = _format_condition_detail(2, detail)

    assert "Condition 2" in line
    assert "actual: running" in line
    assert "✗" in line


def test_format_condition_detail_not_evaluated():
    from hex_events_cli import _format_condition_detail

    detail = {"field": "score", "op": "gte", "expected": 10, "actual": None, "passed": "not_evaluated"}
    line = _format_condition_detail(3, detail)

    assert "Condition 3" in line
    assert "not evaluated" in line
    assert "short-circuited" in line


def test_format_condition_detail_count_expression():
    from hex_events_cli import _format_condition_detail

    detail = {
        "field": "count(boi.spec.failed, 1h)",
        "op": "gte",
        "expected": 3,
        "actual": 1,
        "passed": False,
    }
    line = _format_condition_detail(1, detail)

    assert "count(boi.spec.failed, 1h)" in line
    assert "actual: 1" in line
    assert "✗" in line
