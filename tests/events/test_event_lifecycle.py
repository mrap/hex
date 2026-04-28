"""test_event_lifecycle.py — Unit-level integration test for the event→policy→condition pipeline.

No daemon subprocess. Tests the core components in isolation:
  1. EventsDB creation and event insertion
  2. Policy loading from a YAML string
  3. Condition evaluation against an event payload
  4. Policy match verification
  5. Action execution via _process_event_policies

This is a fast, hermetic test — no I/O side-effects besides the temp SQLite DB.
"""
import json
import os
import sys
import shutil
import tempfile

import pytest
import yaml

# Ensure repo root is on sys.path (conftest.py does this too, but be explicit)
_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, _REPO_ROOT)

from db import EventsDB
from policy import load_policies, Policy, Rule, Condition, Action
from conditions import evaluate_conditions, evaluate_conditions_with_details
from hex_eventd import _process_event_policies


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def tmp_db(tmp_path):
    """EventsDB backed by a temp SQLite file. Cleaned up after each test."""
    db_path = str(tmp_path / "test_events.db")
    db = EventsDB(db_path)
    yield db
    db.close()


@pytest.fixture
def tmp_policies_dir(tmp_path):
    """Empty temp directory for policy YAML files."""
    policies_dir = tmp_path / "policies"
    policies_dir.mkdir()
    return policies_dir


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def write_policy(policies_dir, name: str, content: str):
    """Write a YAML string to a policy file in the given directory."""
    path = os.path.join(str(policies_dir), name)
    with open(path, "w") as f:
        f.write(content)
    return path


SIMPLE_SHELL_POLICY = """
name: test-lifecycle-shell
description: Shell action policy for lifecycle testing
rules:
  - name: echo-on-check
    trigger:
      event: test.lifecycle.check
    conditions:
      - field: status
        op: eq
        value: "ok"
    actions:
      - type: shell
        command: "echo 'lifecycle-test-ok'"
"""

EMIT_CHAIN_POLICY = """
name: test-lifecycle-emit
description: Emit-chain policy for lifecycle testing
rules:
  - name: emit-on-input
    trigger:
      event: test.lifecycle.input
    actions:
      - type: emit
        event: test.lifecycle.output
        payload:
          origin: "test"
"""

CONDITION_BLOCK_POLICY = """
name: test-lifecycle-block
description: Policy where condition should block action
rules:
  - name: block-wrong-status
    trigger:
      event: test.lifecycle.blocked
    conditions:
      - field: status
        op: eq
        value: "go"
    actions:
      - type: shell
        command: "echo 'should-not-run'"
"""

GLOB_TRIGGER_POLICY = """
name: test-lifecycle-glob
description: Policy using glob trigger pattern
rules:
  - name: catch-all-lifecycle
    trigger:
      event: test.lifecycle.*
    conditions:
      - field: tagged
        op: eq
        value: "yes"
    actions:
      - type: shell
        command: "echo 'glob-matched'"
"""


# ---------------------------------------------------------------------------
# 1. EventsDB: insert and retrieve events
# ---------------------------------------------------------------------------

class TestEventInsertAndRetrieve:
    def test_insert_event_returns_id(self, tmp_db):
        eid = tmp_db.insert_event("test.lifecycle.check", '{"status":"ok"}', "test")
        assert isinstance(eid, int)
        assert eid > 0

    def test_get_unprocessed_returns_inserted(self, tmp_db):
        tmp_db.insert_event("test.lifecycle.check", '{"status":"ok"}', "test")
        events = tmp_db.get_unprocessed()
        assert len(events) == 1
        assert events[0]["event_type"] == "test.lifecycle.check"

    def test_mark_processed_removes_from_unprocessed(self, tmp_db):
        eid = tmp_db.insert_event("test.lifecycle.check", '{"status":"ok"}', "test")
        tmp_db.mark_processed(eid)
        assert len(tmp_db.get_unprocessed()) == 0

    def test_payload_is_valid_json(self, tmp_db):
        payload = {"status": "ok", "count": 42}
        tmp_db.insert_event("test.lifecycle.check", json.dumps(payload), "test")
        events = tmp_db.get_unprocessed()
        parsed = json.loads(events[0]["payload"])
        assert parsed["status"] == "ok"
        assert parsed["count"] == 42

    def test_dedup_key_prevents_double_processing(self, tmp_db):
        """Dedup key should prevent re-insertion of an already-processed event."""
        eid1 = tmp_db.insert_event("test.dedup", '{"x":1}', "test", dedup_key="key-1")
        tmp_db.mark_processed(eid1)
        eid2 = tmp_db.insert_event("test.dedup", '{"x":2}', "test", dedup_key="key-1")
        assert eid2 is None  # deduplicated


