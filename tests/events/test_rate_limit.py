"""Tests for rate-limiting functionality.

1. Rate-limited action creates action_log entry with type "rate_limited"
2. Rate-limited action appears in history output with ⊘ marker
3. Rate limit counter resets after window expires
"""
import io
import json
import os
import sys
import tempfile
import time
from unittest.mock import patch

import pytest

# Ensure hex-events root is on path (conftest.py handles this, but be explicit)
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from db import EventsDB
from policy import Action, Policy, Rule, check_rate_limit, record_fire
from hex_eventd import _process_event_policies


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def db(tmp_path):
    db_path = str(tmp_path / "test.db")
    d = EventsDB(db_path)
    yield d
    d.close()


def make_policy(name, trigger_event, max_fires, window="1h", actions=None):
    """Build a Policy with a single rule and rate limit."""
    if actions is None:
        actions = [Action(type="shell", params={"command": "echo fired"})]
    rule = Rule(
        name=f"{name}.rule",
        trigger_event=trigger_event,
        conditions=[],
        actions=actions,
    )
    return Policy(
        name=name,
        rules=[rule],
        rate_limit={"max_fires": max_fires, "window": window},
    )


# ---------------------------------------------------------------------------
# Test 1: Rate-limited action creates action_log entry
# ---------------------------------------------------------------------------

def test_rate_limited_creates_action_log_entry(db):
    """Second firing of a rate-limited policy inserts a 'rate_limited' action_log row."""
    policy = make_policy("test-gate", "test.event", max_fires=1, window="1h")

    # First event — fires successfully, records a fire timestamp
    eid1 = db.insert_event("test.event", '{"x": 1}', "test")
    event1 = db.get_unprocessed()[0]
    _process_event_policies(event1, [policy], db)

    # Second event — should be rate-limited
    eid2 = db.insert_event("test.event", '{"x": 2}', "test")
    event2 = db.get_unprocessed()[0]
    _process_event_policies(event2, [policy], db)

    logs = db.get_action_logs(eid2)
    assert len(logs) == 1, f"Expected 1 action_log entry, got {len(logs)}"

    entry = logs[0]
    assert entry["action_type"] == "rate_limited"
    assert entry["status"] == "suppressed"
    assert entry["recipe"] == "test-gate"

    detail = json.loads(entry["action_detail"])
    assert detail["policy"] == "test-gate"
    assert detail["max_fires"] == 1
    assert detail["fires_in_window"] >= 1

    assert "Rate limited:" in entry["error_message"]


def test_rate_limited_does_not_create_action_log_when_under_limit(db):
    """First firing of a rate-limited policy is NOT suppressed."""
    policy = make_policy("test-gate", "test.event", max_fires=2, window="1h")

    eid = db.insert_event("test.event", '{"x": 1}', "test")
    event = db.get_unprocessed()[0]
    _process_event_policies(event, [policy], db)

    logs = db.get_action_logs(eid)
    rate_limited_logs = [l for l in logs if l["action_type"] == "rate_limited"]
    assert len(rate_limited_logs) == 0, "First firing should not be rate limited"


# ---------------------------------------------------------------------------
# Test 2: Rate-limited events appear in history with ⊘ marker
# ---------------------------------------------------------------------------

def test_history_shows_rate_limited_marker(db, tmp_path):
    """get_rate_limited_by_event returns the policy and history logic shows ⊘."""
    # Insert an event and a rate_limited action_log entry for it
    eid = db.insert_event("test.event", '{"x": 1}', "test")
    db.log_action(eid, "my-policy", "rate_limited",
                  json.dumps({"policy": "my-policy", "rule": "my-policy.rule",
                              "fires_in_window": 1, "max_fires": 1, "window": "1h"}),
                  "suppressed",
                  "Rate limited: 1/1 fires in 1h")
    db.mark_processed(eid, None)

    rate_limited_map = db.get_rate_limited_by_event([eid])
    assert eid in rate_limited_map
    assert rate_limited_map[eid] == "my-policy"


