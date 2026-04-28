"""test_daemon_integration.py — Full daemon lifecycle integration test.

Tests the complete hex-events event lifecycle WITHOUT Claude Code:
  1. Starts hex_eventd.py as a subprocess with an isolated temp DB and policies dir
  2. Emits a test event via the EventsDB API
  3. Polls the action_log table for up to 15 seconds
  4. Verifies the shell action fired (action_log + side-effect file)
  5. Stops the daemon cleanly

Design notes:
- Uses a wrapper script to override the module-level globals (BASE_DIR, DB_PATH, etc.)
  that the daemon reads at import time. The wrapper patches these before run_daemon() runs.
- Each test gets a unique temp directory so there's no lock conflict with a running
  production daemon or other parallel test runs.
- The side-effect file (/tmp/hex-events-integration-result-<uuid>.txt) proves that the
  shell action actually executed, not just that it was logged.
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import uuid

import pytest
import yaml

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, _REPO_ROOT)

from db import EventsDB


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

DAEMON_POLL_INTERVAL = 2      # seconds between daemon event-loop ticks
MAX_WAIT_SECONDS = 15         # maximum time to wait for action to fire
POLL_STEP = 0.5               # how often the test polls the DB


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _write_test_policy(policies_dir: str, result_file: str) -> str:
    """Write the minimal integration test policy YAML. Returns the policy file path."""
    policy = {
        "name": "test-integration",
        "description": "Test policy for daemon integration testing",
        "rules": [
            {
                "name": "test-rule",
                "trigger": {"event": "test.integration.check"},
                "conditions": [
                    {"field": "status", "op": "eq", "value": "fired"},
                ],
                "actions": [
                    {
                        "type": "shell",
                        "command": f"echo 'INTEGRATION_TEST_PASSED' > {result_file}",
                    }
                ],
            }
        ],
    }
    path = os.path.join(policies_dir, "test-integration.yaml")
    with open(path, "w") as f:
        yaml.dump(policy, f, default_flow_style=False)
    return path


def _write_daemon_wrapper(tmp_dir: str, db_path: str, policies_dir: str) -> str:
    """Write a Python wrapper script that patches daemon globals then calls run_daemon().

    This is necessary because hex_eventd.py reads BASE_DIR/DB_PATH/etc. as module-level
    globals at import time. By running a wrapper that sets them BEFORE importing
    hex_eventd, we get a clean isolated daemon pointed at our test paths.
    """
    # Scheduler config path (required by SchedulerAdapter; stub with a no-op file)
    scheduler_config = os.path.join(tmp_dir, "scheduler.yaml")
    with open(scheduler_config, "w") as f:
        f.write("jobs: []\n")

    wrapper_path = os.path.join(tmp_dir, "daemon_wrapper.py")
    wrapper_code = textwrap.dedent(f"""\
        #!/usr/bin/env python3
        import sys, os
        sys.path.insert(0, {_REPO_ROOT!r})

        # Patch module-level globals BEFORE importing hex_eventd
        import hex_eventd
        hex_eventd.BASE_DIR = {tmp_dir!r}
        hex_eventd.DB_PATH = {db_path!r}
        hex_eventd.PID_FILE = os.path.join({tmp_dir!r}, "hex_eventd.pid")
        hex_eventd.LOCK_FILE = os.path.join({tmp_dir!r}, "hex_eventd.lock")
        hex_eventd.LOG_FILE = os.path.join({tmp_dir!r}, "daemon.log")
        hex_eventd.HEALTH_FILE = os.path.join({tmp_dir!r}, "health.json")
        hex_eventd.POLICIES_DIR = {policies_dir!r}
        hex_eventd.SCHEDULER_CONFIG = {scheduler_config!r}
        hex_eventd.POLL_INTERVAL = 1  # faster ticks for tests

        hex_eventd.run_daemon()
    """)
    with open(wrapper_path, "w") as f:
        f.write(wrapper_code)
    os.chmod(wrapper_path, 0o755)
    return wrapper_path


def _wait_for_action_log(db: EventsDB, event_id: int, timeout: float = MAX_WAIT_SECONDS) -> list:
    """Poll action_log until an entry appears for event_id or timeout expires."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        logs = db.get_action_logs(event_id)
        if logs:
            return logs
        time.sleep(POLL_STEP)
    return []


