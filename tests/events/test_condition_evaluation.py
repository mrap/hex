"""Tests for condition evaluation in hex-events policies (t-1 RED phase).

These tests MUST FAIL with current code because:
1. _parse_rule reads 'conditions' (plural) but policy YAML uses 'condition' (singular) —
   so condition blocks are silently ignored and actions fire unconditionally.
2. conditions.py does not support dot-notation field resolution (e.g. payload.spec_id).
3. glob and regex ops are not implemented in conditions.py.

After the fix (t-2), all tests should pass GREEN.
"""
import json
import os
import stat
import sys
import tempfile

import pytest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from db import EventsDB
from policy import load_policies, Rule, Condition
from hex_eventd import _process_event_policies
from conditions import evaluate_conditions
from tests.test_policy_integration import DaemonCtx


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def make_new_policy(name: str, trigger: str, condition: dict, action_event: str) -> str:
    """Generate a new-format policy YAML with a single 'condition:' rule.

    The condition key is singular (as used in the real chain policy),
    not 'conditions' (plural list). This is the format that currently fails.
    """
    value = condition["value"]
    if isinstance(value, str):
        value_yaml = f'"{value}"'
    elif isinstance(value, int):
        value_yaml = str(value)
    else:
        value_yaml = f'"{value}"'

    return f"""name: {name}
description: Test condition policy
rules:
  - name: {name}-rule
    trigger:
      event: {trigger}
    condition:
      field: {condition["field"]}
      op: {condition["op"]}
      value: {value_yaml}
    actions:
      - type: emit
        event: {action_event}
"""


def make_new_policy_no_condition(name: str, trigger: str, action_event: str) -> str:
    """New-format policy without any condition — baseline, should always fire."""
    return f"""name: {name}
description: Test no-condition policy
rules:
  - name: {name}-rule
    trigger:
      event: {trigger}
    actions:
      - type: emit
        event: {action_event}
"""


def action_fired(ctx: DaemonCtx, event_id: int) -> bool:
    """Return True if any action was logged for this event."""
    return bool(ctx.get_action_logs(event_id))


# ---------------------------------------------------------------------------
# 1. eq — matching payload should fire
# ---------------------------------------------------------------------------

def test_condition_eq_matches():
    """condition {field: payload.spec_id, op: eq, value: q-180}: emit q-180 → fire."""
    with DaemonCtx() as ctx:
        ctx.add_policy("eq-match.yaml", make_new_policy(
            "eq-match", "test.cond.eq", {"field": "payload.spec_id", "op": "eq", "value": "q-180"},
            "test.action.fired",
        ))
        event_id = ctx.emit("test.cond.eq", {"spec_id": "q-180"})
        ctx.tick()
        assert action_fired(ctx, event_id), "Expected action to fire when condition matches (eq)"


# ---------------------------------------------------------------------------
# 2. eq — non-matching payload must NOT fire (critical: currently fails)
# ---------------------------------------------------------------------------

def test_condition_eq_blocks_non_matching():
    """condition eq q-180: emit q-999 → must NOT fire."""
    with DaemonCtx() as ctx:
        ctx.add_policy("eq-block.yaml", make_new_policy(
            "eq-block", "test.cond.eq.block", {"field": "payload.spec_id", "op": "eq", "value": "q-180"},
            "test.action.fired",
        ))
        event_id = ctx.emit("test.cond.eq.block", {"spec_id": "q-999"})
        ctx.tick()
        assert not action_fired(ctx, event_id), (
            "Expected action NOT to fire when condition does not match (eq)"
        )


# ---------------------------------------------------------------------------
# 3. neq — matching value must NOT fire
# ---------------------------------------------------------------------------

def test_condition_neq_blocks_matching():
    """condition neq q-180: emit q-180 → must NOT fire."""
    with DaemonCtx() as ctx:
        ctx.add_policy("neq-block.yaml", make_new_policy(
            "neq-block", "test.cond.neq.block", {"field": "payload.spec_id", "op": "neq", "value": "q-180"},
            "test.action.fired",
        ))
        event_id = ctx.emit("test.cond.neq.block", {"spec_id": "q-180"})
        ctx.tick()
        assert not action_fired(ctx, event_id), (
            "Expected action NOT to fire when neq condition matches the exclusion value"
        )


