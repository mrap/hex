"""Test that on_success/on_failure sub-actions fire only once after retries."""
import json
import pytest
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))

from db import EventsDB
from policy import Action
from hex_eventd import run_action_with_retry

# Register action handlers so sub-actions (emit) can be dispatched
import actions.emit  # noqa: F401
import actions.shell  # noqa: F401


class CountingHandler:
    """A handler that fails N times then succeeds. Tracks sub-action calls."""
    def __init__(self, fail_count=0):
        self.fail_count = fail_count
        self.call_count = 0

    def run(self, params, event_payload=None, db=None, workflow_context=None):
        self.call_count += 1
        if self.call_count <= self.fail_count:
            return {"status": "error", "output": f"fail {self.call_count}"}
        return {"status": "success", "output": "ok"}


@pytest.fixture
def db(tmp_path):
    db = EventsDB(str(tmp_path / "test.db"))
    return db


def test_on_failure_fires_once_after_all_retries_exhausted(db):
    """on_failure sub-actions must fire exactly once, after final retry failure."""
    action = Action(type="shell", params={
        "command": "exit 1",
        "retries": 3,
        "on_failure": [
            {"type": "emit", "event": "test.failure", "payload": {"msg": "failed"}}
        ]
    })
    handler = CountingHandler(fail_count=99)  # always fails

    run_action_with_retry(action, event_id=1, recipe_name="test",
                          payload={}, db=db, handler=handler, sleep_fn=lambda _: None)

    # Should have been called 4 times (initial + 3 retries)
    assert handler.call_count == 4

    # on_failure emit should have fired exactly ONCE
    events = db.conn.execute(
        "SELECT * FROM events WHERE event_type = 'test.failure'"
    ).fetchall()
    assert len(events) == 1, f"Expected 1 failure event, got {len(events)}"


def test_on_success_fires_once_after_retries_then_success(db):
    """on_success sub-actions must fire exactly once when action eventually succeeds."""
    action = Action(type="shell", params={
        "command": "echo ok",
        "retries": 3,
        "on_success": [
            {"type": "emit", "event": "test.success", "payload": {"msg": "ok"}}
        ]
    })
    handler = CountingHandler(fail_count=2)  # fails twice then succeeds

    run_action_with_retry(action, event_id=1, recipe_name="test",
                          payload={}, db=db, handler=handler, sleep_fn=lambda _: None)

    assert handler.call_count == 3

    events = db.conn.execute(
        "SELECT * FROM events WHERE event_type = 'test.success'"
    ).fetchall()
    assert len(events) == 1, f"Expected 1 success event, got {len(events)}"


def test_no_sub_actions_fire_on_intermediate_failures(db):
    """During retries, neither on_success nor on_failure should fire."""
    action = Action(type="shell", params={
        "command": "exit 1",
        "retries": 2,
        "on_success": [
            {"type": "emit", "event": "test.success"}
        ],
        "on_failure": [
            {"type": "emit", "event": "test.failure"}
        ]
    })
    handler = CountingHandler(fail_count=99)

    run_action_with_retry(action, event_id=1, recipe_name="test",
                          payload={}, db=db, handler=handler, sleep_fn=lambda _: None)

    success_events = db.conn.execute(
        "SELECT * FROM events WHERE event_type = 'test.success'"
    ).fetchall()
    failure_events = db.conn.execute(
        "SELECT * FROM events WHERE event_type = 'test.failure'"
    ).fetchall()

    assert len(success_events) == 0, "on_success should not fire when all retries fail"
    assert len(failure_events) == 1, "on_failure should fire exactly once"
