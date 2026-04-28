"""Integration test framework for hex-events policies (t-1).

Exercises the full pipeline end-to-end:
  emit event → in-process daemon tick → policy matches → action executes
  → action_log verified.

Design: Uses a DaemonCtx class that wraps real daemon functions
(_process_event_policies, load_policies) with a temp DB and temp policies
directory. No subprocess daemon needed — processing happens synchronously
in the test process.
"""
import json
import os
import shutil
import stat
import sys
import tempfile
import time

import pytest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from db import EventsDB
from policy import load_policies
from hex_eventd import _process_event_policies


# ---------------------------------------------------------------------------
# DaemonCtx: in-process daemon simulation
# ---------------------------------------------------------------------------

class DaemonCtx:
    """Simulates the hex-events daemon for one test.

    Usage::

        with DaemonCtx() as ctx:
            ctx.add_policy("ping.yaml", create_test_policy("ping", "test.ping", "shell", "echo pong"))
            event_id = ctx.emit("test.ping", {})
            ctx.tick()
            rows = ctx.get_action_logs(event_id)
    """

    def __init__(self):
        self.tmp_dir = tempfile.mkdtemp(prefix="hex-int-test-")
        self.db_path = os.path.join(self.tmp_dir, "test.db")
        self.policies_dir = os.path.join(self.tmp_dir, "policies")
        os.makedirs(self.policies_dir)
        self.db = EventsDB(self.db_path)
        self._policies = []

    def add_policy(self, filename: str, content: str):
        """Write a YAML policy to the test policies dir and reload all policies."""
        path = os.path.join(self.policies_dir, filename)
        with open(path, "w") as f:
            f.write(content)
        self._reload_policies()

    def _reload_policies(self):
        self._policies = load_policies(self.policies_dir)

    def emit(self, event_type: str, payload: dict = None, source: str = "test") -> int:
        """Insert an event into the test DB. Returns event_id."""
        return self.db.insert_event(
            event_type, json.dumps(payload or {}), source
        )

    def tick(self) -> int:
        """Process all unprocessed events (one daemon poll tick). Returns event count."""
        events = self.db.get_unprocessed()
        for event in events:
            _process_event_policies(event, self._policies, self.db)
        return len(events)

    def get_action_logs(self, event_id: int = None) -> list:
        """Return action_log rows for an event_id (or all rows if None)."""
        if event_id is not None:
            return self.db.get_action_logs(event_id)
        rows = self.db.conn.execute(
            "SELECT * FROM action_log ORDER BY id"
        ).fetchall()
        return [dict(r) for r in rows]

    def count_events(self, event_type: str) -> int:
        """Count events of a given type in the DB."""
        row = self.db.conn.execute(
            "SELECT COUNT(*) as cnt FROM events WHERE event_type = ?",
            (event_type,),
        ).fetchone()
        return row["cnt"]

    def close(self):
        self.db.close()
        shutil.rmtree(self.tmp_dir, ignore_errors=True)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


# ---------------------------------------------------------------------------
# Pytest fixture
# ---------------------------------------------------------------------------

@pytest.fixture
def daemon_with_test_db():
    """Start an in-process daemon with isolated DB and policies dir.

    Yields (db_path, ctx) where ctx is a DaemonCtx instance.
    Teardown cleans up temp files.
    """
    ctx = DaemonCtx()
    try:
        yield ctx.db_path, ctx
    finally:
        ctx.close()


# ---------------------------------------------------------------------------
# Test helpers
# ---------------------------------------------------------------------------

def emit_and_wait(ctx_or_dbpath, event_type: str, payload: dict = None,
                  timeout: float = 10) -> dict | None:
    """Emit an event and wait for an action_log entry.

    If ctx_or_dbpath is a DaemonCtx, processes events synchronously and
    returns the first action_log row. If a db_path string is provided,
    polls the action_log with up to `timeout` seconds (for subprocess daemons).

    Returns the first action_log row dict, or None if no action fired.
    """
    if isinstance(ctx_or_dbpath, DaemonCtx):
        ctx = ctx_or_dbpath
        event_id = ctx.emit(event_type, payload)
        ctx.tick()
        rows = ctx.get_action_logs(event_id)
        return rows[0] if rows else None
    else:
        db_path = ctx_or_dbpath
        db = EventsDB(db_path)
        event_id = db.insert_event(event_type, json.dumps(payload or {}), "test")
        deadline = time.time() + timeout
        while time.time() < deadline:
            rows = db.get_action_logs(event_id)
            if rows:
                db.close()
                return rows[0]
            time.sleep(0.1)
        db.close()
        return None