# ---------------------------------------------------------------------------
# 2. Policy loading from YAML
# ---------------------------------------------------------------------------

class TestPolicyLoading:
    def test_load_simple_policy(self, tmp_policies_dir):
        write_policy(tmp_policies_dir, "check.yaml", SIMPLE_SHELL_POLICY)
        policies = load_policies(str(tmp_policies_dir))
        assert len(policies) == 1
        p = policies[0]
        assert p.name == "test-lifecycle-shell"
        assert len(p.rules) == 1

    def test_rule_trigger_event(self, tmp_policies_dir):
        write_policy(tmp_policies_dir, "check.yaml", SIMPLE_SHELL_POLICY)
        policies = load_policies(str(tmp_policies_dir))
        rule = policies[0].rules[0]
        assert rule.trigger_event == "test.lifecycle.check"

    def test_rule_conditions_parsed(self, tmp_policies_dir):
        write_policy(tmp_policies_dir, "check.yaml", SIMPLE_SHELL_POLICY)
        policies = load_policies(str(tmp_policies_dir))
        rule = policies[0].rules[0]
        assert len(rule.conditions) == 1
        cond = rule.conditions[0]
        assert cond.field == "status"
        assert cond.op == "eq"
        assert cond.value == "ok"

    def test_rule_actions_parsed(self, tmp_policies_dir):
        write_policy(tmp_policies_dir, "check.yaml", SIMPLE_SHELL_POLICY)
        policies = load_policies(str(tmp_policies_dir))
        rule = policies[0].rules[0]
        assert len(rule.actions) == 1
        action = rule.actions[0]
        assert action.type == "shell"
        assert "echo" in action.params["command"]

    def test_load_multiple_policies(self, tmp_policies_dir):
        write_policy(tmp_policies_dir, "a.yaml", SIMPLE_SHELL_POLICY)
        write_policy(tmp_policies_dir, "b.yaml", EMIT_CHAIN_POLICY)
        policies = load_policies(str(tmp_policies_dir))
        names = {p.name for p in policies}
        assert "test-lifecycle-shell" in names
        assert "test-lifecycle-emit" in names

    def test_glob_trigger_policy_loaded(self, tmp_policies_dir):
        write_policy(tmp_policies_dir, "glob.yaml", GLOB_TRIGGER_POLICY)
        policies = load_policies(str(tmp_policies_dir))
        rule = policies[0].rules[0]
        assert rule.trigger_event == "test.lifecycle.*"


# ---------------------------------------------------------------------------
# 3. Condition evaluation
# ---------------------------------------------------------------------------