def _wait_for_file(path: str, timeout: float = MAX_WAIT_SECONDS) -> bool:
    """Poll until a file exists at path or timeout expires. Returns True if found."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if os.path.exists(path):
            return True
        time.sleep(POLL_STEP)
    return False


def _stop_daemon(proc: subprocess.Popen, timeout: float = 5.0):
    """Send SIGTERM to the daemon subprocess and wait for clean exit."""
    if proc.poll() is not None:
        return  # already exited
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=2)


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def integration_env(tmp_path):
    """Set up an isolated daemon environment: temp dir, DB, policies dir, result file."""
    # Unique run ID to avoid collisions if tests run in parallel
    run_id = uuid.uuid4().hex[:8]

    db_path = str(tmp_path / "events.db")
    policies_dir = str(tmp_path / "policies")
    os.makedirs(policies_dir)
    result_file = f"/tmp/hex-events-integration-result-{run_id}.txt"

    # Pre-create the DB so the test can insert events before the daemon starts
    db = EventsDB(db_path)

    # Write the test policy
    _write_test_policy(policies_dir, result_file)

    yield {
        "tmp_dir": str(tmp_path),
        "db_path": db_path,
        "db": db,
        "policies_dir": policies_dir,
        "result_file": result_file,
    }

    # Cleanup: close DB, remove result file
    db.close()
    if os.path.exists(result_file):
        os.remove(result_file)


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

class TestDaemonIntegration:
    """Full daemon lifecycle integration tests."""

    def test_daemon_starts_and_processes_event(self, integration_env):
        """Daemon starts, event is emitted, shell action fires, side-effect verified."""
        env = integration_env
        db: EventsDB = env["db"]
        result_file = env["result_file"]

        # Write the daemon wrapper pointing at our temp paths
        wrapper = _write_daemon_wrapper(env["tmp_dir"], env["db_path"], env["policies_dir"])

        # Start the daemon
        daemon = subprocess.Popen(
            [sys.executable, wrapper],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        try:
            # Give the daemon a moment to initialize and load policies
            time.sleep(DAEMON_POLL_INTERVAL + 0.5)

            # Emit the test event
            event_id = db.insert_event(
                "test.integration.check",
                json.dumps({"status": "fired"}),
                "test-harness",
            )
            assert event_id is not None, "Event insertion failed"

            # Wait for the action_log entry
            logs = _wait_for_action_log(db, event_id)
            assert logs, (
                f"No action_log entry appeared within {MAX_WAIT_SECONDS}s "
                f"for event_id={event_id}. "
                f"Daemon log: {env['tmp_dir']}/daemon.log"
            )

            # Verify the action succeeded
            success_logs = [l for l in logs if l["status"] == "success"]
            assert success_logs, (
                f"Action log exists but no 'success' status. Got: {logs}"
            )

            # Verify the shell action's side effect
            assert _wait_for_file(result_file), (
                f"Side-effect file {result_file!r} was not created within {MAX_WAIT_SECONDS}s"
            )

            with open(result_file) as f:
                content = f.read().strip()
            assert "INTEGRATION_TEST_PASSED" in content

            # Verify the event was marked as processed
            unprocessed = db.get_unprocessed()
            assert not any(e["id"] == event_id for e in unprocessed), (
                "Event was not marked as processed"
            )

        finally:
            _stop_daemon(daemon)

    def test_condition_mismatch_blocks_action(self, integration_env):
        """Event with wrong payload should not trigger the shell action."""
        env = integration_env
        db: EventsDB = env["db"]
        result_file = env["result_file"]

        wrapper = _write_daemon_wrapper(env["tmp_dir"], env["db_path"], env["policies_dir"])
        daemon = subprocess.Popen(
            [sys.executable, wrapper],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        try:
            time.sleep(DAEMON_POLL_INTERVAL + 0.5)

            # Emit event with wrong status (condition expects "fired")
            event_id = db.insert_event(
                "test.integration.check",
                json.dumps({"status": "not-fired"}),
                "test-harness",
            )
            assert event_id is not None

            # Wait long enough for the daemon to process it
            deadline = time.time() + MAX_WAIT_SECONDS
            processed = False
            while time.time() < deadline:
                unprocessed = db.get_unprocessed()
                if not any(e["id"] == event_id for e in unprocessed):
                    processed = True
                    break
                time.sleep(POLL_STEP)

            assert processed, "Event was never marked as processed"

            # No action should have been logged (condition blocked it)
            logs = db.get_action_logs(event_id)
            assert not logs, (
                f"Action log should be empty for blocked event, got: {logs}"
            )

            # Side-effect file should NOT exist
            assert not os.path.exists(result_file), (
                f"Result file {result_file!r} exists but should not (condition was not met)"
            )

        finally:
            _stop_daemon(daemon)

    def test_daemon_hot_reloads_policies(self, integration_env):
        """Daemon should pick up a newly added policy without restart."""
        env = integration_env
        db: EventsDB = env["db"]

        # Start the daemon with only the existing test policy
        wrapper = _write_daemon_wrapper(env["tmp_dir"], env["db_path"], env["policies_dir"])
        daemon = subprocess.Popen(
            [sys.executable, wrapper],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        try:
            time.sleep(DAEMON_POLL_INTERVAL + 0.5)

            # Add a second policy dynamically
            hot_result = f"/tmp/hex-events-hot-reload-{uuid.uuid4().hex[:8]}.txt"
            hot_policy = {
                "name": "test-hot-reload",
                "description": "Policy added after daemon started",
                "rules": [
                    {
                        "name": "hot-rule",
                        "trigger": {"event": "test.integration.hot"},
                        "actions": [
                            {
                                "type": "shell",
                                "command": f"echo 'HOT_RELOAD_OK' > {hot_result}",
                            }
                        ],
                    }
                ],
            }
            hot_path = os.path.join(env["policies_dir"], "hot-reload.yaml")
            with open(hot_path, "w") as f:
                yaml.dump(hot_policy, f, default_flow_style=False)

            # Wait for the daemon to hot-reload (reload interval is 10s in prod,
            # but our wrapper keeps the default). We wait up to 15s.
            time.sleep(11)

            # Emit an event for the new policy
            event_id = db.insert_event(
                "test.integration.hot",
                json.dumps({"msg": "hello"}),
                "test-harness",
            )
            assert event_id is not None

            logs = _wait_for_action_log(db, event_id)
            assert logs, (
                f"Hot-reloaded policy did not fire within {MAX_WAIT_SECONDS}s"
            )
            assert any(l["status"] == "success" for l in logs), (
                f"Hot-reload action did not succeed. Logs: {logs}"
            )

            assert _wait_for_file(hot_result), (
                f"Hot-reload side-effect file not created: {hot_result}"
            )

        finally:
            _stop_daemon(daemon)
            for f in [
                env.get("result_file"),
                locals().get("hot_result"),
            ]:
                if f and os.path.exists(f):
                    os.remove(f)

    def test_daemon_processes_multiple_events_sequentially(self, integration_env):
        """Daemon processes a queue of events in insertion order."""
        env = integration_env
        db: EventsDB = env["db"]
        result_file = env["result_file"]

        wrapper = _write_daemon_wrapper(env["tmp_dir"], env["db_path"], env["policies_dir"])
        daemon = subprocess.Popen(
            [sys.executable, wrapper],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        try:
            time.sleep(DAEMON_POLL_INTERVAL + 0.5)

            # Insert multiple events — the matching ones should fire, the non-matching should not
            matching_id = db.insert_event(
                "test.integration.check",
                json.dumps({"status": "fired"}),
                "test-harness",
            )
            noise_id = db.insert_event(
                "unrelated.event",
                json.dumps({"key": "value"}),
                "test-harness",
            )
            second_matching_id = db.insert_event(
                "test.integration.check",
                json.dumps({"status": "fired"}),
                "test-harness",
            )

            # Wait for all events to be processed
            deadline = time.time() + MAX_WAIT_SECONDS
            while time.time() < deadline:
                unprocessed = db.get_unprocessed()
                if not unprocessed:
                    break
                time.sleep(POLL_STEP)

            assert not db.get_unprocessed(), "Some events were not processed"

            # Both matching events should have action logs
            for eid in (matching_id, second_matching_id):
                logs = db.get_action_logs(eid)
                assert logs, f"No action log for event_id={eid}"
                assert any(l["status"] == "success" for l in logs), (
                    f"No success log for event_id={eid}: {logs}"
                )

            # Noise event should have no action log
            noise_logs = db.get_action_logs(noise_id)
            assert not noise_logs, f"Unexpected action log for noise event: {noise_logs}"

        finally:
            _stop_daemon(daemon)

    def test_daemon_stops_cleanly_on_sigterm(self, integration_env):
        """Daemon should exit with code 0 when sent SIGTERM."""
        env = integration_env

        wrapper = _write_daemon_wrapper(env["tmp_dir"], env["db_path"], env["policies_dir"])
        daemon = subprocess.Popen(
            [sys.executable, wrapper],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        # Give it a moment to start
        time.sleep(DAEMON_POLL_INTERVAL)
        assert daemon.poll() is None, "Daemon exited prematurely"

        # Send SIGTERM
        daemon.send_signal(signal.SIGTERM)
        try:
            retcode = daemon.wait(timeout=8)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
            retcode = -1

        assert retcode == 0, f"Daemon exited with code {retcode} after SIGTERM (expected 0)"
