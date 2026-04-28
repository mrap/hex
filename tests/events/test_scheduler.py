"""Tests for scheduler adapter (t-5). TDD — written before implementation."""
import json
import os
import sys
import tempfile
from datetime import datetime

import pytest

sys.path.insert(0, os.path.expanduser("~/.hex-events"))

from db import EventsDB
from adapters.scheduler import SchedulerAdapter, _iso_minute


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


@pytest.fixture
def scheduler_config(tmp_path):
    config = tmp_path / "scheduler.yaml"
    config.write_text(
        "schedules:\n"
        "  - name: test-30m\n"
        "    cron: '*/30 * * * *'\n"
        "    event: timer.tick.30m\n"
        "  - name: test-1h\n"
        "    cron: '0 * * * *'\n"
        "    event: timer.tick.1h\n"
    )
    return str(config)


# ---------------------------------------------------------------------------
# Config loading
# ---------------------------------------------------------------------------

def test_scheduler_loads_config(scheduler_config):
    s = SchedulerAdapter(config_path=scheduler_config)
    assert len(s.schedules) == 2
    assert s.schedules[0]["event"] == "timer.tick.30m"
    assert s.schedules[1]["event"] == "timer.tick.1h"


def test_scheduler_empty_if_no_file(tmp_path):
    path = str(tmp_path / "nonexistent.yaml")
    s = SchedulerAdapter(config_path=path)
    assert s.schedules == []


# ---------------------------------------------------------------------------
# Cron evaluation correctness
# ---------------------------------------------------------------------------

def test_iso_minute_format():
    dt = datetime(2026, 3, 19, 14, 7, 33)
    assert _iso_minute(dt) == "2026-03-19T14:07"


def test_cron_evaluation_30m():
    from croniter import croniter
    now = datetime(2026, 3, 19, 14, 7, 33)
    cron = croniter("*/30 * * * *", now)
    last = cron.get_prev(datetime)
    assert _iso_minute(last) == "2026-03-19T14:00"


def test_cron_evaluation_30m_second_window():
    from croniter import croniter
    now = datetime(2026, 3, 19, 14, 31, 0)
    cron = croniter("*/30 * * * *", now)
    last = cron.get_prev(datetime)
    assert _iso_minute(last) == "2026-03-19T14:30"


# ---------------------------------------------------------------------------
# tick() — emitting timer events
# ---------------------------------------------------------------------------

def test_tick_emits_timer_event(db, scheduler_config):
    s = SchedulerAdapter(config_path=scheduler_config)
    now = datetime(2026, 3, 19, 14, 5, 0)
    emitted = s.tick(db, now=now)
    assert "timer.tick.30m" in emitted
    rows = db.get_unprocessed()
    types = {r["event_type"] for r in rows}
    assert "timer.tick.30m" in types


def test_tick_dedup_key_format(db, scheduler_config):
    s = SchedulerAdapter(config_path=scheduler_config)
    now = datetime(2026, 3, 19, 14, 5, 0)
    s.tick(db, now=now)
    row = db.conn.execute(
        "SELECT dedup_key FROM events WHERE event_type = 'timer.tick.30m'"
    ).fetchone()
    assert row is not None
    assert row["dedup_key"] == "timer.tick.30m:2026-03-19T14:00"


def test_tick_no_double_emit_within_window(db, scheduler_config):
    """Multiple daemon ticks within the same cron window only emit once."""
    s = SchedulerAdapter(config_path=scheduler_config)
    now1 = datetime(2026, 3, 19, 14, 5, 0)
    s.tick(db, now=now1)
    now2 = datetime(2026, 3, 19, 14, 5, 2)  # 2s later, same 30m window
    s.tick(db, now=now2)
    count = db.conn.execute(
        "SELECT COUNT(*) FROM events WHERE event_type = 'timer.tick.30m'"
    ).fetchone()[0]
    assert count == 1