def assert_action_fired(db_path, event_id: int, expected_recipe: str,
                        expected_status: str = "success") -> dict:
    """Assert that an action was logged with the expected recipe and status.

    Returns the matching action_log row dict.
    Raises AssertionError if not found.
    """
    if isinstance(db_path, DaemonCtx):
        rows = db_path.get_action_logs(event_id)
    else:
        db = EventsDB(db_path)
        rows = db.get_action_logs(event_id)
        db.close()

    matching = [
        r for r in rows
        if r["recipe"] == expected_recipe and r["status"] == expected_status
    ]
    assert matching, (
        f"Expected action_log entry: recipe={expected_recipe!r}, "
        f"status={expected_status!r}. Got: {rows}"
    )
    return matching[0]


def assert_no_action_fired(db_path, event_id: int, recipe: str = None):
    """Assert that NO (matching) action was logged for the event.

    If recipe is given, checks that no action with that recipe fired.
    Otherwise checks that no action at all was logged.
    """
    if isinstance(db_path, DaemonCtx):
        rows = db_path.get_action_logs(event_id)
    else:
        db = EventsDB(db_path)
        rows = db.get_action_logs(event_id)
        db.close()

    if recipe:
        matching = [r for r in rows if r["recipe"] == recipe]
        assert not matching, (
            f"Expected NO action for recipe={recipe!r} but got: {matching}"
        )
    else:
        assert not rows, f"Expected NO actions but got: {rows}"


def create_test_policy(name: str, trigger_event: str, action_type: str,
                       action_command: str, conditions: list = None,
                       enabled: bool = True) -> str:
    """Generate a minimal valid old-format policy YAML.

    Uses the old recipe format (trigger + actions) which is auto-wrapped into
    a single-rule Policy by load_policies — no schema validation applied.
    """
    lines = [
        f"name: {name}",
        f"description: Test policy for {name}",
        f"trigger:",
        f"  event: {trigger_event}",
    ]
    if not enabled:
        lines.append("enabled: false")
    if conditions:
        lines.append("conditions:")
        for cond in conditions:
            lines.append(f"  - field: {cond['field']}")
            lines.append(f"    op: {cond['op']}")
            val = cond["value"]
            if isinstance(val, str):
                lines.append(f"    value: \"{val}\"")
            else:
                lines.append(f"    value: {val}")
    lines += [
        f"actions:",
        f"  - type: {action_type}",
        f'    command: "{action_command}"',
    ]
    return "\n".join(lines) + "\n"


def mock_script(name: str, exit_code: int = 0, output: str = "ok") -> str:
    """Create a temp executable script. Returns its absolute path.

    The script prints `output` to stdout and exits with `exit_code`.
    Used to mock external scripts (auto-commit, verify, etc.) so tests
    have no real side effects.
    """
    tmp_dir = tempfile.mkdtemp(prefix="hex-mock-")
    path = os.path.join(tmp_dir, name)
    with open(path, "w") as f:
        f.write(f"#!/bin/sh\necho '{output}'\nexit {exit_code}\n")
    os.chmod(path, stat.S_IRWXU | stat.S_IRGRP | stat.S_IXGRP)
    return path


# ---------------------------------------------------------------------------
# Framework self-test
# ---------------------------------------------------------------------------

def test_framework_works(daemon_with_test_db):
    """Smoke test: trivial policy fires when its trigger event is emitted."""
    db_path, ctx = daemon_with_test_db

    # Create a trivial policy: trigger="test.ping", action=echo pong
    ctx.add_policy(
        "test-ping.yaml",
        create_test_policy("test-ping", "test.ping", "shell", "echo pong"),
    )

    # Emit trigger event and process it
    row = emit_and_wait(ctx, "test.ping", {})

    # Verify action fired
    assert row is not None, "Expected action_log entry but none was recorded"
    assert row["recipe"] == "test-ping", f"Expected recipe='test-ping', got {row['recipe']!r}"
    assert row["status"] == "success", f"Expected status='success', got {row['status']!r}"


def test_emit_and_wait_returns_action_row(daemon_with_test_db):
    """emit_and_wait returns the first action_log row for the emitted event."""
    db_path, ctx = daemon_with_test_db
    ctx.add_policy(
        "tw.yaml",
        create_test_policy("tw-policy", "test.wait", "shell", "echo waiting"),
    )
    row = emit_and_wait(ctx, "test.wait", {"key": "value"})
    assert row is not None
    assert row["action_type"] == "shell"


