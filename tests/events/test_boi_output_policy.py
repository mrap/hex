"""Tests for the boi-output-persistence policy (R-047).

Tests:
  1. Policy loads correctly (metadata, provides/requires, 2 rules)
  2. boi.iteration.done with tasks_completed > 0 → deferred boi.output.check-due
  3. boi.iteration.done with tasks_completed == 0 → no deferred check
  4. boi.output.check-due triggers the check rule
  5. Rule matching logic
"""
import json
import os
import sys
import tempfile

import pytest

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, _REPO_ROOT)

from policy import load_policies
from conditions import evaluate_conditions, Condition
from hex_eventd import process_event

POLICY_FILE = os.path.join(_REPO_ROOT, "policies", "boi-lifecycle", "boi-output-persistence.yaml")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def load_boi_policy():
    """Load the boi-output-persistence policy from the policies directory."""
    policies_dir = os.path.join(_REPO_ROOT, "policies")
    policies = load_policies(policies_dir)
    for p in policies:
        if p.name == "boi-output-persistence":
            return p
    raise RuntimeError("boi-output-persistence policy not found")


def make_db():
    from db import EventsDB
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    return EventsDB(path), path


# ---------------------------------------------------------------------------
# Test 1: Policy loads correctly
# ---------------------------------------------------------------------------

def test_policy_loads_correctly():
    policy = load_boi_policy()

    assert policy.name == "boi-output-persistence"
    assert "R-047" in policy.reflection_ids
    assert "26" in policy.standing_orders
    assert len(policy.rules) == 2


def test_policy_provides_requires():
    policy = load_boi_policy()

    assert "boi.output.check-due" in policy.provides.get("events", [])
    assert "policy.violation" in policy.provides.get("events", [])
    assert "boi.iteration.done" in policy.requires.get("events", [])


def test_policy_has_rate_limit():
    policy = load_boi_policy()
    assert policy.rate_limit is not None
    assert policy.rate_limit.get("max_fires", 0) > 0


# ---------------------------------------------------------------------------
# Test 2: Rule trigger matching
# ---------------------------------------------------------------------------

def test_arm_rule_matches_iteration_done():
    policy = load_boi_policy()
    rules = policy.rules

    arm_rules = [r for r in rules if r.matches_event_type("boi.iteration.done")]
    assert len(arm_rules) == 1
    assert arm_rules[0].name == "arm-check-on-iteration-done"


def test_check_rule_matches_check_due():
    policy = load_boi_policy()
    rules = policy.rules

    check_rules = [r for r in rules if r.matches_event_type("boi.output.check-due")]
    assert len(check_rules) == 1
    assert check_rules[0].name == "check-output-files-exist"


def test_arm_rule_has_emit_action():
    policy = load_boi_policy()
    arm_rule = next(r for r in policy.rules if r.name == "arm-check-on-iteration-done")

    assert len(arm_rule.actions) == 1
    action = arm_rule.actions[0]
    assert action.type == "emit"
    assert action.params.get("event") == "boi.output.check-due"
    assert action.params.get("delay") == "2m"
    assert "cancel_group" in action.params


def test_check_rule_has_shell_action():
    policy = load_boi_policy()
    check_rule = next(r for r in policy.rules if r.name == "check-output-files-exist")

    assert len(check_rule.actions) == 1
    action = check_rule.actions[0]
    assert action.type == "shell"
    assert "command" in action.params


# ---------------------------------------------------------------------------
# Test 3: Condition evaluation — tasks_completed > 0
# ---------------------------------------------------------------------------

def test_condition_tasks_completed_gt_zero_passes():
    cond = Condition(field="tasks_completed", op="gt", value=0)
    payload = {"tasks_completed": 3, "spec_id": "q-138"}
    assert evaluate_conditions([cond], payload, db=None) is True


def test_condition_tasks_completed_zero_fails():
    cond = Condition(field="tasks_completed", op="gt", value=0)
    payload = {"tasks_completed": 0, "spec_id": "q-138"}
    assert evaluate_conditions([cond], payload, db=None) is False


