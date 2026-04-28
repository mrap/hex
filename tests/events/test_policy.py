"""Tests for policy.py — Policy loader and rate limiting."""
import os
import sys
import time
import tempfile

import pytest
import yaml

# conftest.py adds parent dir to sys.path
from policy import Policy, Rule, load_policies, check_rate_limit, record_fire
from recipe import Condition, Action


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def write_yaml(d: str, fname: str, data: dict) -> str:
    path = os.path.join(d, fname)
    with open(path, "w") as f:
        yaml.dump(data, f)
    return path


# ---------------------------------------------------------------------------
# Policy loading — new format
# ---------------------------------------------------------------------------

def test_load_policy_new_format():
    with tempfile.TemporaryDirectory() as d:
        write_yaml(d, "my-policy.yaml", {
            "name": "my-policy",
            "description": "Test policy",
            "standing_orders": ["R-033"],
            "reflection_ids": ["abc"],
            "provides": {"events": ["policy.violation"]},
            "requires": {"events": ["git.commit"]},
            "rate_limit": {"max_fires": 5, "window": "1h"},
            "rules": [
                {
                    "name": "rule-1",
                    "trigger": {"event": "git.commit"},
                    "conditions": [
                        {"field": "branch", "op": "eq", "value": "main"}
                    ],
                    "actions": [
                        {"type": "emit", "event": "policy.violation"}
                    ],
                }
            ],
        })
        policies = load_policies(d)

    assert len(policies) == 1
    p = policies[0]
    assert p.name == "my-policy"
    assert p.description == "Test policy"
    assert p.standing_orders == ["R-033"]
    assert p.reflection_ids == ["abc"]
    assert p.provides == {"events": ["policy.violation"]}
    assert p.requires == {"events": ["git.commit"]}
    assert p.rate_limit == {"max_fires": 5, "window": "1h"}
    assert p.source_file is not None

    assert len(p.rules) == 1
    r = p.rules[0]
    assert r.name == "rule-1"
    assert r.trigger_event == "git.commit"
    assert len(r.conditions) == 1
    assert r.conditions[0].field == "branch"
    assert r.conditions[0].op == "eq"
    assert r.conditions[0].value == "main"
    assert len(r.actions) == 1
    assert r.actions[0].type == "emit"


def test_load_multiple_rules_in_policy():
    with tempfile.TemporaryDirectory() as d:
        write_yaml(d, "multi.yaml", {
            "name": "multi-rule",
            "rules": [
                {"name": "r1", "trigger": {"event": "a.b"}, "actions": [{"type": "shell", "command": "echo 1"}]},
                {"name": "r2", "trigger": {"event": "c.d"}, "actions": [{"type": "shell", "command": "echo 2"}]},
            ],
        })
        policies = load_policies(d)

    assert len(policies) == 1
    assert len(policies[0].rules) == 2
    assert policies[0].rules[0].trigger_event == "a.b"
    assert policies[0].rules[1].trigger_event == "c.d"


# ---------------------------------------------------------------------------
# Old recipe auto-wrap
# ---------------------------------------------------------------------------

def test_old_recipe_autowrap_basic():
    with tempfile.TemporaryDirectory() as d:
        write_yaml(d, "old.yaml", {
            "name": "landing-refresh",
            "trigger": {"event": "file.modified"},
            "conditions": [
                {"field": "path", "op": "contains", "value": "landings/"}
            ],
            "actions": [
                {"type": "shell", "command": "echo hello"}
            ],
        })
        policies = load_policies(d)

    assert len(policies) == 1
    p = policies[0]
    assert p.name == "landing-refresh"
    assert len(p.rules) == 1
    r = p.rules[0]
    assert r.trigger_event == "file.modified"
    assert len(r.conditions) == 1
    assert r.conditions[0].field == "path"
    assert len(r.actions) == 1
    assert r.actions[0].type == "shell"


def test_old_recipe_autowrap_no_conditions():
    with tempfile.TemporaryDirectory() as d:
        write_yaml(d, "simple.yaml", {
            "name": "simple",
            "trigger": {"event": "git.push"},
            "actions": [{"type": "shell", "command": "echo pushed"}],
        })
        policies = load_policies(d)

    p = policies[0]
    assert p.rules[0].trigger_event == "git.push"
    assert p.rules[0].conditions == []


