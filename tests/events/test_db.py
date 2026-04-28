# tests/test_db.py
"""TDD tests for db.py v2 features: dedup_key, deferred_events, parse_duration."""
import os
import tempfile
from datetime import datetime, timedelta

import pytest

from db import EventsDB, parse_duration


@pytest.fixture
def db():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    d = EventsDB(path)
    yield d
    d.close()
    os.unlink(path)


# ---------------------------------------------------------------------------
# parse_duration
# ---------------------------------------------------------------------------

def test_parse_duration_seconds():
    assert parse_duration("30s") == 30

def test_parse_duration_minutes():
    assert parse_duration("10m") == 600

def test_parse_duration_hours():
    assert parse_duration("2h") == 7200

def test_parse_duration_days():
    assert parse_duration("1d") == 86400

def test_parse_duration_bare_int_treated_as_hours():
    # Backwards compat: bare integer treated as hours
    assert parse_duration("1") == 3600
    assert parse_duration("2") == 7200

def test_parse_duration_invalid_suffix():
    with pytest.raises(ValueError, match="10x"):
        parse_duration("10x")

def test_parse_duration_empty_string():
    with pytest.raises(ValueError, match="''"):
        parse_duration("")

def test_parse_duration_none_input():
    with pytest.raises(ValueError, match="None"):
        parse_duration(None)

def test_parse_duration_non_numeric_prefix():
    with pytest.raises(ValueError, match="xm"):
        parse_duration("xm")


# ---------------------------------------------------------------------------
# dedup_key on events table
# ---------------------------------------------------------------------------

def test_dedup_key_column_exists(db):
    """events table must have dedup_key column."""
    cols = [r[1] for r in db.conn.execute("PRAGMA table_info(events)").fetchall()]
    assert "dedup_key" in cols

def test_insert_event_without_dedup_key(db):
    """Normal insert still works."""
    eid = db.insert_event("test.event", "{}", "test-src")
    assert eid is not None
    rows = db.get_unprocessed()
    assert len(rows) == 1

def test_insert_event_with_dedup_key_first_time(db):
    """First insert with dedup_key succeeds."""
    eid = db.insert_event("test.event", "{}", "test-src", dedup_key="key-abc")
    assert eid is not None

def test_dedup_skips_if_already_processed(db):
    """Insert with dedup_key is skipped when a processed event has same key."""
    eid1 = db.insert_event("test.event", "{}", "test-src", dedup_key="key-xyz")
    assert eid1 is not None
    # Mark as processed
    db.mark_processed(eid1, "some-policy")
    # Second insert with same dedup_key should be skipped
    eid2 = db.insert_event("test.event", "{}", "test-src", dedup_key="key-xyz")
    assert eid2 is None
    # No new unprocessed events
    rows = db.get_unprocessed()
    assert len(rows) == 0

def test_dedup_allows_if_not_yet_processed(db):
    """Insert with dedup_key succeeds when existing event is still unprocessed."""
    eid1 = db.insert_event("test.event", "{}", "test-src", dedup_key="key-dup")
    # Do NOT mark processed — existing row is unprocessed
    eid2 = db.insert_event("test.event", "{}", "test-src", dedup_key="key-dup")
    # Second insert is allowed (spec: skip only if processed_at IS NOT NULL)
    assert eid2 is not None

def test_dedup_key_different_keys_both_insert(db):
    """Different dedup_keys are independent."""
    eid1 = db.insert_event("test.event", "{}", "src", dedup_key="key-1")
    db.mark_processed(eid1, "p")
    eid2 = db.insert_event("test.event", "{}", "src", dedup_key="key-2")
    assert eid2 is not None


# ---------------------------------------------------------------------------
# deferred_events table
# ---------------------------------------------------------------------------

def test_deferred_events_table_exists(db):
    cols = [r[1] for r in db.conn.execute("PRAGMA table_info(deferred_events)").fetchall()]
    for col in ("id", "event_type", "payload", "source", "fire_at", "cancel_group", "created_at"):
        assert col in cols, f"Missing column: {col}"