def test_tick_emits_again_for_new_window(db, scheduler_config):
    """A new cron window triggers a new emit."""
    s = SchedulerAdapter(config_path=scheduler_config)
    now1 = datetime(2026, 3, 19, 14, 5, 0)   # window: 14:00
    s.tick(db, now=now1)
    # Mark first event as processed so dedup allows next window
    db.conn.execute("UPDATE events SET processed_at = datetime('now')")
    db.conn.commit()
    now2 = datetime(2026, 3, 19, 14, 35, 0)  # window: 14:30
    s.tick(db, now=now2)
    count = db.conn.execute(
        "SELECT COUNT(*) FROM events WHERE event_type = 'timer.tick.30m'"
    ).fetchone()[0]
    assert count == 2


def test_tick_source_is_scheduler(db, scheduler_config):
    s = SchedulerAdapter(config_path=scheduler_config)
    s.tick(db, now=datetime(2026, 3, 19, 14, 5, 0))
    row = db.conn.execute(
        "SELECT source FROM events WHERE event_type = 'timer.tick.30m'"
    ).fetchone()
    assert row["source"] == "scheduler"


def test_tick_payload_contains_scheduled_at(db, scheduler_config):
    s = SchedulerAdapter(config_path=scheduler_config)
    s.tick(db, now=datetime(2026, 3, 19, 14, 5, 0))
    row = db.conn.execute(
        "SELECT payload FROM events WHERE event_type = 'timer.tick.30m'"
    ).fetchone()
    payload = json.loads(row["payload"])
    assert payload["scheduled_at"] == "2026-03-19T14:00"


def test_tick_no_crash_with_empty_schedules(db, tmp_path):
    config = tmp_path / "empty.yaml"
    config.write_text("schedules: []\n")
    s = SchedulerAdapter(config_path=str(config))
    emitted = s.tick(db)
    assert emitted == []


def test_tick_returns_both_schedule_events(db, scheduler_config):
    s = SchedulerAdapter(config_path=scheduler_config)
    # 14:00 is on the hour, so both 30m and 1h should fire
    now = datetime(2026, 3, 19, 14, 1, 0)
    emitted = s.tick(db, now=now)
    assert "timer.tick.30m" in emitted
    assert "timer.tick.1h" in emitted


# ---------------------------------------------------------------------------
# startup_catchup() — catch-up on restart
# ---------------------------------------------------------------------------

def test_startup_catchup_emits_missed_tick(db, scheduler_config):
    """On startup with fresh DB, emit catch-up for the last missed tick."""
    s = SchedulerAdapter(config_path=scheduler_config)
    now = datetime(2026, 3, 19, 14, 5, 0)
    emitted = s.startup_catchup(db, now=now)
    assert "timer.tick.30m" in emitted
    rows = db.get_unprocessed()
    types = {r["event_type"] for r in rows}
    assert "timer.tick.30m" in types


def test_startup_catchup_noop_if_already_processed(db, scheduler_config):
    """On restart, don't re-emit if tick was already emitted and processed."""
    s = SchedulerAdapter(config_path=scheduler_config)
    now = datetime(2026, 3, 19, 14, 5, 0)
    # First session: tick() emitted the event, daemon processed it
    s.tick(db, now=now)
    db.conn.execute("UPDATE events SET processed_at = datetime('now')")
    db.conn.commit()
    # New session: startup_catchup should skip (already processed)
    s2 = SchedulerAdapter(config_path=scheduler_config)
    emitted = s2.startup_catchup(db, now=now)
    assert "timer.tick.30m" not in emitted
    count = db.conn.execute(
        "SELECT COUNT(*) FROM events WHERE event_type = 'timer.tick.30m'"
    ).fetchone()[0]
    assert count == 1  # no duplicate