# ---------------------------------------------------------------------------
# 4. neq — non-matching value should fire
# ---------------------------------------------------------------------------

def test_condition_neq_allows_non_matching():
    """condition neq q-180: emit q-999 → should fire."""
    with DaemonCtx() as ctx:
        ctx.add_policy("neq-allow.yaml", make_new_policy(
            "neq-allow", "test.cond.neq.allow", {"field": "payload.spec_id", "op": "neq", "value": "q-180"},
            "test.action.fired",
        ))
        event_id = ctx.emit("test.cond.neq.allow", {"spec_id": "q-999"})
        ctx.tick()
        assert action_fired(ctx, event_id), (
            "Expected action to fire when neq condition value does not match"
        )


# ---------------------------------------------------------------------------
# 5. contains — substring present → fire
# ---------------------------------------------------------------------------

def test_condition_contains_matches():
    """condition {field: payload.tags, op: contains, value: urgent}: emit {tags: urgent,high} → fire."""
    with DaemonCtx() as ctx:
        ctx.add_policy("contains-match.yaml", make_new_policy(
            "contains-match", "test.cond.contains", {"field": "payload.tags", "op": "contains", "value": "urgent"},
            "test.action.fired",
        ))
        event_id = ctx.emit("test.cond.contains", {"tags": "urgent,high"})
        ctx.tick()
        assert action_fired(ctx, event_id), "Expected action to fire when contains condition matches"


# ---------------------------------------------------------------------------
# 6. contains — substring absent → must NOT fire
# ---------------------------------------------------------------------------

def test_condition_contains_blocks():
    """condition contains urgent: emit {tags: low} → must NOT fire."""
    with DaemonCtx() as ctx:
        ctx.add_policy("contains-block.yaml", make_new_policy(
            "contains-block", "test.cond.contains.block", {"field": "payload.tags", "op": "contains", "value": "urgent"},
            "test.action.fired",
        ))
        event_id = ctx.emit("test.cond.contains.block", {"tags": "low"})
        ctx.tick()
        assert not action_fired(ctx, event_id), (
            "Expected action NOT to fire when contains condition does not match"
        )


# ---------------------------------------------------------------------------
# 7. nested field — dot-notation traversal (payload.tasks.completed)
# ---------------------------------------------------------------------------

def test_condition_nested_field():
    """condition {field: payload.tasks.completed, op: gt, value: 0}: nested resolution."""
    # Matching: tasks.completed = 5 → should fire
    with DaemonCtx() as ctx:
        ctx.add_policy("nested-match.yaml", make_new_policy(
            "nested-match", "test.cond.nested", {"field": "payload.tasks.completed", "op": "gt", "value": 0},
            "test.action.fired",
        ))
        event_id = ctx.emit("test.cond.nested", {"tasks": {"completed": 5}})
        ctx.tick()
        assert action_fired(ctx, event_id), (
            "Expected action to fire when nested field tasks.completed=5 > 0"
        )

    # Non-matching: tasks.completed = 0 → must NOT fire
    with DaemonCtx() as ctx:
        ctx.add_policy("nested-block.yaml", make_new_policy(
            "nested-block", "test.cond.nested.block", {"field": "payload.tasks.completed", "op": "gt", "value": 0},
            "test.action.fired",
        ))
        event_id = ctx.emit("test.cond.nested.block", {"tasks": {"completed": 0}})
        ctx.tick()
        assert not action_fired(ctx, event_id), (
            "Expected action NOT to fire when nested field tasks.completed=0 is not > 0"
        )


# ---------------------------------------------------------------------------
# 8. missing field — should NOT fire and must not crash
# ---------------------------------------------------------------------------

def test_condition_missing_field_does_not_crash():
    """condition references payload.nonexistent → should NOT fire, no crash."""
    with DaemonCtx() as ctx:
        ctx.add_policy("missing-field.yaml", make_new_policy(
            "missing-field", "test.cond.missing", {"field": "payload.nonexistent", "op": "eq", "value": "anything"},
            "test.action.fired",
        ))
        # Emit event without the referenced field
        event_id = ctx.emit("test.cond.missing", {"other_field": "value"})
        ctx.tick()
        assert not action_fired(ctx, event_id), (
            "Expected action NOT to fire when condition field is missing from payload"
        )