def test_condition_tasks_completed_missing_fails():
    """Missing field should fail the condition."""
    cond = Condition(field="tasks_completed", op="gt", value=0)
    payload = {"spec_id": "q-138"}
    assert evaluate_conditions([cond], payload, db=None) is False


# ---------------------------------------------------------------------------
# Test 4: boi.iteration.done → deferred check created
# ---------------------------------------------------------------------------

def test_iteration_done_creates_deferred_check():
    db, path = make_db()
    try:
        policy = load_boi_policy()
        rules = policy.rules

        payload = json.dumps({
            "spec_id": "q-138",
            "tasks_completed": 2,
            "output_paths": ["/tmp/test-output.md"],
        })
        db.insert_event("boi.iteration.done", payload, "test")
        event = db.get_unprocessed()[0]

        process_event(event, rules, db)

        deferred = db.conn.execute("SELECT * FROM deferred_events").fetchall()
        assert len(deferred) == 1
        assert deferred[0]["event_type"] == "boi.output.check-due"
        # cancel_group should contain the spec_id template or value
        assert "boi-output-check" in deferred[0]["cancel_group"]
    finally:
        db.close()
        os.unlink(path)


def test_iteration_done_zero_tasks_no_deferred_check():
    """boi.iteration.done with tasks_completed=0 should NOT create a deferred check."""
    db, path = make_db()
    try:
        policy = load_boi_policy()
        rules = policy.rules

        payload = json.dumps({
            "spec_id": "q-138",
            "tasks_completed": 0,
            "output_paths": [],
        })
        db.insert_event("boi.iteration.done", payload, "test")
        event = db.get_unprocessed()[0]

        process_event(event, rules, db)

        deferred = db.conn.execute("SELECT * FROM deferred_events").fetchall()
        assert len(deferred) == 0, "No deferred check when no tasks were completed"
    finally:
        db.close()
        os.unlink(path)


# ---------------------------------------------------------------------------
# Test 5: Multiple iterations — cancel_group debounces
# ---------------------------------------------------------------------------

def test_check_rule_fires_with_no_output_paths():
    """boi.output.check-due with no output_paths field should not crash or emit violation."""
    db, path = make_db()
    try:
        policy = load_boi_policy()
        rules = policy.rules

        # Event with no output_paths field at all
        payload = json.dumps({"spec_id": "q-138"})
        db.insert_event("boi.output.check-due", payload, "test")
        event = db.get_unprocessed()[0]

        # Should not raise; empty output_paths → no missing files → no violation
        process_event(event, rules, db)

        # No policy.violation events should be emitted
        new_events = db.conn.execute(
            "SELECT * FROM events WHERE event_type = 'policy.violation'"
        ).fetchall()
        assert len(new_events) == 0, "No violation when output_paths is absent"
    finally:
        db.close()
        os.unlink(path)


def test_check_rule_fires_with_null_output_paths():
    """boi.output.check-due with explicit null output_paths should not crash."""
    db, path = make_db()
    try:
        policy = load_boi_policy()
        rules = policy.rules

        payload = json.dumps({"spec_id": "q-138", "output_paths": None})
        db.insert_event("boi.output.check-due", payload, "test")
        event = db.get_unprocessed()[0]

        process_event(event, rules, db)

        new_events = db.conn.execute(
            "SELECT * FROM events WHERE event_type = 'policy.violation'"
        ).fetchall()
        assert len(new_events) == 0, "No violation when output_paths is null"
    finally:
        db.close()
        os.unlink(path)


def test_multiple_iterations_cancel_group_deduplicated():
    """Multiple boi.iteration.done events for the same spec → last wins."""
    db, path = make_db()
    try:
        policy = load_boi_policy()
        rules = policy.rules

        # Two iterations for the same spec
        for i in range(2):
            payload = json.dumps({
                "spec_id": "q-138",
                "tasks_completed": 1,
                "output_paths": ["/tmp/output.md"],
            })
            db.insert_event("boi.iteration.done", payload, "test")

        for event in db.get_unprocessed():
            process_event(event, rules, db)

        deferred = db.conn.execute("SELECT * FROM deferred_events").fetchall()
        assert len(deferred) == 1, "cancel_group should keep only latest deferred check"
    finally:
        db.close()
        os.unlink(path)
