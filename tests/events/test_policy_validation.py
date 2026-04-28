"""Tests for policy validation (t-1: RED phase).

These tests MUST FAIL until policy_validator.py is implemented.
"""
import pytest
from policy_validator import validate_policy


# --- Fixtures ---

VALID_POLICY = {
    "name": "my-policy",
    "rules": [
        {
            "name": "rule-one",
            "trigger": {"event": "boi.completed"},
            "actions": [{"type": "shell", "command": "echo hello"}],
        }
    ],
}


# --- Tests ---

def test_valid_policy_loads_without_error():
    errors = validate_policy(VALID_POLICY, "policies/valid.yaml")
    assert errors == [], f"Expected no errors, got: {errors}"


def test_missing_name_raises_error():
    policy = {"rules": [{"name": "r", "trigger": {"event": "x"}, "actions": [{"type": "notify"}]}]}
    errors = validate_policy(policy, "policies/test.yaml")
    assert any("name" in e for e in errors), f"Expected 'name' error, got: {errors}"


def test_missing_rules_raises_error():
    policy = {"name": "no-rules-policy"}
    errors = validate_policy(policy, "policies/test.yaml")
    assert any("rules" in e for e in errors), f"Expected 'rules' error, got: {errors}"


def test_rule_missing_trigger_raises_error():
    policy = {
        "name": "p",
        "rules": [{"name": "r", "actions": [{"type": "notify"}]}],
    }
    errors = validate_policy(policy, "policies/test.yaml")
    assert any("trigger" in e for e in errors), f"Expected 'trigger' error, got: {errors}"


def test_rule_missing_actions_raises_error():
    policy = {
        "name": "p",
        "rules": [{"name": "r", "trigger": {"event": "x"}}],
    }
    errors = validate_policy(policy, "policies/test.yaml")
    assert any("actions" in e for e in errors), f"Expected 'actions' error, got: {errors}"


def test_trigger_missing_event_raises_error():
    policy = {
        "name": "p",
        "rules": [{"name": "r", "trigger": {}, "actions": [{"type": "notify"}]}],
    }
    errors = validate_policy(policy, "policies/test.yaml")
    assert any("event" in e for e in errors), f"Expected 'event' error, got: {errors}"


def test_invalid_action_type_raises_error():
    policy = {
        "name": "p",
        "rules": [{"name": "r", "trigger": {"event": "x"}, "actions": [{"type": "bogus"}]}],
    }
    errors = validate_policy(policy, "policies/test.yaml")
    assert len(errors) > 0, "Expected error for invalid action type"
    combined = " ".join(errors)
    assert "bogus" in combined or "shell" in combined, \
        f"Expected error to mention invalid type or valid types, got: {errors}"


def test_shell_action_missing_command_raises_error():
    policy = {
        "name": "p",
        "rules": [{"name": "r", "trigger": {"event": "x"}, "actions": [{"type": "shell"}]}],
    }
    errors = validate_policy(policy, "policies/test.yaml")
    assert any("command" in e for e in errors), f"Expected 'command' error, got: {errors}"


def test_emit_action_missing_event_raises_error():
    policy = {
        "name": "p",
        "rules": [{"name": "r", "trigger": {"event": "x"}, "actions": [{"type": "emit"}]}],
    }
    errors = validate_policy(policy, "policies/test.yaml")
    assert any("event" in e for e in errors), f"Expected 'event' error, got: {errors}"


def test_invalid_condition_op_raises_error():
    policy = {
        "name": "p",
        "rules": [{
            "name": "r",
            "trigger": {"event": "x"},
            "actions": [{"type": "notify"}],
            "condition": {"field": "status", "op": "foobar", "value": "done"},
        }],
    }
    errors = validate_policy(policy, "policies/test.yaml")
    assert len(errors) > 0, "Expected error for invalid condition op"
    combined = " ".join(errors)
    assert "foobar" in combined or "eq" in combined, \
        f"Expected error to mention invalid op or valid ops, got: {errors}"


def test_condition_missing_field_raises_error():
    policy = {
        "name": "p",
        "rules": [{
            "name": "r",
            "trigger": {"event": "x"},
            "actions": [{"type": "notify"}],
            "condition": {"op": "eq", "value": "done"},
        }],
    }
    errors = validate_policy(policy, "policies/test.yaml")
    assert any("field" in e for e in errors), f"Expected 'field' error, got: {errors}"


def test_validation_returns_all_errors_not_just_first():
    """Policy with multiple errors should return all of them."""
    policy = {
        "name": "p",
        "rules": [
            # rule 1: missing trigger
            {"name": "r1", "actions": [{"type": "notify"}]},
            # rule 2: invalid action type
            {"name": "r2", "trigger": {"event": "x"}, "actions": [{"type": "bogus"}]},
            # rule 3: shell action missing command
            {"name": "r3", "trigger": {"event": "x"}, "actions": [{"type": "shell"}]},
        ],
    }
    errors = validate_policy(policy, "policies/test.yaml")
    assert len(errors) >= 3, f"Expected at least 3 errors, got {len(errors)}: {errors}"


def test_daemon_skips_invalid_policy_and_loads_rest():
    """Simulate loading multiple policies: one bad, rest good."""
    good_policy = {
        "name": "good",
        "rules": [{"name": "r", "trigger": {"event": "x"}, "actions": [{"type": "notify"}]}],
    }
    bad_policy = {"name": "bad"}  # missing rules

    good_errors = validate_policy(good_policy, "policies/good.yaml")
    bad_errors = validate_policy(bad_policy, "policies/bad.yaml")

    assert good_errors == [], f"Good policy should have no errors: {good_errors}"
    assert len(bad_errors) > 0, "Bad policy should have errors"


def test_error_message_includes_policy_filename():
    policy = {"name": "p"}  # missing rules
    errors = validate_policy(policy, "policies/my-custom-policy.yaml")
    combined = " ".join(errors)
    assert "my-custom-policy.yaml" in combined, \
        f"Expected filename in error message, got: {errors}"


def test_error_message_includes_rule_name():
    policy = {
        "name": "p",
        "rules": [{"name": "my-special-rule", "trigger": {"event": "x"}, "actions": [{"type": "shell"}]}],
    }
    errors = validate_policy(policy, "policies/test.yaml")
    combined = " ".join(errors)
    assert "my-special-rule" in combined, \
        f"Expected rule name in error message, got: {errors}"
