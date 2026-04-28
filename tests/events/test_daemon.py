# test_daemon.py
import json
import os
import tempfile
import time
import pytest
from db import EventsDB
from recipe import Recipe
from conditions import evaluate_conditions
from actions import get_action_handler

# Test the daemon's process_event function (extracted from daemon for testability)
from hex_eventd import process_event, match_policies as match_recipes

@pytest.fixture
def db():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    d = EventsDB(path)
    yield d
    d.close()
    os.unlink(path)

def make_recipe(name, event, actions=None, conditions=None):
    return Recipe.from_dict({
        "name": name,
        "trigger": {"event": event},
        "conditions": conditions or [],
        "actions": actions or [{"type": "shell", "command": "echo matched"}],
    })

def test_match_recipes_exact():
    recipes = [make_recipe("r1", "boi.spec.completed"), make_recipe("r2", "session.stop")]
    matched = match_recipes(recipes, "boi.spec.completed")
    assert len(matched) == 1
    assert matched[0].name == "r1"

def test_match_recipes_glob():
    recipes = [make_recipe("r1", "boi.spec.*")]
    matched = match_recipes(recipes, "boi.spec.completed")
    assert len(matched) == 1

def test_match_recipes_none():
    recipes = [make_recipe("r1", "boi.spec.completed")]
    matched = match_recipes(recipes, "session.stop")
    assert len(matched) == 0

def test_process_event_fires_action(db):
    recipes = [make_recipe("r1", "test.event")]
    eid = db.insert_event("test.event", '{"msg":"hi"}', "test")
    event = db.get_unprocessed()[0]
    process_event(event, recipes, db)
    # Event should be marked processed
    assert len(db.get_unprocessed()) == 0
    # Action should be logged
    logs = db.get_action_logs(eid)
    assert len(logs) == 1
    assert logs[0]["status"] == "success"

def test_process_event_invalid_json(db):
    recipes = [make_recipe("r1", "test.malformed")]
    eid = db.insert_event("test.malformed", "not-valid-json", "test")
    event = db.get_unprocessed()[0]
    process_event(event, recipes, db)
    # Event should be marked processed despite bad payload
    assert len(db.get_unprocessed()) == 0
    # No action should have been logged
    assert len(db.get_action_logs(eid)) == 0

def test_process_event_emit_action(db):
    """EmitAction should insert a chained event into the DB when db is passed."""
    recipe = Recipe.from_dict({
        "name": "chain-test",
        "trigger": {"event": "source.event"},
        "conditions": [],
        "actions": [{"type": "emit", "event": "chained.event", "payload": {}}],
    })
    db.insert_event("source.event", '{"x":1}', "test")
    event = db.get_unprocessed()[0]
    process_event(event, [recipe], db)
    # Source event should be processed (chained event will also be in unprocessed)
    unprocessed_types = [e["event_type"] for e in db.get_unprocessed()]
    assert "source.event" not in unprocessed_types
    # A chained event should have been inserted
    row = db.conn.execute(
        "SELECT * FROM events WHERE event_type='chained.event'"
    ).fetchone()
    assert row is not None
    assert row["source"] == "policy-emit"

def test_process_event_condition_blocks(db):
    recipes = [make_recipe("r1", "test.event",
        conditions=[{"field": "status", "op": "eq", "value": "failed"}])]
    eid = db.insert_event("test.event", '{"status":"completed"}', "test")
    event = db.get_unprocessed()[0]
    process_event(event, recipes, db)
    # Event marked processed but no action logged (condition didn't match)
    assert len(db.get_unprocessed()) == 0
    assert len(db.get_action_logs(eid)) == 0