class TestConditionEvaluation:
    def test_eq_condition_passes(self):
        cond = Condition(field="status", op="eq", value="ok")
        assert evaluate_conditions([cond], {"status": "ok"}, db=None) is True

    def test_eq_condition_fails(self):
        cond = Condition(field="status", op="eq", value="ok")
        assert evaluate_conditions([cond], {"status": "fail"}, db=None) is False

    def test_missing_field_fails(self):
        cond = Condition(field="missing_field", op="eq", value="anything")
        assert evaluate_conditions([cond], {"status": "ok"}, db=None) is False

    def test_empty_conditions_passes(self):
        assert evaluate_conditions([], {"status": "ok"}, db=None) is True

    def test_multiple_conditions_and_logic(self):
        conds = [
            Condition(field="status", op="eq", value="ok"),
            Condition(field="count", op="gt", value=0),
        ]
        assert evaluate_conditions(conds, {"status": "ok", "count": 5}, db=None) is True
        assert evaluate_conditions(conds, {"status": "ok", "count": 0}, db=None) is False

    def test_short_circuit_on_first_failure(self):
        """Second condition should not be evaluated if first fails."""
        conds = [
            Condition(field="status", op="eq", value="ok"),
            Condition(field="missing", op="eq", value="anything"),
        ]
        passed, details = evaluate_conditions_with_details(conds, {"status": "wrong"}, db=None)
        assert passed is False
        assert details[0]["passed"] is False
        assert details[1]["passed"] == "not_evaluated"

    def test_contains_operator(self):
        cond = Condition(field="message", op="contains", value="error")
        assert evaluate_conditions([cond], {"message": "fatal error occurred"}, db=None) is True
        assert evaluate_conditions([cond], {"message": "all good"}, db=None) is False

    def test_glob_operator(self):
        cond = Condition(field="event_name", op="glob", value="boi.spec.*")
        assert evaluate_conditions([cond], {"event_name": "boi.spec.completed"}, db=None) is True
        assert evaluate_conditions([cond], {"event_name": "session.stop"}, db=None) is False

    def test_regex_operator(self):
        cond = Condition(field="id", op="regex", value=r"^q-\d+$")
        assert evaluate_conditions([cond], {"id": "q-123"}, db=None) is True
        assert evaluate_conditions([cond], {"id": "abc"}, db=None) is False

    def test_neq_operator(self):
        cond = Condition(field="status", op="neq", value="failed")
        assert evaluate_conditions([cond], {"status": "ok"}, db=None) is True
        assert evaluate_conditions([cond], {"status": "failed"}, db=None) is False


# ---------------------------------------------------------------------------
# 4. Policy match: does the policy trigger event match the event type?
# ---------------------------------------------------------------------------

class TestPolicyTriggerMatching:
    def _make_rule(self, trigger_event: str) -> Rule:
        return Rule(name="test", trigger_event=trigger_event, conditions=[], actions=[])

    def test_exact_match(self):
        rule = self._make_rule("test.lifecycle.check")
        assert rule.matches_event_type("test.lifecycle.check") is True
        assert rule.matches_event_type("test.lifecycle.other") is False

    def test_glob_wildcard_match(self):
        rule = self._make_rule("test.lifecycle.*")
        assert rule.matches_event_type("test.lifecycle.check") is True
        assert rule.matches_event_type("test.lifecycle.fire") is True
        assert rule.matches_event_type("other.event") is False

    def test_double_wildcard(self):
        rule = self._make_rule("boi.*.*")
        assert rule.matches_event_type("boi.spec.completed") is True
        assert rule.matches_event_type("boi.output.committed") is True
        assert rule.matches_event_type("session.stop") is False


# ---------------------------------------------------------------------------
# 5. Full pipeline: event → policy → condition → action via _process_event_policies
# ---------------------------------------------------------------------------