def test_startup_catchup_noop_if_already_queued(db, scheduler_config):
    """Don't re-emit if tick exists but not yet processed (still in queue)."""
    s = SchedulerAdapter(config_path=scheduler_config)
    now = datetime(2026, 3, 19, 14, 5, 0)
    s.tick(db, now=now)  # emit but don't process
    s2 = SchedulerAdapter(config_path=scheduler_config)
    emitted = s2.startup_catchup(db, now=now)
    assert "timer.tick.30m" not in emitted
    count = db.conn.execute(
        "SELECT COUNT(*) FROM events WHERE event_type = 'timer.tick.30m'"
    ).fetchone()[0]
    assert count == 1


def test_startup_catchup_is_emit_latest_only(db, scheduler_config):
    """Even if many ticks were missed, only the latest tick is emitted."""
    s = SchedulerAdapter(config_path=scheduler_config)
    # now is 5 hours after scheduler was last active; many 30m ticks missed
    now = datetime(2026, 3, 19, 19, 5, 0)
    s.startup_catchup(db, now=now)
    count = db.conn.execute(
        "SELECT COUNT(*) FROM events WHERE event_type = 'timer.tick.30m'"
    ).fetchone()[0]
    assert count == 1  # only 1 catch-up, not 10


def test_startup_catchup_catchup_flag_in_payload(db, scheduler_config):
    s = SchedulerAdapter(config_path=scheduler_config)
    now = datetime(2026, 3, 19, 14, 5, 0)
    s.startup_catchup(db, now=now)
    row = db.conn.execute(
        "SELECT payload FROM events WHERE source = 'scheduler-catchup'"
    ).fetchone()
    assert row is not None
    payload = json.loads(row["payload"])
    assert payload.get("catchup") is True


# ---------------------------------------------------------------------------
# Dedup hardening — duplicate YAML entries (t-2 GREEN tests)
# ---------------------------------------------------------------------------

def test_duplicate_schedule_entries_deduped(db, tmp_path):
    """GREEN test: duplicate YAML entries emit exactly once due to _load() dedup."""
    config = tmp_path / "dup_scheduler.yaml"
    config.write_text(
        "schedules:\n"
        "  - name: test-dup-1\n"
        "    cron: '* * * * *'\n"
        "    event: timer.tick.test-dup\n"
        "  - name: test-dup-2\n"
        "    cron: '* * * * *'\n"
        "    event: timer.tick.test-dup\n"
    )
    s = SchedulerAdapter(config_path=str(config))
    now = datetime(2026, 3, 19, 14, 5, 0)
    result = s.tick(db, now=now)
    assert len(result) == 1  # dedup keeps only the first occurrence


def test_tick_emits_once_per_window(db, scheduler_config):
    """tick() called twice in the same cron window emits 0 events on second call."""
    s = SchedulerAdapter(config_path=scheduler_config)
    now = datetime(2026, 3, 19, 14, 5, 0)
    first = s.tick(db, now=now)
    assert len(first) > 0
    second = s.tick(db, now=now)
    assert len(second) == 0


# ---------------------------------------------------------------------------
# Config validation — duplicate schedule entries (t-3)
# ---------------------------------------------------------------------------

def test_load_warns_on_duplicate_schedules(tmp_path, caplog):
    """_load() deduplicates duplicate event entries and logs a warning."""
    import logging
    config = tmp_path / "dup_scheduler.yaml"
    config.write_text(
        "schedules:\n"
        "  - name: test-dup-1\n"
        "    cron: '* * * * *'\n"
        "    event: timer.tick.test-dup\n"
        "  - name: test-dup-2\n"
        "    cron: '* * * * *'\n"
        "    event: timer.tick.test-dup\n"
        "  - name: test-unique\n"
        "    cron: '0 * * * *'\n"
        "    event: timer.tick.unique\n"
    )
    with caplog.at_level(logging.WARNING, logger="hex-events"):
        s = SchedulerAdapter(config_path=str(config))
    assert len(s.schedules) == 2  # 3 raw entries, 2 after dedup
    assert any("duplicate" in msg.lower() for msg in caplog.messages)
