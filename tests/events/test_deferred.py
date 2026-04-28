# tests/test_deferred.py
"""TDD tests for daemon deferred event drain logic."""
import json
import os
import sys
import tempfile
from datetime import datetime, timedelta

import pytest

sys.path.insert(0, os.path.expanduser("~/.hex-events"))

from db import EventsDB
from hex_eventd import drain_deferred


@pytest.fixture
def db():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    d = EventsDB(path)
    yield d
    d.close()
    os.unlink(path)


@pytest.fixture
def db_path():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    yield path
    if os.path.exists(path):
        os.unlink(path)


# ---------------------------------------------------------------------------
# drain_deferred promotes due events to events table
# ---------------------------------------------------------------------------

def test_drain_deferred_promotes_due_event(db):
    past = (datetime.utcnow() - timedelta(seconds=5)).isoformat()
    db.insert_deferred("landings.check-due", '{"from": "deferred"}', "policy-emit", past)
    drain_deferred(db)
    rows = db.get_unprocessed()
    assert len(rows) == 1
    assert rows[0]["event_type"] == "landings.check-due"
    payload = json.loads(rows[0]["payload"])
    assert payload == {"from": "deferred"}


def test_drain_deferred_does_not_promote_future_events(db):
    future = (datetime.utcnow() + timedelta(minutes=5)).isoformat()
    db.insert_deferred("future.event", "{}", "src", future)
    drain_deferred(db)
    assert db.get_unprocessed() == []
    deferred = db.conn.execute("SELECT COUNT(*) FROM deferred_events").fetchone()[0]
    assert deferred == 1  # still there


def test_drain_deferred_deletes_from_deferred_after_promoting(db):
    past = (datetime.utcnow() - timedelta(seconds=1)).isoformat()
    db.insert_deferred("check.due", "{}", "src", past)
    drain_deferred(db)
    deferred_count = db.conn.execute("SELECT COUNT(*) FROM deferred_events").fetchone()[0]
    assert deferred_count == 0


def test_drain_deferred_dual_write_safety(db):
    """delete from deferred first, then insert to events — verified by outcome."""
    past = (datetime.utcnow() - timedelta(seconds=1)).isoformat()
    db.insert_deferred("safety.event", "{}", "src", past)
    drain_deferred(db)
    # After drain: deferred empty, events has the row
    assert db.conn.execute("SELECT COUNT(*) FROM deferred_events").fetchone()[0] == 0
    assert len(db.get_unprocessed()) == 1


def test_drain_deferred_multiple_due_events(db):
    past = (datetime.utcnow() - timedelta(seconds=1)).isoformat()
    db.insert_deferred("event.a", "{}", "src", past)
    db.insert_deferred("event.b", "{}", "src", past)
    db.insert_deferred("event.c", "{}", "src", past)
    drain_deferred(db)
    rows = db.get_unprocessed()
    types = {r["event_type"] for r in rows}
    assert types == {"event.a", "event.b", "event.c"}
    assert db.conn.execute("SELECT COUNT(*) FROM deferred_events").fetchone()[0] == 0


def test_drain_deferred_empty_is_noop(db):
    # No deferred events; should not raise
    drain_deferred(db)
    assert db.get_unprocessed() == []


def test_drain_deferred_preserves_source(db):
    past = (datetime.utcnow() - timedelta(seconds=1)).isoformat()
    db.insert_deferred("src.event", "{}", "policy-emit", past)
    drain_deferred(db)
    row = db.conn.execute("SELECT source FROM events").fetchone()
    assert row["source"] == "policy-emit"


# ---------------------------------------------------------------------------
# Restart recovery: deferred events persisted in SQLite survive restart
# ---------------------------------------------------------------------------

def test_restart_recovery_deferred_events_persist(db_path):
    """Deferred events written in one DB session survive to a new session."""
    future = (datetime.utcnow() + timedelta(minutes=5)).isoformat()
    d1 = EventsDB(db_path)
    d1.insert_deferred("restart.event", '{"key": "val"}', "policy-emit", future)
    d1.close()

    # Simulate restart: new EventsDB instance
    d2 = EventsDB(db_path)
    rows = d2.conn.execute("SELECT * FROM deferred_events").fetchall()
    d2.close()
    assert len(rows) == 1
    assert rows[0]["event_type"] == "restart.event"


def test_restart_recovery_due_events_drained(db_path):
    """On restart, deferred events that became due are drained correctly."""
    past = (datetime.utcnow() - timedelta(seconds=5)).isoformat()
    d1 = EventsDB(db_path)
    d1.insert_deferred("overdue.event", "{}", "src", past)
    d1.close()

    # Simulate restart: new session drains on first tick
    d2 = EventsDB(db_path)
    drain_deferred(d2)
    rows = d2.get_unprocessed()
    d2.close()
    assert len(rows) == 1
    assert rows[0]["event_type"] == "overdue.event"


# ---------------------------------------------------------------------------
# cancel_group debounce end-to-end
# ---------------------------------------------------------------------------

def test_cancel_group_debounce_last_write_wins(db):
    """Repeated deferred emits with same cancel_group: only the last one fires."""
    past = (datetime.utcnow() - timedelta(seconds=5)).isoformat()
    # First emit
    db.insert_deferred("check.due", '{"seq":1}', "src", past, cancel_group="grp")
    # Second emit replaces first
    db.insert_deferred("check.due", '{"seq":2}', "src", past, cancel_group="grp")
    drain_deferred(db)
    rows = db.get_unprocessed()
    assert len(rows) == 1
    assert json.loads(rows[0]["payload"]) == {"seq": 2}
