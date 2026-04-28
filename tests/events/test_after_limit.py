"""Tests for after_limit policy field (t-3 and t-4).

Verifies:
- after_limit: delete + max_fires: 1 → file deleted when limit reached
- after_limit: disable + max_fires: 1 → file remains with enabled: false
- after_limit: delete + ttl → file deleted when TTL expires
- after_limit: disable (default) + ttl → file remains, rule skipped
"""
import json
import os
import sys
import tempfile
import time

import pytest
import yaml

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from db import EventsDB
from policy import load_policies
from hex_eventd import _process_event_policies


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _make_db(tmp_dir: str) -> EventsDB:
    return EventsDB(os.path.join(tmp_dir, "test.db"))


def _emit(db: EventsDB, event_type: str, payload: dict = None) -> int:
    return db.insert_event(event_type, json.dumps(payload or {}), "test")


def _tick(db: EventsDB, policies: list) -> int:
    events = db.get_unprocessed()
    for event in events:
        _process_event_policies(event, policies, db)
    return len(events)


def _write_policy(policies_dir: str, name: str, data: dict) -> str:
    path = os.path.join(policies_dir, f"{name}.yaml")
    with open(path, "w") as f:
        yaml.dump(data, f)
    return path


def _make_policy_dict(name: str, event: str, max_fires: int = None,
                      after_limit: str = None, ttl: str = None) -> dict:
    rule = {
        "name": f"{name}-rule",
        "trigger": {"event": event},
        "actions": [{"type": "shell", "command": "echo hello"}],
    }
    if ttl:
        rule["ttl"] = ttl
    data = {
        "name": name,
        "rules": [rule],
    }
    if max_fires is not None:
        data["max_fires"] = max_fires
    if after_limit is not None:
        data["after_limit"] = after_limit
    return data


# ---------------------------------------------------------------------------
# max_fires + after_limit tests
# ---------------------------------------------------------------------------

def test_after_limit_delete_removes_file_on_max_fires():
    """after_limit: delete + max_fires: 1 → file deleted after the limit is reached."""
    with tempfile.TemporaryDirectory() as tmp:
        policies_dir = os.path.join(tmp, "policies")
        os.makedirs(policies_dir)
        db = _make_db(tmp)

        data = _make_policy_dict("ephemeral", "test.ping",
                                 max_fires=1, after_limit="delete")
        policy_path = _write_policy(policies_dir, "ephemeral", data)
        assert os.path.exists(policy_path)

        policies = load_policies(policies_dir)
        _emit(db, "test.ping")
        _tick(db, policies)

        assert not os.path.exists(policy_path), (
            "Policy file should be deleted when max_fires reached and after_limit=delete"
        )


def test_after_limit_disable_leaves_file_disabled_on_max_fires():
    """after_limit: disable + max_fires: 1 → file remains with enabled: false."""
    with tempfile.TemporaryDirectory() as tmp:
        policies_dir = os.path.join(tmp, "policies")
        os.makedirs(policies_dir)
        db = _make_db(tmp)

        data = _make_policy_dict("bounded", "test.ping",
                                 max_fires=1, after_limit="disable")
        policy_path = _write_policy(policies_dir, "bounded", data)

        policies = load_policies(policies_dir)
        _emit(db, "test.ping")
        _tick(db, policies)

        assert os.path.exists(policy_path), (
            "Policy file should still exist when after_limit=disable"
        )
        with open(policy_path) as f:
            updated = yaml.safe_load(f)
        assert updated.get("enabled") is False, (
            "Policy should have enabled: false when max_fires reached with after_limit=disable"
        )


def test_after_limit_default_disable_on_max_fires():
    """after_limit defaults to disable — same behavior as existing max_fires tests."""
    with tempfile.TemporaryDirectory() as tmp:
        policies_dir = os.path.join(tmp, "policies")
        os.makedirs(policies_dir)
        db = _make_db(tmp)

        # No after_limit field → default behavior
        data = _make_policy_dict("default-policy", "test.ping", max_fires=1)
        policy_path = _write_policy(policies_dir, "default-policy", data)

        policies = load_policies(policies_dir)
        _emit(db, "test.ping")
        _tick(db, policies)

        assert os.path.exists(policy_path), "File should remain with default after_limit"
        with open(policy_path) as f:
            updated = yaml.safe_load(f)
        assert updated.get("enabled") is False


# ---------------------------------------------------------------------------
# TTL + after_limit tests (t-4)
# ---------------------------------------------------------------------------

def _seed_rule_first_fire(db: EventsDB, policy_name: str, rule_name: str, ts: str):
    """Directly insert a past action_taken=1 eval row to simulate prior firing."""
    db.conn.execute(
        "INSERT INTO policy_eval_log "
        "(event_id, policy_name, rule_name, matched, conditions_passed, "
        "condition_details, rate_limited, action_taken, evaluated_at, workflow) "
        "VALUES (?, ?, ?, 1, 1, NULL, 0, 1, ?, NULL)",
        ("seed-event", policy_name, rule_name, ts),
    )
    db.conn.commit()


def _past_ts(seconds_ago: int = 10) -> str:
    """Return an ISO timestamp N seconds in the past (UTC, no tzinfo — matches DB format)."""
    from datetime import datetime, timedelta
    return (datetime.utcnow() - timedelta(seconds=seconds_ago)).isoformat()


def test_after_limit_delete_removes_file_on_ttl_expiry():
    """after_limit: delete + expired TTL → file deleted when TTL has passed."""
    with tempfile.TemporaryDirectory() as tmp:
        policies_dir = os.path.join(tmp, "policies")
        os.makedirs(policies_dir)
        db = _make_db(tmp)

        data = _make_policy_dict("ttl-ephemeral", "test.ping",
                                 after_limit="delete", ttl="1s")
        policy_path = _write_policy(policies_dir, "ttl-ephemeral", data)

        # Seed a first-fire timestamp 10 seconds ago so the 1s TTL is clearly expired
        _seed_rule_first_fire(db, "ttl-ephemeral", "ttl-ephemeral-rule", _past_ts(10))

        policies = load_policies(policies_dir)
        _emit(db, "test.ping")
        _tick(db, policies)

        assert not os.path.exists(policy_path), (
            "Policy file should be deleted when TTL expired and after_limit=delete"
        )


def test_after_limit_disable_leaves_file_on_ttl_expiry():
    """after_limit: disable (default) + expired TTL → file remains, rule just skipped."""
    with tempfile.TemporaryDirectory() as tmp:
        policies_dir = os.path.join(tmp, "policies")
        os.makedirs(policies_dir)
        db = _make_db(tmp)

        data = _make_policy_dict("ttl-bounded", "test.ping",
                                 after_limit="disable", ttl="1s")
        policy_path = _write_policy(policies_dir, "ttl-bounded", data)

        _seed_rule_first_fire(db, "ttl-bounded", "ttl-bounded-rule", _past_ts(10))

        policies = load_policies(policies_dir)
        _emit(db, "test.ping")
        _tick(db, policies)

        assert os.path.exists(policy_path), (
            "Policy file should remain when TTL expired but after_limit=disable"
        )