# ---------------------------------------------------------------------------
# 9. no condition — baseline: should always fire
# ---------------------------------------------------------------------------

def test_no_condition_always_fires():
    """Rule without condition block fires on every matching trigger event."""
    with DaemonCtx() as ctx:
        ctx.add_policy("no-cond.yaml", make_new_policy_no_condition(
            "no-cond", "test.cond.none", "test.action.fired",
        ))
        event_id = ctx.emit("test.cond.none", {"anything": "goes"})
        ctx.tick()
        assert action_fired(ctx, event_id), (
            "Expected action to fire for rule with no condition (baseline)"
        )


# ---------------------------------------------------------------------------
# 10. glob op
# ---------------------------------------------------------------------------

def test_condition_glob_matches():
    """condition {field: payload.spec_id, op: glob, value: q-18*}: glob matching."""
    # Matching: q-180 matches q-18* → should fire
    with DaemonCtx() as ctx:
        ctx.add_policy("glob-match.yaml", make_new_policy(
            "glob-match", "test.cond.glob", {"field": "payload.spec_id", "op": "glob", "value": "q-18*"},
            "test.action.fired",
        ))
        event_id = ctx.emit("test.cond.glob", {"spec_id": "q-180"})
        ctx.tick()
        assert action_fired(ctx, event_id), (
            "Expected action to fire when glob pattern q-18* matches q-180"
        )

    # Non-matching: q-200 does not match q-18* → must NOT fire
    with DaemonCtx() as ctx:
        ctx.add_policy("glob-block.yaml", make_new_policy(
            "glob-block", "test.cond.glob.block", {"field": "payload.spec_id", "op": "glob", "value": "q-18*"},
            "test.action.fired",
        ))
        event_id = ctx.emit("test.cond.glob.block", {"spec_id": "q-200"})
        ctx.tick()
        assert not action_fired(ctx, event_id), (
            "Expected action NOT to fire when glob pattern q-18* does not match q-200"
        )


# ---------------------------------------------------------------------------
# 11. Chain policy integration test (t-4)
# ---------------------------------------------------------------------------

def _make_mock_dispatch_script() -> str:
    """Create a temp executable that simulates 'boi dispatch'. Returns its path."""
    tmp_dir = tempfile.mkdtemp(prefix="hex-mock-dispatch-")
    path = os.path.join(tmp_dir, "mock-boi-dispatch.sh")
    with open(path, "w") as f:
        f.write("#!/bin/sh\necho 'mock-dispatched'\nexit 0\n")
    os.chmod(path, stat.S_IRWXU | stat.S_IRGRP | stat.S_IXGRP)
    return path


def _make_chain_policy(script_path: str) -> str:
    """Reproduce chain-q180-to-experiments.yaml structure with a mock dispatch script.

    Uses the new-format: rules list with singular 'condition:' block and
    'payload.spec_id' dot-notation field, exactly matching the real policy.
    """
    return f"""name: chain-q180-test
description: Test version of chain-q180-to-experiments policy
enabled: true
requires:
  events:
    - boi.spec.completed
rules:
  - name: dispatch-experiments-after-q180
    trigger:
      event: boi.spec.completed
    condition:
      field: payload.spec_id
      op: eq
      value: "q-180"
    actions:
      - type: shell
        command: "{script_path}"
"""


def test_chain_policy_fires_only_for_q180():
    """chain-q180 policy: fires for spec_id=q-180, NOT for spec_id=q-TEST-NOT-180."""
    script = _make_mock_dispatch_script()

    # Should NOT fire for q-TEST-NOT-180
    with DaemonCtx() as ctx:
        ctx.add_policy("chain-q180-test.yaml", _make_chain_policy(script))
        event_id_other = ctx.emit("boi.spec.completed", {"spec_id": "q-TEST-NOT-180"})
        ctx.tick()
        assert not action_fired(ctx, event_id_other), (
            "chain policy must NOT fire for spec_id='q-TEST-NOT-180'"
        )

    # Should fire for q-180
    with DaemonCtx() as ctx:
        ctx.add_policy("chain-q180-test.yaml", _make_chain_policy(script))
        event_id_180 = ctx.emit("boi.spec.completed", {"spec_id": "q-180"})
        ctx.tick()
        assert action_fired(ctx, event_id_180), (
            "chain policy must fire for spec_id='q-180'"
        )
