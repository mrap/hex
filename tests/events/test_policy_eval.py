"""Tests for the policy evaluation trace (t-4).

Covers:
1. Processing an event creates policy_eval_log entries
2. Matched-but-conditions-failed entry has correct condition_details JSON
3. Rate-limited evaluations are logged with rate_limited=1
4. trace CLI command output format
5. Unmatched triggers produce no log entries (not logged)
"""
import argparse
import json
import os
import sys
import tempfile
import time
from io import StringIO
from unittest.mock import patch

import pytest

# conftest.py adds parent dir to sys.path
from db import EventsDB
from policy import Policy, Rule, Action, Condition
from hex_eventd import _process_event_policies
from hex_events_cli import _format_trace_row, cmd_trace


# ---------------------------------------------------------------------------
# Fixtures and helpers
# ---------------------------------------------------------------------------

@pytest.fixture
def tmp_db_path():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    yield path
    if os.path.exists(path):
        os.unlink(path)


@pytest.fixture
def db(tmp_db_path):
    d = EventsDB(tmp_db_path)
    yield d
    d.close()


def make_policy(name, trigger_event, conditions=None, actions=None, rate_limit=None):
    rule = Rule(
        name=name,
        trigger_event=trigger_event,
        conditions=conditions or [],
        actions=actions or [],
    )
    return Policy(name=name, rules=[rule], rate_limit=rate_limit)


def insert_event(db, event_type, payload=None):
    eid = db.insert_event(event_type, json.dumps(payload or {}), "test")
    return dict(db.conn.execute("SELECT * FROM events WHERE id = ?", (eid,)).fetchone())


# ---------------------------------------------------------------------------
# 1. Processing an event creates policy_eval_log entries
# ---------------------------------------------------------------------------

def test_processing_event_creates_eval_entries(db):
    policy = make_policy("test-policy", "git.commit")
    event = insert_event(db, "git.commit", {"branch": "main"})

    _process_event_policies(event, [policy], db)

    rows = db.get_policy_evals(event["id"])
    assert len(rows) == 1
    row = rows[0]
    assert row["policy_name"] == "test-policy"
    assert row["rule_name"] == "test-policy"
    assert row["matched"] == 1
    assert row["evaluated_at"] is not None


def test_multiple_policies_only_matched_logged(db):
    matching = make_policy("match-policy", "git.commit")
    non_matching = make_policy("skip-policy", "file.modified")
    event = insert_event(db, "git.commit", {})

    _process_event_policies(event, [matching, non_matching], db)

    rows = db.get_policy_evals(event["id"])
    policy_names = [r["policy_name"] for r in rows]
    assert "match-policy" in policy_names
    assert "skip-policy" not in policy_names


# ---------------------------------------------------------------------------
# 2. Matched-but-conditions-failed entry has correct condition_details JSON
# ---------------------------------------------------------------------------

def test_conditions_failed_entry_has_condition_details(db):
    policy = make_policy(
        "check-branch",
        "git.commit",
        conditions=[Condition(field="branch", op="eq", value="main")],
    )
    event = insert_event(db, "git.commit", {"branch": "dev"})

    _process_event_policies(event, [policy], db)

    rows = db.get_policy_evals(event["id"])
    assert len(rows) == 1
    row = rows[0]
    assert row["matched"] == 1
    assert row["conditions_passed"] == 0
    assert row["action_taken"] == 0

    details = json.loads(row["condition_details"])
    assert len(details) == 1
    d = details[0]
    assert d["field"] == "branch"
    assert d["op"] == "eq"
    assert d["expected"] == "main"
    assert d["actual"] == "dev"
    assert d["passed"] is False


def test_conditions_passed_entry_has_correct_fields(db):
    policy = make_policy(
        "check-status",
        "task.done",
        conditions=[Condition(field="status", op="eq", value="completed")],
    )
    event = insert_event(db, "task.done", {"status": "completed"})

    _process_event_policies(event, [policy], db)

    rows = db.get_policy_evals(event["id"])
    assert len(rows) == 1
    row = rows[0]
    assert row["matched"] == 1
    assert row["conditions_passed"] == 1

    details = json.loads(row["condition_details"])
    assert details[0]["passed"] is True
    assert details[0]["actual"] == "completed"


# ---------------------------------------------------------------------------
# 3. Rate-limited evaluations are logged with rate_limited=1
# ---------------------------------------------------------------------------

def test_rate_limited_evaluation_logged(db):
    policy = make_policy(
        "throttled-policy",
        "git.commit",
        rate_limit={"max_fires": 1, "window": "1h"},
    )
    # Pre-fill fires to hit the limit
    policy.last_fires = [time.time()]

    event = insert_event(db, "git.commit", {})
    _process_event_policies(event, [policy], db)

    rows = db.get_policy_evals(event["id"])
    assert len(rows) == 1
    row = rows[0]
    assert row["rate_limited"] == 1
    assert row["matched"] == 1
    assert row["action_taken"] == 0
    assert row["conditions_passed"] is None


