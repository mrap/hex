"""End-to-end tests for the landings-staleness policy (t-8).

Tests:
  1. git.commit event → deferred landings.check-due created with cancel_group
  2. landings.check-due fires policy.violation when no landings.updated in window
  3. landings.check-due does NOT fire policy.violation when landings.updated is recent
  4. Policy loads correctly (metadata, provides/requires, 3 rules)
"""
import json
import os
import sys
import tempfile
from datetime import datetime, timedelta

import pytest

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, _REPO_ROOT)

from db import EventsDB
from policy import load_policies
from conditions import evaluate_conditions
from hex_eventd import process_event, drain_deferred

POLICY_FILE = os.path.join(_REPO_ROOT, "policies", "landings-staleness.yaml")

pytestmark = pytest.mark.skipif(
    not os.path.exists(POLICY_FILE),
    reason="landings-staleness.yaml policy not found in repo (policy removed or not yet added)",
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def make_db():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    return EventsDB(path), path


def load_landings_policy():
    """Load the landings-staleness policy from the policies directory."""
    policies_dir = os.path.join(_REPO_ROOT, "policies")
    policies = load_policies(policies_dir)
    for p in policies:
        if p.name == "landings-staleness":
            return p
    raise RuntimeError("landings-staleness policy not found")


def get_rules(policy):
    """Extract Rule objects from a policy — duck-type compatible with Recipe."""
    return policy.rules


def insert_event_at(db, event_type, payload, source, created_at):
    """Insert event with a specific created_at timestamp (bypasses dedup).

    created_at must use SQLite-compatible format (space separator, not 'T').
    """
    # Normalize to SQLite datetime format (space separator)
    if isinstance(created_at, str):
        created_at = created_at.replace("T", " ").split(".")[0]
    db.conn.execute(
        "INSERT INTO events (event_type, payload, source, created_at, processed_at) "
        "VALUES (?, ?, ?, ?, datetime('now'))",
        (event_type, json.dumps(payload), source, created_at),
    )
    db.conn.commit()


# ---------------------------------------------------------------------------
# Test 1: Policy loads correctly
# ---------------------------------------------------------------------------

def test_policy_loads_correctly():
    policy = load_landings_policy()

    assert policy.name == "landings-staleness"
    assert "R-033" in policy.reflection_ids
    assert "9" in policy.standing_orders
    assert len(policy.rules) == 3

    rule_names = [r.name for r in policy.rules]
    assert "arm-check-on-commit" in rule_names
    assert "check-landings-staleness" in rule_names
    assert "notify-repeated-violations" in rule_names


def test_policy_provides_requires():
    policy = load_landings_policy()

    assert "landings.check-due" in policy.provides.get("events", [])
    assert "policy.violation" in policy.provides.get("events", [])
    assert "git.commit" in policy.requires.get("events", [])
    assert "landings.updated" in policy.requires.get("events", [])


def test_policy_rule_triggers():
    policy = load_landings_policy()
    rules = get_rules(policy)

    commit_rules = [r for r in rules if r.matches_event_type("git.commit")]
    assert len(commit_rules) == 1
    assert commit_rules[0].name == "arm-check-on-commit"

    check_rules = [r for r in rules if r.matches_event_type("landings.check-due")]
    assert len(check_rules) == 1
    assert check_rules[0].name == "check-landings-staleness"

    violation_rules = [r for r in rules if r.matches_event_type("policy.violation")]
    assert len(violation_rules) == 1
    assert violation_rules[0].name == "notify-repeated-violations"


# ---------------------------------------------------------------------------
# Test 2: git.commit → deferred landings.check-due created
# ---------------------------------------------------------------------------

def test_git_commit_creates_deferred_check():
    db, path = make_db()
    try:
        policy = load_landings_policy()
        rules = get_rules(policy)

        event_id = db.insert_event("git.commit", json.dumps({"sha": "abc123"}), "test")
        event = db.get_unprocessed()[0]

        process_event(event, rules, db)

        # Deferred event should exist
        deferred = db.conn.execute("SELECT * FROM deferred_events").fetchall()
        assert len(deferred) == 1
        assert deferred[0]["event_type"] == "landings.check-due"
        assert deferred[0]["cancel_group"] == "landings-check"

        # No policy.violation yet
        violations = db.conn.execute(
            "SELECT id FROM events WHERE event_type = 'policy.violation'"
        ).fetchall()
        assert len(violations) == 0
    finally:
        db.close()
        os.unlink(path)


def test_git_commit_cancel_group_debounces():
    """Multiple git.commit events → only one pending deferred check (cancel_group)."""
    db, path = make_db()
    try:
        policy = load_landings_policy()
        rules = get_rules(policy)

        for sha in ["abc", "def", "ghi"]:
            event_id = db.insert_event("git.commit", json.dumps({"sha": sha}), "test")

        for event in db.get_unprocessed():
            process_event(event, rules, db)

        deferred = db.conn.execute("SELECT * FROM deferred_events").fetchall()
        assert len(deferred) == 1, "cancel_group should keep only the latest pending check"
    finally:
        db.close()
        os.unlink(path)


# ---------------------------------------------------------------------------
# Test 3: landings.check-due → policy.violation when no landings.updated
# ---------------------------------------------------------------------------

def test_check_due_fires_violation_when_no_landings_updated():
    db, path = make_db()
    try:
        policy = load_landings_policy()
        rules = get_rules(policy)

        # No landings.updated events exist

        event_id = db.insert_event("landings.check-due", json.dumps({}), "test")
        event = db.get_unprocessed()[0]
        process_event(event, rules, db)

        violations = db.conn.execute(
            "SELECT id FROM events WHERE event_type = 'policy.violation'"
        ).fetchall()
        assert len(violations) == 1
    finally:
        db.close()
        os.unlink(path)


# ---------------------------------------------------------------------------
# Test 4: landings.check-due → NO violation when landings.updated is recent
# ---------------------------------------------------------------------------

def test_check_due_no_violation_when_landings_updated_recently():
    db, path = make_db()
    try:
        policy = load_landings_policy()
        rules = get_rules(policy)

        # Simulate a recent landings.updated (5 minutes ago = within 10m window)
        recent_ts = (datetime.utcnow() - timedelta(minutes=5)).isoformat()
        insert_event_at(db, "landings.updated", {"path": "landings/main.md"},
                        "test", recent_ts)

        event_id = db.insert_event("landings.check-due", json.dumps({}), "test")
        # get only unprocessed (insert_event_at marks as processed already)
        events = [e for e in db.get_unprocessed() if e["event_type"] == "landings.check-due"]
        assert len(events) == 1
        process_event(events[0], rules, db)

        violations = db.conn.execute(
            "SELECT id FROM events WHERE event_type = 'policy.violation'"
        ).fetchall()
        assert len(violations) == 0, "No violation when landings.updated is within window"
    finally:
        db.close()
        os.unlink(path)


def test_check_due_fires_violation_when_landings_updated_too_old():
    db, path = make_db()
    try:
        policy = load_landings_policy()
        rules = get_rules(policy)

        # Simulate an OLD landings.updated (20 minutes ago = outside 10m window)
        old_ts = (datetime.utcnow() - timedelta(minutes=20)).isoformat()
        insert_event_at(db, "landings.updated", {"path": "landings/main.md"},
                        "test", old_ts)

        event_id = db.insert_event("landings.check-due", json.dumps({}), "test")
        events = [e for e in db.get_unprocessed() if e["event_type"] == "landings.check-due"]
        process_event(events[0], rules, db)

        violations = db.conn.execute(
            "SELECT id FROM events WHERE event_type = 'policy.violation'"
        ).fetchall()
        assert len(violations) == 1, "Violation fires when landings.updated is outside window"
    finally:
        db.close()
        os.unlink(path)


# ---------------------------------------------------------------------------
# Test 5: Full pipeline with time-forwarded drain (git.commit → drain → violation)
# ---------------------------------------------------------------------------

def test_full_pipeline_time_forwarded():
    """git.commit → deferred created → drain (time-forwarded) → violation fires."""
    db, path = make_db()
    try:
        policy = load_landings_policy()
        rules = get_rules(policy)

        # Step 1: Emit git.commit, process it
        db.insert_event("git.commit", json.dumps({"sha": "deadbeef"}), "test")
        commit_event = db.get_unprocessed()[0]
        process_event(commit_event, rules, db)

        # Verify deferred check was created
        deferred = db.conn.execute("SELECT * FROM deferred_events").fetchall()
        assert len(deferred) == 1
        assert deferred[0]["event_type"] == "landings.check-due"

        # Step 2: Fast-forward time by updating fire_at to past
        db.conn.execute(
            "UPDATE deferred_events SET fire_at = datetime('now', '-1 second')"
        )
        db.conn.commit()

        # Step 3: Drain deferred events (no landings.updated in last 10m)
        drain_deferred(db)

        # landings.check-due should now be in events table
        check_events = db.conn.execute(
            "SELECT id FROM events WHERE event_type = 'landings.check-due'"
        ).fetchall()
        assert len(check_events) == 1

        # Step 4: Process the check-due event
        pending = [e for e in db.get_unprocessed() if e["event_type"] == "landings.check-due"]
        assert len(pending) == 1
        process_event(pending[0], rules, db)

        # policy.violation should have fired
        violations = db.conn.execute(
            "SELECT id FROM events WHERE event_type = 'policy.violation'"
        ).fetchall()
        assert len(violations) == 1
    finally:
        db.close()
        os.unlink(path)


def test_full_pipeline_no_violation_when_updated():
    """git.commit → drain → no violation when landings.updated was emitted."""
    db, path = make_db()
    try:
        policy = load_landings_policy()
        rules = get_rules(policy)

        # Step 1: Emit git.commit
        db.insert_event("git.commit", json.dumps({"sha": "c0ffee"}), "test")
        commit_event = db.get_unprocessed()[0]
        process_event(commit_event, rules, db)

        # Step 2: Emit landings.updated (within 10m window = now)
        db.insert_event("landings.updated", json.dumps({"path": "landings/main.md"}), "test")
        # Mark it as processed (it's background data, not pending work)
        lu_events = [e for e in db.get_unprocessed() if e["event_type"] == "landings.updated"]
        for e in lu_events:
            db.mark_processed(e["id"])

        # Step 3: Fast-forward deferred fire_at to past
        db.conn.execute(
            "UPDATE deferred_events SET fire_at = datetime('now', '-1 second')"
        )
        db.conn.commit()

        drain_deferred(db)

        # Step 4: Process the check-due event
        pending = [e for e in db.get_unprocessed() if e["event_type"] == "landings.check-due"]
        assert len(pending) == 1
        process_event(pending[0], rules, db)

        # No violation: landings.updated was within the 10m window
        violations = db.conn.execute(
            "SELECT id FROM events WHERE event_type = 'policy.violation'"
        ).fetchall()
        assert len(violations) == 0
    finally:
        db.close()
        os.unlink(path)