def test_old_recipe_provides_requires_inferred_from_emit():
    """Auto-wrap should infer provides from emit actions, requires from trigger."""
    with tempfile.TemporaryDirectory() as d:
        write_yaml(d, "infer.yaml", {
            "name": "infer-test",
            "trigger": {"event": "git.commit"},
            "actions": [
                {"type": "emit", "event": "landings.check-due"},
                {"type": "shell", "command": "echo hi"},
            ],
        })
        policies = load_policies(d)

    p = policies[0]
    assert "git.commit" in p.requires.get("events", [])
    assert "landings.check-due" in p.provides.get("events", [])


# ---------------------------------------------------------------------------
# Provides / requires extraction
# ---------------------------------------------------------------------------

def test_provides_requires_explicit():
    with tempfile.TemporaryDirectory() as d:
        write_yaml(d, "p.yaml", {
            "name": "p",
            "provides": {"events": ["x.done", "y.ready"]},
            "requires": {"events": ["a.start"]},
            "rules": [{"name": "r", "trigger": {"event": "a.start"}, "actions": [{"type": "notify"}]}],
        })
        policies = load_policies(d)

    p = policies[0]
    assert p.provides["events"] == ["x.done", "y.ready"]
    assert p.requires["events"] == ["a.start"]


# ---------------------------------------------------------------------------
# Invalid files skipped gracefully
# ---------------------------------------------------------------------------

def test_invalid_yaml_skipped():
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "bad.yaml")
        with open(path, "w") as f:
            f.write(": [invalid yaml")
        # valid one alongside
        write_yaml(d, "good.yaml", {
            "name": "good",
            "trigger": {"event": "x.y"},
            "actions": [{"type": "shell", "command": "echo ok"}],
        })
        policies = load_policies(d)

    assert len(policies) == 1
    assert policies[0].name == "good"


def test_missing_fields_skipped():
    with tempfile.TemporaryDirectory() as d:
        write_yaml(d, "incomplete.yaml", {"name": "no-rules-no-trigger"})
        write_yaml(d, "ok.yaml", {
            "name": "ok",
            "trigger": {"event": "a.b"},
            "actions": [{"type": "shell", "command": "echo ok"}],
        })
        policies = load_policies(d)

    assert len(policies) == 1
    assert policies[0].name == "ok"


# ---------------------------------------------------------------------------
# Rate limit enforcement
# ---------------------------------------------------------------------------

def test_rate_limit_not_exceeded():
    policy = Policy(
        name="test",
        rules=[],
        rate_limit={"max_fires": 3, "window": "1m"},
    )
    for _ in range(3):
        assert check_rate_limit(policy) is True
        record_fire(policy)

    # 3 fires in 1 minute — next should be blocked
    assert check_rate_limit(policy) is False


def test_rate_limit_none_means_unlimited():
    policy = Policy(name="test", rules=[], rate_limit=None)
    for _ in range(100):
        assert check_rate_limit(policy) is True
        record_fire(policy)


def test_rate_limit_window_expiry():
    """Fires outside the window don't count."""
    policy = Policy(
        name="test",
        rules=[],
        rate_limit={"max_fires": 2, "window": "1s"},
    )
    # Record 2 fires 2 seconds ago (outside 1s window)
    old_ts = time.time() - 2.0
    policy.last_fires = [old_ts, old_ts]

    # Should be allowed now because old fires are outside window
    assert check_rate_limit(policy) is True


def test_rate_limit_mixed_inside_outside_window():
    policy = Policy(
        name="test",
        rules=[],
        rate_limit={"max_fires": 2, "window": "5s"},
    )
    # 1 old fire (outside window), 1 recent fire (inside window)
    policy.last_fires = [time.time() - 10.0, time.time() - 1.0]

    # Only 1 fire inside window, max is 2 → allowed
    assert check_rate_limit(policy) is True
    record_fire(policy)

    # Now 2 fires inside window → blocked
    assert check_rate_limit(policy) is False


# ---------------------------------------------------------------------------
# Rule glob matching
# ---------------------------------------------------------------------------

def test_rule_matches_event_type_exact():
    rule = Rule(name="r", trigger_event="git.push", actions=[])
    assert rule.matches_event_type("git.push") is True
    assert rule.matches_event_type("git.commit") is False


def test_rule_matches_event_type_glob():
    rule = Rule(name="r", trigger_event="git.*", actions=[])
    assert rule.matches_event_type("git.push") is True
    assert rule.matches_event_type("git.commit") is True
    assert rule.matches_event_type("file.modified") is False


# ---------------------------------------------------------------------------
# Empty directory
# ---------------------------------------------------------------------------

def test_empty_dir_returns_empty_list():
    with tempfile.TemporaryDirectory() as d:
        policies = load_policies(d)
    assert policies == []