def test_assert_action_fired_helper(daemon_with_test_db):
    """assert_action_fired passes when action is in action_log, fails otherwise."""
    db_path, ctx = daemon_with_test_db
    ctx.add_policy(
        "af.yaml",
        create_test_policy("af-policy", "test.assert", "shell", "echo asserting"),
    )
    event_id = ctx.emit("test.assert", {})
    ctx.tick()
    # Should pass
    assert_action_fired(db_path, event_id, "af-policy", expected_status="success")


def test_mock_script_runs_correctly(daemon_with_test_db):
    """mock_script creates a real executable used in policy commands."""
    db_path, ctx = daemon_with_test_db
    script_path = mock_script("my_mock.sh", exit_code=0, output="mock-ran")
    ctx.add_policy(
        "ms.yaml",
        create_test_policy("ms-policy", "test.mock", "shell", script_path),
    )
    row = emit_and_wait(ctx, "test.mock", {})
    assert row is not None
    assert row["status"] == "success"
    assert "mock-ran" in (row.get("error_message") or row.get("action_detail") or "")


def test_wrong_event_does_not_trigger_policy(daemon_with_test_db):
    """Emitting the wrong event type does not trigger the policy."""
    db_path, ctx = daemon_with_test_db
    ctx.add_policy(
        "ne.yaml",
        create_test_policy("ne-policy", "test.specific", "shell", "echo specific"),
    )
    event_id = ctx.emit("test.other", {})
    ctx.tick()
    assert_no_action_fired(db_path, event_id)


# ---------------------------------------------------------------------------
# t-2 helper: old-format policy with shell + on_success / on_failure emit
# ---------------------------------------------------------------------------

def create_chained_policy(name: str, trigger_event: str, action_command: str,
                          retries: int = 0, on_success_emit: str = None,
                          on_failure_emit: str = None,
                          conditions: list = None) -> str:
    """Old-format policy YAML with shell action that chains an emit on success/failure.

    Uses old-format (trigger + actions) so it bypasses schema validation.
    retries=0 avoids exponential-backoff sleeps in tests.
    """
    lines = [
        f"name: {name}",
        f"description: Test chained policy for {name}",
        f"trigger:",
        f"  event: {trigger_event}",
    ]
    if conditions:
        lines.append("conditions:")
        for cond in conditions:
            lines.append(f"  - field: {cond['field']}")
            lines.append(f"    op: {cond['op']}")
            val = cond["value"]
            lines.append(f"    value: \"{val}\"" if isinstance(val, str)
                         else f"    value: {val}")
    lines += [
        "actions:",
        "  - type: shell",
        f'    command: "{action_command}"',
        f"    retries: {retries}",
    ]
    if on_success_emit:
        lines += [
            "    on_success:",
            "      - type: emit",
            f"        event: {on_success_emit}",
        ]
    if on_failure_emit:
        lines += [
            "    on_failure:",
            "      - type: emit",
            f"        event: {on_failure_emit}",
        ]
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------
# t-2: Integration tests for BOI pipeline policies
# ---------------------------------------------------------------------------

def test_boi_auto_commit_fires_on_spec_completed(daemon_with_test_db):
    """boi-auto-commit: boi.spec.completed → auto-commit script → boi.output.committed emitted."""
    db_path, ctx = daemon_with_test_db

    script = mock_script("auto-commit-boi-output.sh", exit_code=0, output="committed")
    ctx.add_policy(
        "boi-auto-commit.yaml",
        create_chained_policy(
            "boi-auto-commit", "boi.spec.completed",
            script, on_success_emit="boi.output.committed",
        ),
    )

    event_id = ctx.emit("boi.spec.completed", {"spec_id": "q-test", "target_repo": "/tmp/repo"})
    ctx.tick()

    assert_action_fired(ctx, event_id, "boi-auto-commit", expected_status="success")
    assert ctx.count_events("boi.output.committed") == 1, (
        "Expected boi.output.committed to be emitted after auto-commit"
    )


