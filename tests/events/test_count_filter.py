"""Test count() condition with optional payload filter."""
import json
import pytest
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))

from db import EventsDB
from policy import Condition
from conditions import evaluate_conditions, COUNT_RE


@pytest.fixture
def db(tmp_path):
    d = EventsDB(str(tmp_path / "test.db"))
    d.insert_event("policy.violation",
                   json.dumps({"rule": "R-033", "msg": "stale"}), "test")
    d.insert_event("policy.violation",
                   json.dumps({"rule": "R-033", "msg": "stale2"}), "test")
    d.insert_event("policy.violation",
                   json.dumps({"rule": "R-047", "msg": "missing"}), "test")
    d.insert_event("policy.violation",
                   json.dumps({"rule": "R-047", "msg": "missing2"}), "test")
    d.insert_event("policy.violation",
                   json.dumps({"rule": "R-047", "msg": "missing3"}), "test")
    return d


def test_count_re_matches_with_filter():
    m = COUNT_RE.match("count(policy.violation, 1h, rule=R-033)")
    assert m is not None
    assert m.group(1) == "policy.violation"
    assert m.group(2) == "1h"
    assert m.group(3) == "rule"
    assert m.group(4) == "R-033"


def test_count_re_matches_without_filter():
    m = COUNT_RE.match("count(policy.violation, 1h)")
    assert m is not None
    assert m.group(1) == "policy.violation"
    assert m.group(2) == "1h"
    assert m.group(3) is None


def test_count_with_filter_returns_filtered_count(db):
    conds = [Condition(field="count(policy.violation, 1h, rule=R-033)",
                       op="gte", value=2)]
    assert evaluate_conditions(conds, {}, db) is True

    conds2 = [Condition(field="count(policy.violation, 1h, rule=R-033)",
                        op="gte", value=3)]
    assert evaluate_conditions(conds2, {}, db) is False


def test_count_without_filter_returns_total(db):
    conds = [Condition(field="count(policy.violation, 1h)",
                       op="gte", value=5)]
    assert evaluate_conditions(conds, {}, db) is True


def test_db_count_events_with_payload_filter(db):
    count = db.count_events("policy.violation", seconds=3600,
                            payload_filter=("rule", "R-047"))
    assert count == 3
    count2 = db.count_events("policy.violation", seconds=3600,
                             payload_filter=("rule", "R-033"))
    assert count2 == 2
