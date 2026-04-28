"""End-to-end smoke test for the full telemetry pipeline (t-4).

Tests the complete flow:
1. Emit an event that triggers a policy
2. Verify policy_eval_log has the evaluation trace
3. Verify action_log has the action result
4. Verify condition_details are populated
5. Trigger a rate limit and verify it's logged
6. Run `hex-events trace <event-id>` and verify output
7. Run `hex-events telemetry` and verify output
"""
import argparse
import json
import os
import sys
import tempfile
import time
from io import StringIO
from unittest.mock import patch

import pytest

# conftest.py adds parent dir to sys.path
from db import EventsDB
from policy import Action, Condition, Policy, Rule, record_fire
from hex_eventd import _process_event_policies
from hex_events_cli import cmd_trace, cmd_telemetry


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def make_db(tmp_path):
    db_path = str(tmp_path / "telemetry_test.db")
    return EventsDB(db_path), db_path


def make_policy_with_conditions(name, trigger_event, conditions=None, actions=None,
                                 rate_limit=None):
    """Build a Policy with a single rule."""
    rule = Rule(
        name=f"{name}.rule",
        trigger_event=trigger_event,
        conditions=conditions or [],
        actions=actions or [Action(type="emit", params={"event": f"{name}.fired"})],
    )
    return Policy(name=name, rules=[rule], rate_limit=rate_limit)


def insert_and_fetch(db, event_type, payload=None):
    """Insert an event and return its row dict."""
    eid = db.insert_event(event_type, json.dumps(payload or {}), "test")
    return dict(db.conn.execute("SELECT * FROM events WHERE id = ?", (eid,)).fetchone())


def run_cmd_trace(db_path, event_id, policy=None):
    """Run cmd_trace with a temporary DB path override, return captured output."""
    import hex_events_cli
    orig = hex_events_cli.DB_PATH
    hex_events_cli.DB_PATH = db_path
    try:
        args = argparse.Namespace(event_id=event_id, policy=policy, since=None)
        buf = StringIO()
        with patch("sys.stdout", buf):
            cmd_trace(args)
        return buf.getvalue()
    finally:
        hex_events_cli.DB_PATH = orig


def run_cmd_telemetry(db_path, as_json=False):
    """Run cmd_telemetry with a temporary DB path override, return captured output."""
    import hex_events_cli
    orig = hex_events_cli.DB_PATH
    hex_events_cli.DB_PATH = db_path
    try:
        args = argparse.Namespace(json=as_json)
        buf = StringIO()
        with patch("sys.stdout", buf):
            cmd_telemetry(args)
        return buf.getvalue()
    finally:
        hex_events_cli.DB_PATH = orig


# ---------------------------------------------------------------------------
# 1. Full pipeline smoke test
# ---------------------------------------------------------------------------