def test_insert_deferred(db):
    fire_at = (datetime.utcnow() + timedelta(minutes=10)).isoformat()
    db.insert_deferred("landings.check-due", "{}", "policy", fire_at)
    rows = db.conn.execute("SELECT * FROM deferred_events").fetchall()
    assert len(rows) == 1
    assert rows[0]["event_type"] == "landings.check-due"

def test_get_due_deferred_returns_past(db):
    past = (datetime.utcnow() - timedelta(seconds=5)).isoformat()
    future = (datetime.utcnow() + timedelta(minutes=5)).isoformat()
    db.insert_deferred("past.event", "{}", "src", past)
    db.insert_deferred("future.event", "{}", "src", future)
    due = db.get_due_deferred()
    assert len(due) == 1
    assert due[0]["event_type"] == "past.event"

def test_get_due_deferred_empty(db):
    future = (datetime.utcnow() + timedelta(minutes=5)).isoformat()
    db.insert_deferred("future.event", "{}", "src", future)
    assert db.get_due_deferred() == []

def test_delete_deferred(db):
    fire_at = (datetime.utcnow() + timedelta(minutes=1)).isoformat()
    db.insert_deferred("some.event", "{}", "src", fire_at)
    rows = db.conn.execute("SELECT * FROM deferred_events").fetchall()
    row_id = rows[0]["id"]
    db.delete_deferred(row_id)
    assert db.conn.execute("SELECT COUNT(*) FROM deferred_events").fetchone()[0] == 0

def test_cancel_group_replaces_pending(db):
    """Same cancel_group → old row replaced by new row."""
    fire_at1 = (datetime.utcnow() + timedelta(minutes=5)).isoformat()
    fire_at2 = (datetime.utcnow() + timedelta(minutes=10)).isoformat()
    db.insert_deferred("check.due", '{"v":1}', "src", fire_at1, cancel_group="landings-check")
    db.insert_deferred("check.due", '{"v":2}', "src", fire_at2, cancel_group="landings-check")
    rows = db.conn.execute("SELECT * FROM deferred_events").fetchall()
    assert len(rows) == 1
    assert rows[0]["fire_at"] == fire_at2

def test_cancel_group_none_does_not_replace(db):
    """Without cancel_group, multiple deferred events accumulate."""
    fire_at = (datetime.utcnow() + timedelta(minutes=5)).isoformat()
    db.insert_deferred("check.due", "{}", "src", fire_at, cancel_group=None)
    db.insert_deferred("check.due", "{}", "src", fire_at, cancel_group=None)
    rows = db.conn.execute("SELECT COUNT(*) FROM deferred_events").fetchone()
    assert rows[0] == 2

def test_cancel_group_different_groups_coexist(db):
    """Different cancel_groups are independent."""
    fire_at = (datetime.utcnow() + timedelta(minutes=5)).isoformat()
    db.insert_deferred("a.event", "{}", "src", fire_at, cancel_group="group-a")
    db.insert_deferred("b.event", "{}", "src", fire_at, cancel_group="group-b")
    rows = db.conn.execute("SELECT COUNT(*) FROM deferred_events").fetchone()
    assert rows[0] == 2


# ---------------------------------------------------------------------------
# count_events with seconds
# ---------------------------------------------------------------------------

def test_count_events_with_seconds(db):
    for _ in range(3):
        db.insert_event("ping.event", "{}", "src")
    count = db.count_events("ping.event", seconds=60)
    assert count == 3

def test_count_events_backwards_compat_hours(db):
    for _ in range(2):
        db.insert_event("pong.event", "{}", "src")
    count = db.count_events("pong.event", hours=1)
    assert count == 2

def test_count_events_zero_for_future_cutoff(db):
    db.insert_event("old.event", "{}", "src")
    # Update to past timestamp
    db.conn.execute("UPDATE events SET created_at = datetime('now', '-2 hours')")
    db.conn.commit()
    count = db.count_events("old.event", seconds=60)
    assert count == 0
