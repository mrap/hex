# tests/test_emit.py
"""TDD tests for delayed emit + cancel_group in actions/emit.py."""
import json
import os
import sys
import tempfile
from datetime import datetime, timedelta

import pytest

# Ensure project root on path
sys.path.insert(0, os.path.expanduser("~/.hex-events"))

from db import EventsDB, parse_duration
from actions import get_action_handler


@pytest.fixture
def db():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    d = EventsDB(path)
    yield d
    d.close()
    os.unlink(path)


def emit(params: dict, db: EventsDB) -> dict:
    handler = get_action_handler("emit")
    return handler.run(params, event_payload={}, db=db)


# ---------------------------------------------------------------------------
# Normal emit (no delay) still works
# ---------------------------------------------------------------------------

def test_emit_without_delay_inserts_event(db):
    result = emit({"event": "test.event"}, db)
    assert result["status"] == "success"
    rows = db.get_unprocessed()
    assert len(rows) == 1
    assert rows[0]["event_type"] == "test.event"
    # No deferred rows created
    deferred = db.conn.execute("SELECT COUNT(*) FROM deferred_events").fetchone()[0]
    assert deferred == 0


def test_emit_without_delay_returns_emitted_key(db):
    result = emit({"event": "ping.event"}, db)
    assert result["emitted"] == "ping.event"


# ---------------------------------------------------------------------------
# Delayed emit creates deferred row, NOT events row
# ---------------------------------------------------------------------------

def test_emit_with_delay_creates_deferred_row(db):
    result = emit({"event": "landings.check-due", "delay": "10m"}, db)
    assert result["status"] == "success"
    assert result.get("deferred") is True
    # No row in events table
    assert db.get_unprocessed() == []
    # One row in deferred_events
    rows = db.conn.execute("SELECT * FROM deferred_events").fetchall()
    assert len(rows) == 1
    assert rows[0]["event_type"] == "landings.check-due"


def test_emit_with_delay_sets_fire_at_in_future(db):
    before = datetime.utcnow()
    emit({"event": "check.due", "delay": "5m"}, db)
    row = db.conn.execute("SELECT fire_at FROM deferred_events").fetchone()
    fire_at = datetime.fromisoformat(row["fire_at"])
    # fire_at should be ~5 minutes in the future (allow 5s tolerance)
    expected = before + timedelta(minutes=5)
    assert fire_at >= expected - timedelta(seconds=5)
    assert fire_at <= expected + timedelta(seconds=5)


def test_emit_with_delay_source_is_policy_emit(db):
    emit({"event": "check.due", "delay": "1m"}, db)
    row = db.conn.execute("SELECT source FROM deferred_events").fetchone()
    assert row["source"] == "policy-emit"


def test_emit_with_delay_stores_payload(db):
    emit({"event": "check.due", "delay": "1m", "payload": {"key": "val"}}, db)
    row = db.conn.execute("SELECT payload FROM deferred_events").fetchone()
    payload = json.loads(row["payload"])
    assert payload == {"key": "val"}


# ---------------------------------------------------------------------------
# cancel_group
# ---------------------------------------------------------------------------

def test_emit_with_cancel_group_stores_it(db):
    emit({"event": "check.due", "delay": "10m", "cancel_group": "landings-check"}, db)
    row = db.conn.execute("SELECT cancel_group FROM deferred_events").fetchone()
    assert row["cancel_group"] == "landings-check"


def test_emit_cancel_group_replaces_existing(db):
    """Second emit with same cancel_group replaces first deferred row."""
    emit({"event": "check.due", "delay": "5m", "cancel_group": "debounce-grp"}, db)
    emit({"event": "check.due", "delay": "10m", "cancel_group": "debounce-grp"}, db)
    rows = db.conn.execute("SELECT * FROM deferred_events").fetchall()
    assert len(rows) == 1  # only one row remains


def test_emit_different_cancel_groups_coexist(db):
    emit({"event": "a.event", "delay": "5m", "cancel_group": "grp-a"}, db)
    emit({"event": "b.event", "delay": "5m", "cancel_group": "grp-b"}, db)
    count = db.conn.execute("SELECT COUNT(*) FROM deferred_events").fetchone()[0]
    assert count == 2


def test_emit_without_cancel_group_accumulates(db):
    emit({"event": "x.event", "delay": "5m"}, db)
    emit({"event": "x.event", "delay": "5m"}, db)
    count = db.conn.execute("SELECT COUNT(*) FROM deferred_events").fetchone()[0]
    assert count == 2


# ---------------------------------------------------------------------------
# delay=0 or missing delay → immediate (no deferred row)
# ---------------------------------------------------------------------------

def test_emit_with_zero_delay_is_immediate(db):
    emit({"event": "now.event", "delay": "0s"}, db)
    deferred = db.conn.execute("SELECT COUNT(*) FROM deferred_events").fetchone()[0]
    assert deferred == 0
    rows = db.get_unprocessed()
    assert len(rows) == 1


# ---------------------------------------------------------------------------
# Error path tests (t-12)
# ---------------------------------------------------------------------------

def test_emit_deferred_no_db_returns_error():
    """Delayed emit with db=None must return error, not silent success."""
    handler = get_action_handler("emit")
    result = handler.run({"event": "check.due", "delay": "5m"}, event_payload={}, db=None)
    assert result["status"] == "error"
    assert "no database connection" in result["output"]


def test_emit_bad_template_returns_error(db):
    """Invalid Jinja2 template or non-JSON result must return error."""
    handler = get_action_handler("emit")
    # "{{ bad_filter | nonexistent_filter }}" will raise a Jinja2 UndefinedError
    result = handler.run(
        {"event": "check.due", "payload": "{{ event | nonexistent_filter }}"},
        event_payload={},
        db=db,
    )
    assert result["status"] == "error"
    assert "Template render failed" in result["output"]


def test_emit_missing_event_key_returns_error(db):
    """emit action without 'event' key must return error."""
    handler = get_action_handler("emit")
    result = handler.run({"payload": {"x": 1}}, event_payload={}, db=db)
    assert result["status"] == "error"
    assert "missing required 'event' parameter" in result["output"]