def test_history_cmd_output_shows_oplus_marker(db, tmp_path):
    """cmd_history prints ⊘ for rate-limited events."""
    import hex_events_cli

    # Inject a rate-limited event
    eid = db.insert_event("test.event", '{"x": 1}', "test")
    db.log_action(eid, "my-gate", "rate_limited",
                  json.dumps({"policy": "my-gate", "rule": "my-gate.rule",
                              "fires_in_window": 1, "max_fires": 1, "window": "1h"}),
                  "suppressed",
                  "Rate limited: 1/1 fires in 1h")
    db.mark_processed(eid, None)

    db_path = db.conn.execute("PRAGMA database_list").fetchone()[2]

    captured = io.StringIO()
    with patch.object(hex_events_cli, "DB_PATH", db_path), \
         patch("sys.stdout", captured):
        class FakeArgs:
            since = None
        hex_events_cli.cmd_history(FakeArgs())

    output = captured.getvalue()
    assert "⊘" in output, f"Expected ⊘ in output, got:\n{output}"
    assert "my-gate" in output


def test_history_cmd_output_shows_checkmark_for_success(db, tmp_path):
    """cmd_history prints ✓ for successfully processed non-rate-limited events."""
    import hex_events_cli

    eid = db.insert_event("test.event", '{"x": 1}', "test")
    db.mark_processed(eid, "some-recipe")

    db_path = db.conn.execute("PRAGMA database_list").fetchone()[2]

    captured = io.StringIO()
    with patch.object(hex_events_cli, "DB_PATH", db_path), \
         patch("sys.stdout", captured):
        class FakeArgs:
            since = None
        hex_events_cli.cmd_history(FakeArgs())

    output = captured.getvalue()
    assert "✓" in output
    assert "⊘" not in output


# ---------------------------------------------------------------------------
# Test 3: Rate limit counter resets after window expires
# ---------------------------------------------------------------------------

def test_rate_limit_resets_after_window_expires():
    """Fires older than the window don't count; policy becomes allowed again."""
    policy = make_policy("expiry-gate", "test.event", max_fires=1, window="60s")

    # Simulate a fire that happened 61 seconds ago (outside the 60s window)
    old_fire_ts = time.time() - 61
    policy.last_fires.append(old_fire_ts)

    # Should be allowed because the old fire is outside the window
    assert check_rate_limit(policy) is True


def test_rate_limit_blocks_within_window():
    """Fires within the window count against the limit."""
    policy = make_policy("expiry-gate", "test.event", max_fires=1, window="60s")

    # Record a recent fire
    record_fire(policy)

    # Should be blocked because we're within the 60s window
    assert check_rate_limit(policy) is False


def test_rate_limit_resets_integration(db):
    """Simulates full flow: fire → rate limited → window expires → allowed again."""
    policy = make_policy("expiry-gate", "test.event", max_fires=1, window="60s")

    # First fire — succeeds, records fire timestamp
    eid1 = db.insert_event("test.event", '{"x": 1}', "test")
    event1 = db.get_unprocessed()[0]
    _process_event_policies(event1, [policy], db)

    # Verify the first fire was allowed (no rate_limited entry)
    logs1 = db.get_action_logs(eid1)
    assert not any(l["action_type"] == "rate_limited" for l in logs1)

    # Second fire — should be suppressed (within window)
    eid2 = db.insert_event("test.event", '{"x": 2}', "test")
    event2 = db.get_unprocessed()[0]
    _process_event_policies(event2, [policy], db)
    logs2 = db.get_action_logs(eid2)
    assert any(l["action_type"] == "rate_limited" for l in logs2)

    # Simulate window expiry by backdating the recorded fires
    policy.last_fires = [t - 120 for t in policy.last_fires]  # 2 minutes ago

    # Third fire — window has expired, should be allowed
    assert check_rate_limit(policy) is True
    eid3 = db.insert_event("test.event", '{"x": 3}', "test")
    event3 = db.get_unprocessed()[0]
    _process_event_policies(event3, [policy], db)
    logs3 = db.get_action_logs(eid3)
    assert not any(l["action_type"] == "rate_limited" for l in logs3), \
        "After window expires, policy should be allowed to fire again"
