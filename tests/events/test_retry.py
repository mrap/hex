"""Tests for retry logic in hex_eventd — t-7. TDD first."""
import json
import os
import sys
import tempfile
import time

import pytest

sys.path.insert(0, os.path.expanduser("~/.hex-events"))

from db import EventsDB
from recipe import Recipe, Action, Condition
from hex_eventd import process_event, run_action_with_retry


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def make_db():
    tmp = tempfile.NamedTemporaryFile(suffix=".db", delete=False)
    tmp.close()
    return EventsDB(tmp.name), tmp.name


def make_recipe(name, trigger, actions):
    return Recipe(name=name, trigger_event=trigger, conditions=[], actions=actions)


class _AlwaysFailHandler:
    """Action handler that always returns error."""
    def run(self, params, event_payload=None, db=None, workflow_context=None):
        return {"status": "error", "output": "simulated failure"}


class _FailNTimesHandler:
    """Action handler that fails N times then succeeds."""
    def __init__(self, fail_count):
        self.fail_count = fail_count
        self.call_count = 0

    def run(self, params, event_payload=None, db=None, workflow_context=None):
        self.call_count += 1
        if self.call_count <= self.fail_count:
            return {"status": "error", "output": f"failure {self.call_count}"}
        return {"status": "ok", "output": "success"}


class _AlwaysOkHandler:
    def run(self, params, event_payload=None, db=None, workflow_context=None):
        return {"status": "ok", "output": "success"}


# ---------------------------------------------------------------------------
# Tests: run_action_with_retry
# ---------------------------------------------------------------------------

def test_retry_success_first_try():
    """Action succeeds on first try — no retries, one action_log entry."""
    db, path = make_db()
    try:
        event_id = db.insert_event("test.event", "{}", "test")
        handler = _AlwaysOkHandler()
        action = Action(type="shell", params={"cmd": "echo ok", "retries": 3})

        result = run_action_with_retry(
            action=action,
            event_id=event_id,
            recipe_name="test-recipe",
            payload={},
            db=db,
            handler=handler,
            sleep_fn=lambda s: None,
        )
        assert result["status"] == "ok"
        logs = db.get_action_logs(event_id)
        # Only one log entry (success)
        assert len(logs) == 1
        assert logs[0]["status"] == "ok"
    finally:
        db.close()
        os.unlink(path)


def test_retry_fails_then_succeeds():
    """Action fails 2 times then succeeds — 2 retry logs + 1 success log."""
    db, path = make_db()
    try:
        event_id = db.insert_event("test.event", "{}", "test")
        handler = _FailNTimesHandler(fail_count=2)
        action = Action(type="shell", params={"cmd": "echo ok", "retries": 3})

        result = run_action_with_retry(
            action=action,
            event_id=event_id,
            recipe_name="test-recipe",
            payload={},
            db=db,
            handler=handler,
            sleep_fn=lambda s: None,
        )
        assert result["status"] == "ok"
        logs = db.get_action_logs(event_id)
        statuses = [l["status"] for l in logs]
        # 2 retry attempts + 1 success
        assert len(logs) == 3
        assert statuses[0].startswith("retry_")
        assert statuses[1].startswith("retry_")
        assert statuses[2] == "ok"
        assert handler.call_count == 3
    finally:
        db.close()
        os.unlink(path)


def test_retry_all_failures_permanent():
    """Action always fails — logs retries then permanent failure."""
    db, path = make_db()
    try:
        event_id = db.insert_event("test.event", "{}", "test")
        handler = _AlwaysFailHandler()
        action = Action(type="shell", params={"cmd": "echo fail", "retries": 3})

        result = run_action_with_retry(
            action=action,
            event_id=event_id,
            recipe_name="test-recipe",
            payload={},
            db=db,
            handler=handler,
            sleep_fn=lambda s: None,
        )
        assert result["status"] == "error"
        logs = db.get_action_logs(event_id)
        statuses = [l["status"] for l in logs]
        # 3 retry attempts + 1 final error
        assert len(logs) == 4
        assert statuses[0].startswith("retry_")
        assert statuses[1].startswith("retry_")
        assert statuses[2].startswith("retry_")
        assert statuses[3] == "error"
    finally:
        db.close()
        os.unlink(path)