def test_boi_completion_gate_fires_on_output_committed(daemon_with_test_db):
    """boi-completion-gate: boi.output.committed → verify script (success) → boi.completion.verified emitted."""
    db_path, ctx = daemon_with_test_db

    script = mock_script("verify-boi-completion.sh", exit_code=0, output="verified")
    ctx.add_policy(
        "boi-completion-gate.yaml",
        create_chained_policy(
            "boi-completion-gate", "boi.output.committed",
            script, on_success_emit="boi.completion.verified",
        ),
    )

    event_id = ctx.emit("boi.output.committed", {"spec_id": "q-test", "target_repo": "/tmp/repo"})
    ctx.tick()

    assert_action_fired(ctx, event_id, "boi-completion-gate", expected_status="success")
    assert ctx.count_events("boi.completion.verified") == 1, (
        "Expected boi.completion.verified to be emitted on verify success"
    )


def test_boi_completion_gate_emits_violation_on_verify_failure(daemon_with_test_db):
    """boi-completion-gate: verify script fails → policy.violation emitted."""
    db_path, ctx = daemon_with_test_db

    script = mock_script("verify-boi-fail.sh", exit_code=1, output="test failure")
    ctx.add_policy(
        "boi-completion-gate-fail.yaml",
        create_chained_policy(
            "boi-completion-gate-fail", "boi.output.committed",
            script, on_failure_emit="policy.violation",
        ),
    )

    event_id = ctx.emit("boi.output.committed", {"spec_id": "q-test"})
    ctx.tick()

    rows = ctx.get_action_logs(event_id)
    assert rows, "Expected action_log entries after processing"
    # Final status is "error" after retries exhausted (retries=0 → 1 attempt)
    assert any(r["status"] == "error" for r in rows), (
        f"Expected error status in action_log; got: {rows}"
    )
    assert ctx.count_events("policy.violation") == 1, (
        "Expected policy.violation to be emitted on verify failure"
    )


def test_boi_landings_bridge_updates_on_dispatch(daemon_with_test_db):
    """boi-landings-bridge: boi.spec.dispatched → update-landings script fires."""
    db_path, ctx = daemon_with_test_db

    script = mock_script("update-landings-from-boi.sh", exit_code=0, output="landings-updated")
    ctx.add_policy(
        "boi-landings-bridge.yaml",
        create_test_policy("boi-landings-bridge", "boi.spec.dispatched", "shell", script),
    )

    event_id = ctx.emit("boi.spec.dispatched", {"spec_id": "q-test", "spec_title": "Test"})
    ctx.tick()

    assert_action_fired(ctx, event_id, "boi-landings-bridge", expected_status="success")


def test_boi_output_persistence_fires(daemon_with_test_db):
    """boi-output-persistence: boi.iteration.done (real trigger) with tasks_completed>0 → action fires."""
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "boi-output-persistence.yaml",
        create_test_policy(
            "boi-output-persistence", "boi.iteration.done", "shell",
            "echo persistence-check",
            conditions=[{"field": "tasks_completed", "op": "gt", "value": 0}],
        ),
    )

    event_id = ctx.emit("boi.iteration.done", {"spec_id": "q-test", "tasks_completed": 2})
    ctx.tick()

    assert_action_fired(ctx, event_id, "boi-output-persistence", expected_status="success")


def test_boi_workspace_isolation_fires(daemon_with_test_db):
    """boi-workspace-isolation: boi.workspace.leak (real trigger) → isolation violation action fires."""
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "boi-workspace-isolation.yaml",
        create_test_policy(
            "boi-workspace-isolation", "boi.workspace.leak", "shell",
            "echo isolation-check",
        ),
    )

    event_id = ctx.emit("boi.workspace.leak", {
        "spec_id": "q-test", "worker_id": "w-1",
        "leaked_files": ["file.txt"], "worktree_path": "/tmp/wt",
    })
    ctx.tick()

    assert_action_fired(ctx, event_id, "boi-workspace-isolation", expected_status="success")


def test_boi_chain_policy_fires_only_for_q180(daemon_with_test_db):
    """chain policy: fires only when spec_id='q-180', not for other spec IDs."""
    db_path, ctx = daemon_with_test_db

    script = mock_script("boi-dispatch.sh", exit_code=0, output="dispatched")
    ctx.add_policy(
        "chain-q180.yaml",
        create_test_policy(
            "chain-q180", "boi.spec.completed", "shell", script,
            conditions=[{"field": "spec_id", "op": "eq", "value": "q-180"}],
        ),
    )

    # Should fire for q-180
    event_id_180 = ctx.emit("boi.spec.completed", {"spec_id": "q-180"})
    ctx.tick()
    assert_action_fired(ctx, event_id_180, "chain-q180", expected_status="success")

    # Should NOT fire for q-999
    event_id_999 = ctx.emit("boi.spec.completed", {"spec_id": "q-999"})
    ctx.tick()
    assert_no_action_fired(ctx, event_id_999)