def test_not_rate_limited_has_rate_limited_zero(db):
    policy = make_policy("free-policy", "git.push")
    event = insert_event(db, "git.push", {})

    _process_event_policies(event, [policy], db)

    rows = db.get_policy_evals(event["id"])
    assert rows[0]["rate_limited"] == 0


# ---------------------------------------------------------------------------
# 4. trace CLI command output format
# ---------------------------------------------------------------------------

def test_format_trace_row_conditions_passed():
    row = {
        "policy_name": "notify-policy",
        "rule_name": "notify-rule",
        "matched": 1,
        "conditions_passed": 1,
        "condition_details": json.dumps([
            {
                "field": "status", "op": "eq", "expected": "completed",
                "actual": "completed", "passed": True,
            }
        ]),
        "rate_limited": 0,
        "action_taken": 1,
    }
    result = _format_trace_row("task.completed", row, [])
    assert "✓" in result
    assert "notify-policy" in result
    assert "notify-rule" in result
    assert "Conditions: passed" in result


def test_format_trace_row_conditions_failed():
    row = {
        "policy_name": "check-policy",
        "rule_name": "check-rule",
        "matched": 1,
        "conditions_passed": 0,
        "condition_details": json.dumps([
            {
                "field": "status", "op": "eq", "expected": "done",
                "actual": "failed", "passed": False,
            }
        ]),
        "rate_limited": 0,
        "action_taken": 0,
    }
    result = _format_trace_row("task.done", row, [])
    assert "✗" in result
    assert "Conditions: failed" in result
    assert "status eq done" in result
    assert "actual: failed" in result


def test_format_trace_row_rate_limited():
    row = {
        "policy_name": "rl-policy",
        "rule_name": "rl-rule",
        "matched": 1,
        "conditions_passed": None,
        "condition_details": None,
        "rate_limited": 1,
        "action_taken": 0,
    }
    result = _format_trace_row("some.event", row, [])
    assert "⊘" in result
    assert "Rate limited: yes" in result
    assert "rl-policy" in result


def test_cmd_trace_event_output(tmp_db_path):
    """cmd_trace shows policy evaluations for a given event-id."""
    d = EventsDB(tmp_db_path)
    policy = make_policy("my-policy", "test.event")
    event = insert_event(d, "test.event", {})
    _process_event_policies(event, [policy], d)
    d.close()

    import hex_events_cli
    original_db_path = hex_events_cli.DB_PATH
    hex_events_cli.DB_PATH = tmp_db_path

    try:
        args = argparse.Namespace(event_id=event["id"], policy=None, since=None)
        captured = StringIO()
        with patch("sys.stdout", captured):
            cmd_trace(args)
        output = captured.getvalue()
    finally:
        hex_events_cli.DB_PATH = original_db_path

    assert "Policy evaluations:" in output
    assert "my-policy" in output
    assert f"Event #{event['id']}" in output


def test_cmd_trace_policy_filter(tmp_db_path):
    """cmd_trace --policy filters to only that policy."""
    d = EventsDB(tmp_db_path)
    policy_a = make_policy("policy-a", "x.event")
    policy_b = make_policy("policy-b", "x.event")
    event = insert_event(d, "x.event", {})
    _process_event_policies(event, [policy_a, policy_b], d)
    d.close()

    import hex_events_cli
    original_db_path = hex_events_cli.DB_PATH
    hex_events_cli.DB_PATH = tmp_db_path

    try:
        args = argparse.Namespace(event_id=event["id"], policy="policy-a", since=None)
        captured = StringIO()
        with patch("sys.stdout", captured):
            cmd_trace(args)
        output = captured.getvalue()
    finally:
        hex_events_cli.DB_PATH = original_db_path

    assert "policy-a" in output
    assert "policy-b" not in output


# ---------------------------------------------------------------------------
# 5. Unmatched triggers produce no log entries
# ---------------------------------------------------------------------------

def test_unmatched_event_type_not_logged(db):
    """Policies whose trigger doesn't match the event are not logged at all."""
    policy = make_policy("git-policy", "git.commit")
    event = insert_event(db, "file.modified", {})

    _process_event_policies(event, [policy], db)

    rows = db.get_policy_evals(event["id"])
    assert rows == []


def test_no_policies_no_eval_entries(db):
    event = insert_event(db, "any.event", {})
    _process_event_policies(event, [], db)
    assert db.get_policy_evals(event["id"]) == []
