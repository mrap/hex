"""Tests for the boi-workspace-isolation policy.

Tests:
  1. Policy loads correctly (name, metadata, provides/requires, rules)
  2. Correct standing_orders and reflection_ids references
  3. Rule 1 triggers on boi.workspace.leak → policy.violation action
  4. Rule 2 triggers on boi.workspace.leak with count condition >= 3
  5. Rule 2 conditions are correct (count(boi.workspace.leak, 1h) gte 3)
  6. Both rules match boi.workspace.leak event type
"""
import os
import sys

import pytest

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, _REPO_ROOT)

from policy import load_policies

POLICY_FILE = os.path.join(_REPO_ROOT, "policies", "boi-lifecycle", "boi-workspace-isolation.yaml")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def load_workspace_isolation_policy():
    """Load the boi-workspace-isolation policy from the policies directory."""
    policies_dir = os.path.join(_REPO_ROOT, "policies")
    policies = load_policies(policies_dir)
    for p in policies:
        if p.name == "boi-workspace-isolation":
            return p
    raise RuntimeError("boi-workspace-isolation policy not found")


# ---------------------------------------------------------------------------
# Test 1: Policy file exists
# ---------------------------------------------------------------------------

def test_policy_file_exists():
    assert os.path.exists(POLICY_FILE), f"Policy file not found: {POLICY_FILE}"


# ---------------------------------------------------------------------------
# Test 2: Policy loads correctly
# ---------------------------------------------------------------------------

def test_policy_loads_correctly():
    policy = load_workspace_isolation_policy()

    assert policy.name == "boi-workspace-isolation"
    assert len(policy.rules) == 2


# ---------------------------------------------------------------------------
# Test 3: Metadata — standing_orders and reflection_ids
# ---------------------------------------------------------------------------

def test_policy_standing_orders():
    policy = load_workspace_isolation_policy()
    assert "26" in policy.standing_orders, (
        f"Expected standing_order '26', got {policy.standing_orders}"
    )


def test_policy_reflection_ids():
    policy = load_workspace_isolation_policy()
    assert "R-047" in policy.reflection_ids, (
        f"Expected reflection_id 'R-047', got {policy.reflection_ids}"
    )


# ---------------------------------------------------------------------------
# Test 4: provides/requires
# ---------------------------------------------------------------------------

def test_policy_provides_policy_violation():
    policy = load_workspace_isolation_policy()
    assert "policy.violation" in policy.provides.get("events", [])


def test_policy_requires_workspace_leak():
    policy = load_workspace_isolation_policy()
    assert "boi.workspace.leak" in policy.requires.get("events", [])


# ---------------------------------------------------------------------------
# Test 5: Rule 1 — emit-violation-on-leak
# ---------------------------------------------------------------------------

def test_rule1_name_and_trigger():
    policy = load_workspace_isolation_policy()
    rule1 = policy.rules[0]
    assert rule1.name == "emit-violation-on-leak"
    assert rule1.trigger_event == "boi.workspace.leak"


def test_rule1_no_conditions():
    """Rule 1 fires unconditionally on every leak."""
    policy = load_workspace_isolation_policy()
    rule1 = policy.rules[0]
    assert rule1.conditions == []


def test_rule1_emits_policy_violation():
    policy = load_workspace_isolation_policy()
    rule1 = policy.rules[0]
    emit_actions = [a for a in rule1.actions if a.type == "emit"]
    assert len(emit_actions) >= 1
    emitted_events = [a.params.get("event") for a in emit_actions]
    assert "policy.violation" in emitted_events


def test_rule1_violation_payload_has_policy_field():
    policy = load_workspace_isolation_policy()
    rule1 = policy.rules[0]
    emit_actions = [a for a in rule1.actions if a.type == "emit"]
    violation_action = next(
        (a for a in emit_actions if a.params.get("event") == "policy.violation"),
        None
    )
    assert violation_action is not None
    payload = violation_action.params.get("payload", {})
    assert payload.get("policy") == "boi-workspace-isolation"


def test_rule1_violation_payload_has_leaked_files():
    policy = load_workspace_isolation_policy()
    rule1 = policy.rules[0]
    emit_actions = [a for a in rule1.actions if a.type == "emit"]
    violation_action = next(
        (a for a in emit_actions if a.params.get("event") == "policy.violation"),
        None
    )
    assert violation_action is not None
    payload = violation_action.params.get("payload", {})
    assert "leaked_files" in payload


# ---------------------------------------------------------------------------
# Test 6: Rule 2 — notify-on-repeated-leaks
# ---------------------------------------------------------------------------

def test_rule2_name_and_trigger():
    policy = load_workspace_isolation_policy()
    rule2 = policy.rules[1]
    assert rule2.name == "notify-on-repeated-leaks"
    assert rule2.trigger_event == "boi.workspace.leak"


def test_rule2_has_count_condition():
    """Rule 2 must have a count(boi.workspace.leak, 1h) >= 3 condition."""
    policy = load_workspace_isolation_policy()
    rule2 = policy.rules[1]
    assert len(rule2.conditions) >= 1
    cond = rule2.conditions[0]
    assert "count(boi.workspace.leak" in cond.field
    assert "1h" in cond.field
    assert cond.op == "gte"
    assert cond.value == 3


def test_rule2_has_notify_action():
    policy = load_workspace_isolation_policy()
    rule2 = policy.rules[1]
    notify_actions = [a for a in rule2.actions if a.type == "notify"]
    assert len(notify_actions) >= 1


def test_rule2_has_emit_action():
    policy = load_workspace_isolation_policy()
    rule2 = policy.rules[1]
    emit_actions = [a for a in rule2.actions if a.type == "emit"]
    assert len(emit_actions) >= 1


# ---------------------------------------------------------------------------
# Test 7: Both rules match boi.workspace.leak
# ---------------------------------------------------------------------------

def test_all_rules_match_workspace_leak():
    policy = load_workspace_isolation_policy()
    for rule in policy.rules:
        assert rule.matches_event_type("boi.workspace.leak"), (
            f"Rule '{rule.name}' does not match 'boi.workspace.leak'"
        )


def test_rules_do_not_match_other_events():
    policy = load_workspace_isolation_policy()
    for rule in policy.rules:
        assert not rule.matches_event_type("boi.spec.completed")
        assert not rule.matches_event_type("policy.violation")