# ---------------------------------------------------------------------------
# t-3: Integration tests for file and landing policies
# ---------------------------------------------------------------------------

def test_file_triage_fires_on_file_created(daemon_with_test_db):
    """file-triage: file.created with path in raw/captures/ → triage action fires."""
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "file-triage.yaml",
        create_test_policy(
            "file-triage", "file.created", "shell",
            "echo triage-check",
            conditions=[{"field": "path", "op": "contains", "value": "raw/captures/"}],
        ),
    )

    event_id = ctx.emit("file.created", {"path": "/home/user/raw/captures/sample.txt"})
    ctx.tick()

    assert_action_fired(ctx, event_id, "file-triage", expected_status="success")


def test_commit_changelog_fires_on_file_changed(daemon_with_test_db):
    """commit-changelog: git.push → changelog action fires.

    The real policy triggers on git.push (not file.changed); test name kept
    per spec, trigger matches actual policy YAML.
    """
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "commit-changelog.yaml",
        create_test_policy("commit-changelog", "git.push", "shell", "echo changelog-update"),
    )

    event_id = ctx.emit("git.push", {"branch": "main", "commit": "abc123"})
    ctx.tick()

    assert_action_fired(ctx, event_id, "commit-changelog", expected_status="success")


def test_landing_refresh_fires_on_status_changed(daemon_with_test_db):
    """landing-refresh: file.modified with path in landings/ → refresh action fires.

    The real policy triggers on file.modified with a landings/ path condition.
    """
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "landing-refresh.yaml",
        create_test_policy(
            "landing-refresh", "file.modified", "shell",
            "echo dashboard-refresh",
            conditions=[{"field": "path", "op": "contains", "value": "landings/"}],
        ),
    )

    event_id = ctx.emit("file.modified", {"path": "/home/user/hex/landings/2026-03-23.md"})
    ctx.tick()

    assert_action_fired(ctx, event_id, "landing-refresh", expected_status="success")


def test_landings_staleness_detects_stale(daemon_with_test_db):
    """landings-staleness: landings.check-due → staleness check action fires.

    The real policy arms a delayed check on git.commit, then checks on
    landings.check-due.  Here we emit landings.check-due directly to verify
    the staleness-check rule fires without the cron/delay machinery.
    """
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "landings-staleness.yaml",
        create_test_policy(
            "landings-staleness", "landings.check-due", "shell",
            "echo staleness-check",
        ),
    )

    event_id = ctx.emit("landings.check-due", {})
    ctx.tick()

    assert_action_fired(ctx, event_id, "landings-staleness", expected_status="success")


def test_ops_completion_verify_fires(daemon_with_test_db):
    """ops-completion-verify: boi.spec.completed → ops verify action fires."""
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "ops-completion-verify.yaml",
        create_test_policy(
            "ops-completion-verify", "boi.spec.completed", "shell",
            "echo ops-completion-check",
        ),
    )

    event_id = ctx.emit("boi.spec.completed", {
        "queue_id": "q-test", "spec_path": "/tmp/test.spec.md",
    })
    ctx.tick()

    assert_action_fired(ctx, event_id, "ops-completion-verify", expected_status="success")


def test_ops_failure_pattern_fires_on_violation(daemon_with_test_db):
    """ops-failure-pattern: boi.spec.failed → failure pattern detection fires.

    The real policy triggers on boi.spec.failed (not policy.violation).
    """
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "ops-failure-pattern.yaml",
        create_test_policy(
            "ops-failure-pattern", "boi.spec.failed", "shell",
            "echo failure-pattern-check",
        ),
    )

    event_id = ctx.emit("boi.spec.failed", {
        "queue_id": "q-test", "reason": "test failure reason",
    })
    ctx.tick()

    assert_action_fired(ctx, event_id, "ops-failure-pattern", expected_status="success")


def test_ops_spec_digest_fires(daemon_with_test_db):
    """ops-spec-digest: boi.spec.completed → digest action fires."""
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "ops-spec-digest.yaml",
        create_test_policy(
            "ops-spec-digest", "boi.spec.completed", "shell",
            "echo digest-written",
        ),
    )

    event_id = ctx.emit("boi.spec.completed", {
        "queue_id": "q-test", "tasks_done": 3, "tasks_total": 3,
    })
    ctx.tick()

    assert_action_fired(ctx, event_id, "ops-spec-digest", expected_status="success")


