# test_recipe.py
import os
import tempfile
import pytest
from recipe import Recipe, load_recipes

def test_parse_recipe():
    r = Recipe.from_dict({
        "name": "test",
        "trigger": {"event": "test.event"},
        "conditions": [{"field": "status", "op": "eq", "value": "done"}],
        "actions": [{"type": "shell", "command": "echo hi"}],
    })
    assert r.name == "test"
    assert r.trigger_event == "test.event"
    assert len(r.conditions) == 1
    assert len(r.actions) == 1

def test_parse_recipe_no_conditions():
    r = Recipe.from_dict({
        "name": "simple",
        "trigger": {"event": "ping"},
        "actions": [{"type": "shell", "command": "echo pong"}],
    })
    assert r.conditions == []

def test_matches_event_type():
    r = Recipe.from_dict({
        "name": "test",
        "trigger": {"event": "boi.spec.*"},
        "actions": [{"type": "shell", "command": "echo"}],
    })
    assert r.matches_event_type("boi.spec.completed")
    assert r.matches_event_type("boi.spec.failed")
    assert not r.matches_event_type("session.stop")

def test_load_recipes_from_dir(tmp_path):
    (tmp_path / "r1.yaml").write_text(
        "name: r1\ntrigger:\n  event: a\nactions:\n  - type: shell\n    command: echo\n"
    )
    (tmp_path / "r2.yaml").write_text(
        "name: r2\ntrigger:\n  event: b\nactions:\n  - type: shell\n    command: echo\n"
    )
    recipes = load_recipes(str(tmp_path))
    assert len(recipes) == 2

def test_invalid_recipe_skipped(tmp_path):
    (tmp_path / "bad.yaml").write_text("not: a: valid: recipe\n")
    (tmp_path / "good.yaml").write_text(
        "name: good\ntrigger:\n  event: x\nactions:\n  - type: shell\n    command: echo\n"
    )
    recipes = load_recipes(str(tmp_path))
    assert len(recipes) == 1