class TestTelemetryPipeline:
    """Exercises the full telemetry pipeline end-to-end."""

    def test_policy_eval_log_populated(self, tmp_path):
        """Policy evaluation is recorded in policy_eval_log."""
        db, _ = make_db(tmp_path)
        policy = make_policy_with_conditions(
            "smoke-policy", "smoke.event",
            conditions=[Condition(field="status", op="eq", value="ok")],
        )
        event = insert_and_fetch(db, "smoke.event", {"status": "ok"})
        _process_event_policies(event, [policy], db)

        rows = db.get_policy_evals(event["id"])
        db.close()

        assert len(rows) == 1
        row = rows[0]
        assert row["policy_name"] == "smoke-policy"
        assert row["matched"] == 1
        assert row["conditions_passed"] == 1
        assert row["action_taken"] == 1
        assert row["rate_limited"] == 0

    def test_action_log_populated(self, tmp_path):
        """Action execution is recorded in action_log."""
        db, _ = make_db(tmp_path)
        policy = make_policy_with_conditions(
            "action-log-policy", "action.event",
        )
        event = insert_and_fetch(db, "action.event", {})
        _process_event_policies(event, [policy], db)

        logs = db.get_action_logs(event["id"])
        db.close()

        assert len(logs) >= 1
        assert logs[0]["recipe"] == "action-log-policy.rule"

    def test_condition_details_populated(self, tmp_path):
        """condition_details JSON is populated with per-condition pass/fail info."""
        db, _ = make_db(tmp_path)
        policy = make_policy_with_conditions(
            "cond-detail-policy", "cond.event",
            conditions=[Condition(field="branch", op="eq", value="main")],
        )
        # Use payload where condition FAILS so we can inspect details
        event = insert_and_fetch(db, "cond.event", {"branch": "dev"})
        _process_event_policies(event, [policy], db)

        rows = db.get_policy_evals(event["id"])
        db.close()

        assert len(rows) == 1
        row = rows[0]
        assert row["conditions_passed"] == 0
        assert row["condition_details"] is not None

        details = json.loads(row["condition_details"])
        assert len(details) == 1
        d = details[0]
        assert d["field"] == "branch"
        assert d["op"] == "eq"
        assert d["expected"] == "main"
        assert d["actual"] == "dev"
        assert d["passed"] is False

    def test_condition_details_on_pass(self, tmp_path):
        """condition_details is populated even when conditions pass."""
        db, _ = make_db(tmp_path)
        policy = make_policy_with_conditions(
            "cond-pass-policy", "pass.event",
            conditions=[Condition(field="status", op="eq", value="done")],
        )
        event = insert_and_fetch(db, "pass.event", {"status": "done"})
        _process_event_policies(event, [policy], db)

        rows = db.get_policy_evals(event["id"])
        db.close()

        assert rows[0]["conditions_passed"] == 1
        details = json.loads(rows[0]["condition_details"])
        assert details[0]["passed"] is True
        assert details[0]["actual"] == "done"

    def test_rate_limit_logged(self, tmp_path):
        """Rate-limited policy fires create rate_limited action_log entry and eval entry."""
        db, _ = make_db(tmp_path)
        policy = make_policy_with_conditions(
            "rl-policy", "rl.event",
            rate_limit={"max_fires": 1, "window": "1h"},
        )

        # First event — fires successfully
        e1 = insert_and_fetch(db, "rl.event", {})
        _process_event_policies(e1, [policy], db)

        # Second event — should be rate-limited
        e2 = insert_and_fetch(db, "rl.event", {})
        _process_event_policies(e2, [policy], db)

        # action_log should have a rate_limited entry for e2
        logs = db.get_action_logs(e2["id"])
        # eval log should flag rate_limited
        evals = db.get_policy_evals(e2["id"])
        db.close()

        assert any(l["action_type"] == "rate_limited" for l in logs), \
            f"Expected rate_limited action_log entry, got: {logs}"
        assert any(r["rate_limited"] == 1 for r in evals), \
            f"Expected rate_limited=1 in eval log, got: {evals}"

    def test_trace_cli_shows_policy_eval(self, tmp_path):
        """hex-events trace <event-id> shows policy evaluation info."""
        db, db_path = make_db(tmp_path)
        policy = make_policy_with_conditions(
            "trace-test-policy", "trace.event",
            conditions=[Condition(field="env", op="eq", value="prod")],
        )
        event = insert_and_fetch(db, "trace.event", {"env": "prod"})
        _process_event_policies(event, [policy], db)
        db.close()

        output = run_cmd_trace(db_path, event["id"])

        assert f"Event #{event['id']}" in output
        assert "Policy evaluations:" in output
        assert "trace-test-policy" in output
        assert "✓" in output  # conditions passed marker

    def test_trace_cli_shows_rate_limited_marker(self, tmp_path):
        """hex-events trace <event-id> shows ⊘ for rate-limited events."""
        db, db_path = make_db(tmp_path)
        policy = make_policy_with_conditions(
            "rl-trace-policy", "rl.trace.event",
            rate_limit={"max_fires": 1, "window": "1h"},
        )

        e1 = insert_and_fetch(db, "rl.trace.event", {})
        _process_event_policies(e1, [policy], db)

        e2 = insert_and_fetch(db, "rl.trace.event", {})
        _process_event_policies(e2, [policy], db)
        db.close()

        output = run_cmd_trace(db_path, e2["id"])

        assert "⊘" in output
        assert "Rate limited: yes" in output
        assert "rl-trace-policy" in output

    def test_telemetry_cli_text_output(self, tmp_path):
        """hex-events telemetry shows the expected dashboard format."""
        db, db_path = make_db(tmp_path)

        # Emit and process a few events to populate stats
        policy = make_policy_with_conditions("telem-policy", "telem.event")
        for _ in range(3):
            event = insert_and_fetch(db, "telem.event", {})
            _process_event_policies(event, [policy], db)
        db.close()

        output = run_cmd_telemetry(db_path, as_json=False)

        assert "hex-events Telemetry (last 24h)" in output
        assert "Events processed:" in output
        assert "Actions fired:" in output
        assert "Actions failed:" in output
        assert "Rate limits hit:" in output
        assert "Policy violations:" in output
        assert "Daemon:" in output

    def test_telemetry_cli_json_output(self, tmp_path):
        """hex-events telemetry --json returns parseable JSON with expected keys."""
        db, db_path = make_db(tmp_path)
        policy = make_policy_with_conditions("json-telem-policy", "json.telem.event")
        event = insert_and_fetch(db, "json.telem.event", {})
        _process_event_policies(event, [policy], db)
        db.close()

        output = run_cmd_telemetry(db_path, as_json=True)

        data = json.loads(output)
        assert "events_processed" in data
        assert "actions_fired" in data
        assert "actions_failed" in data
        assert "rate_limits_hit" in data
        assert "policy_violations" in data
        assert data["events_processed"] >= 1

    def test_telemetry_counts_rate_limits(self, tmp_path):
        """hex-events telemetry --json counts rate limit hits correctly."""
        db, db_path = make_db(tmp_path)
        policy = make_policy_with_conditions(
            "rl-telem-policy", "rl.telem.event",
            rate_limit={"max_fires": 1, "window": "1h"},
        )

        e1 = insert_and_fetch(db, "rl.telem.event", {})
        _process_event_policies(e1, [policy], db)

        e2 = insert_and_fetch(db, "rl.telem.event", {})
        _process_event_policies(e2, [policy], db)
        db.close()

        output = run_cmd_telemetry(db_path, as_json=True)
        data = json.loads(output)

        assert data["rate_limits_hit"] >= 1

    def test_full_pipeline_all_tables_populated(self, tmp_path):
        """Smoke: single event populates events, action_log, and policy_eval_log."""
        db, db_path = make_db(tmp_path)
        policy = make_policy_with_conditions(
            "full-pipeline-policy", "full.pipeline.event",
            conditions=[Condition(field="ready", op="eq", value="true")],
        )

        event = insert_and_fetch(db, "full.pipeline.event", {"ready": "true"})
        _process_event_policies(event, [policy], db)

        # All three tables should have entries
        evals = db.get_policy_evals(event["id"])
        action_logs = db.get_action_logs(event["id"])

        events_row = db.conn.execute(
            "SELECT * FROM events WHERE id = ?", (event["id"],)
        ).fetchone()
        db.close()

        # events: marked processed
        assert events_row["processed_at"] is not None

        # policy_eval_log: has entry
        assert len(evals) == 1
        assert evals[0]["matched"] == 1
        assert evals[0]["conditions_passed"] == 1

        # action_log: has entry
        assert len(action_logs) >= 1

        # condition_details: populated
        assert evals[0]["condition_details"] is not None
        details = json.loads(evals[0]["condition_details"])
        assert details[0]["actual"] == "true"
        assert details[0]["passed"] is True