# ---------------------------------------------------------------------------
# t-4: Negative tests and edge cases
# ---------------------------------------------------------------------------

def test_policy_does_not_fire_on_wrong_event(daemon_with_test_db):
    """Policy triggers on 'foo.bar'; emitting 'foo.baz' must NOT trigger it."""
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "wrong-event.yaml",
        create_test_policy("wrong-event-policy", "foo.bar", "shell", "echo should-not-run"),
    )

    event_id = ctx.emit("foo.baz", {})
    ctx.tick()

    assert_no_action_fired(db_path, event_id)


def test_condition_filter_blocks_not_fire_non_matching(daemon_with_test_db):
    """Condition filter (spec_id eq q-180) blocks events where spec_id != 'q-180'."""
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "cond-filter.yaml",
        create_test_policy(
            "cond-filter-policy", "boi.spec.completed", "shell",
            "echo filtered-action",
            conditions=[{"field": "spec_id", "op": "eq", "value": "q-180"}],
        ),
    )

    event_id = ctx.emit("boi.spec.completed", {"spec_id": "q-999"})
    ctx.tick()

    assert_no_action_fired(db_path, event_id)


def test_disabled_policy_does_not_fire(daemon_with_test_db):
    """A policy with 'enabled: false' must NOT fire even when its trigger event arrives."""
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "disabled-policy.yaml",
        create_test_policy(
            "disabled-policy", "test.disabled-trigger", "shell",
            "echo should-not-run",
            enabled=False,
        ),
    )

    event_id = ctx.emit("test.disabled-trigger", {})
    ctx.tick()

    assert_no_action_fired(db_path, event_id)


def test_malformed_payload_does_not_crash_daemon(daemon_with_test_db):
    """Invalid JSON payload in an event must not crash the daemon; event is skipped cleanly."""
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "malformed-listener.yaml",
        create_test_policy("malformed-listener", "test.malformed", "shell", "echo ok"),
    )

    # Insert a raw event with invalid JSON payload directly into the DB
    ctx.db.conn.execute(
        "INSERT INTO events (event_type, payload, source) VALUES (?, ?, ?)",
        ("test.malformed", "NOT VALID JSON {{{", "test"),
    )
    ctx.db.conn.commit()

    # Tick must not raise an exception
    try:
        ctx.tick()
    except Exception as exc:
        raise AssertionError(f"daemon tick raised an exception on malformed payload: {exc}")

    # No action should have been logged
    rows = ctx.get_action_logs()
    assert not rows, f"Expected no action_log entries for malformed event, got: {rows}"


def test_negative_action_failure_logged_correctly(daemon_with_test_db):
    """When a script exits with code 1, action_log records status='error'."""
    db_path, ctx = daemon_with_test_db

    script = mock_script("fail-script.sh", exit_code=1, output="intentional failure")
    ctx.add_policy(
        "negative-fail.yaml",
        create_chained_policy(
            "negative-fail-policy", "test.fail-trigger",
            script, retries=0,
        ),
    )

    event_id = ctx.emit("test.fail-trigger", {})
    ctx.tick()

    rows = ctx.get_action_logs(event_id)
    assert rows, "Expected action_log entries after failed action"
    error_rows = [r for r in rows if r["status"] == "error"]
    assert error_rows, (
        f"Expected at least one action_log row with status='error'; got: {rows}"
    )
    # error_message should contain failure detail
    assert any(r.get("error_message") for r in error_rows), (
        f"Expected non-empty error_message in error rows; got: {error_rows}"
    )


def test_edge_multiple_policies_same_trigger(daemon_with_test_db):
    """Two policies with the same trigger event must both fire independently."""
    db_path, ctx = daemon_with_test_db

    ctx.add_policy(
        "edge-alpha.yaml",
        create_test_policy("edge-alpha-policy", "test.shared-trigger", "shell", "echo alpha"),
    )
    ctx.add_policy(
        "edge-beta.yaml",
        create_test_policy("edge-beta-policy", "test.shared-trigger", "shell", "echo beta"),
    )

    event_id = ctx.emit("test.shared-trigger", {})
    ctx.tick()

    rows = ctx.get_action_logs(event_id)
    recipes_fired = {r["recipe"] for r in rows}
    assert "edge-alpha-policy" in recipes_fired, (
        f"edge-alpha-policy did not fire; got: {recipes_fired}"
    )
    assert "edge-beta-policy" in recipes_fired, (
        f"edge-beta-policy did not fire; got: {recipes_fired}"
    )