def test_retry_zero_retries():
    """retries=0 — fails immediately, no retry attempts."""
    db, path = make_db()
    try:
        event_id = db.insert_event("test.event", "{}", "test")
        handler = _AlwaysFailHandler()
        action = Action(type="shell", params={"cmd": "echo fail", "retries": 0})

        result = run_action_with_retry(
            action=action,
            event_id=event_id,
            recipe_name="test-recipe",
            payload={},
            db=db,
            handler=handler,
            sleep_fn=lambda s: None,
        )
        assert result["status"] == "error"
        logs = db.get_action_logs(event_id)
        assert len(logs) == 1
        assert logs[0]["status"] == "error"
    finally:
        db.close()
        os.unlink(path)


def test_retry_default_retries():
    """Default retries=3 when not specified in params."""
    db, path = make_db()
    try:
        event_id = db.insert_event("test.event", "{}", "test")
        handler = _AlwaysFailHandler()
        action = Action(type="shell", params={"cmd": "echo fail"})  # no retries key

        result = run_action_with_retry(
            action=action,
            event_id=event_id,
            recipe_name="test-recipe",
            payload={},
            db=db,
            handler=handler,
            sleep_fn=lambda s: None,
        )
        assert result["status"] == "error"
        logs = db.get_action_logs(event_id)
        # Default 3 retries + 1 final error = 4 logs
        assert len(logs) == 4
    finally:
        db.close()
        os.unlink(path)


def test_retry_backoff_sleep_called():
    """Backoff sleep durations are 1s, 2s, 4s for 3 retries."""
    db, path = make_db()
    try:
        event_id = db.insert_event("test.event", "{}", "test")
        handler = _AlwaysFailHandler()
        action = Action(type="shell", params={"cmd": "echo fail", "retries": 3})
        sleeps = []

        run_action_with_retry(
            action=action,
            event_id=event_id,
            recipe_name="test-recipe",
            payload={},
            db=db,
            handler=handler,
            sleep_fn=lambda s: sleeps.append(s),
        )
        assert sleeps == [1, 2, 4]
    finally:
        db.close()
        os.unlink(path)


# ---------------------------------------------------------------------------
# Tests: events.recipe column — all matched policy names
# ---------------------------------------------------------------------------

def test_recipe_column_stores_all_matched():
    """events.recipe stores comma-separated list of all matched recipe names."""
    db, path = make_db()
    try:
        event_id = db.insert_event("test.event", json.dumps({"val": 1}), "test")
        event = db.get_unprocessed()[0]

        recipes = [
            make_recipe("recipe-a", "test.event", []),
            make_recipe("recipe-b", "test.event", []),
            make_recipe("recipe-c", "other.event", []),  # won't match
        ]

        process_event(event, recipes, db)
        processed = db.conn.execute(
            "SELECT recipe FROM events WHERE id = ?", (event_id,)
        ).fetchone()
        stored = processed["recipe"] or ""
        names = stored.split(",")
        assert "recipe-a" in names
        assert "recipe-b" in names
        assert "recipe-c" not in names
    finally:
        db.close()
        os.unlink(path)


def test_recipe_column_single_match():
    """events.recipe stores single name (no trailing comma) when one recipe matches."""
    db, path = make_db()
    try:
        event_id = db.insert_event("test.event", json.dumps({}), "test")
        event = db.get_unprocessed()[0]
        recipes = [make_recipe("only-one", "test.event", [])]

        process_event(event, recipes, db)
        processed = db.conn.execute(
            "SELECT recipe FROM events WHERE id = ?", (event_id,)
        ).fetchone()
        assert processed["recipe"] == "only-one"
    finally:
        db.close()
        os.unlink(path)


def test_recipe_column_no_match():
    """events.recipe is None when no recipe matches."""
    db, path = make_db()
    try:
        event_id = db.insert_event("test.event", json.dumps({}), "test")
        event = db.get_unprocessed()[0]
        recipes = [make_recipe("other", "other.event", [])]

        process_event(event, recipes, db)
        processed = db.conn.execute(
            "SELECT recipe FROM events WHERE id = ?", (event_id,)
        ).fetchone()
        assert processed["recipe"] is None
    finally:
        db.close()
        os.unlink(path)