class TestFullPipeline:
    def test_event_processed_and_action_logged(self, tmp_db, tmp_policies_dir):
        """Full pipeline: insert event, load policy, process, verify action logged."""
        write_policy(tmp_policies_dir, "check.yaml", SIMPLE_SHELL_POLICY)
        policies = load_policies(str(tmp_policies_dir))

        eid = tmp_db.insert_event(
            "test.lifecycle.check",
            json.dumps({"status": "ok"}),
            "test",
        )
        event = tmp_db.get_unprocessed()[0]
        actions_fired = _process_event_policies(event, policies, tmp_db)

        assert actions_fired == 1
        assert len(tmp_db.get_unprocessed()) == 0
        logs = tmp_db.get_action_logs(eid)
        assert len(logs) >= 1
        assert logs[0]["status"] == "success"

    def test_condition_blocks_action(self, tmp_db, tmp_policies_dir):
        """Condition mismatch should prevent action from firing."""
        write_policy(tmp_policies_dir, "block.yaml", CONDITION_BLOCK_POLICY)
        policies = load_policies(str(tmp_policies_dir))

        eid = tmp_db.insert_event(
            "test.lifecycle.blocked",
            json.dumps({"status": "stop"}),  # "go" would trigger
            "test",
        )
        event = tmp_db.get_unprocessed()[0]
        actions_fired = _process_event_policies(event, policies, tmp_db)

        assert actions_fired == 0
        assert len(tmp_db.get_unprocessed()) == 0
        # No action logged when conditions block
        logs = tmp_db.get_action_logs(eid)
        assert len(logs) == 0

    def test_emit_action_chains_event(self, tmp_db, tmp_policies_dir):
        """Emit action should insert a chained event into the DB."""
        write_policy(tmp_policies_dir, "emit.yaml", EMIT_CHAIN_POLICY)
        policies = load_policies(str(tmp_policies_dir))

        tmp_db.insert_event("test.lifecycle.input", json.dumps({"x": 1}), "test")
        event = tmp_db.get_unprocessed()[0]
        _process_event_policies(event, policies, tmp_db)

        # Original event processed; chained output event should exist
        all_types = [
            r["event_type"]
            for r in tmp_db.conn.execute("SELECT event_type FROM events").fetchall()
        ]
        assert "test.lifecycle.output" in all_types

    def test_unmatched_event_processed_no_action(self, tmp_db, tmp_policies_dir):
        """Event that doesn't match any policy: processed, no action logged."""
        write_policy(tmp_policies_dir, "check.yaml", SIMPLE_SHELL_POLICY)
        policies = load_policies(str(tmp_policies_dir))

        eid = tmp_db.insert_event("totally.different.event", json.dumps({}), "test")
        event = tmp_db.get_unprocessed()[0]
        actions_fired = _process_event_policies(event, policies, tmp_db)

        assert actions_fired == 0
        assert len(tmp_db.get_unprocessed()) == 0
        assert len(tmp_db.get_action_logs(eid)) == 0

    def test_glob_trigger_matches_any_lifecycle_event(self, tmp_db, tmp_policies_dir):
        """Glob trigger test.lifecycle.* should fire for any matching event type."""
        write_policy(tmp_policies_dir, "glob.yaml", GLOB_TRIGGER_POLICY)
        policies = load_policies(str(tmp_policies_dir))

        eid = tmp_db.insert_event(
            "test.lifecycle.arbitrary",
            json.dumps({"tagged": "yes"}),
            "test",
        )
        event = tmp_db.get_unprocessed()[0]
        actions_fired = _process_event_policies(event, policies, tmp_db)

        assert actions_fired == 1
        logs = tmp_db.get_action_logs(eid)
        assert any(l["status"] == "success" for l in logs)

    def test_policy_eval_log_written(self, tmp_db, tmp_policies_dir):
        """Policy eval log should record the evaluation result."""
        write_policy(tmp_policies_dir, "check.yaml", SIMPLE_SHELL_POLICY)
        policies = load_policies(str(tmp_policies_dir))

        eid = tmp_db.insert_event(
            "test.lifecycle.check",
            json.dumps({"status": "ok"}),
            "test",
        )
        event = tmp_db.get_unprocessed()[0]
        _process_event_policies(event, policies, tmp_db)

        evals = tmp_db.get_policy_evals(eid)
        assert len(evals) >= 1
        assert evals[0]["policy_name"] == "test-lifecycle-shell"
        assert evals[0]["matched"] == 1
        assert evals[0]["action_taken"] == 1

    def test_invalid_json_payload_handled_gracefully(self, tmp_db, tmp_policies_dir):
        """Malformed JSON payload should not crash the daemon; event marked processed."""
        write_policy(tmp_policies_dir, "check.yaml", SIMPLE_SHELL_POLICY)
        policies = load_policies(str(tmp_policies_dir))

        eid = tmp_db.insert_event("test.lifecycle.check", "not-valid-json", "test")
        event = tmp_db.get_unprocessed()[0]
        # Should not raise; daemon handles this defensively
        _process_event_policies(event, policies, tmp_db)

        assert len(tmp_db.get_unprocessed()) == 0
        assert len(tmp_db.get_action_logs(eid)) == 0
