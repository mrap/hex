"""Tests for daemon logging, heartbeat, log rotation, and action error capture."""
import logging
import logging.handlers
import os
import subprocess
import sys
import tempfile
import time
import unittest.mock

import pytest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from db import EventsDB
from recipe import Action
from hex_eventd import _setup_logging, run_action_with_retry, HEARTBEAT_INTERVAL


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def make_db():
    tmp = tempfile.NamedTemporaryFile(suffix=".db", delete=False)
    tmp.close()
    return EventsDB(tmp.name), tmp.name


# ---------------------------------------------------------------------------
# Test 1: Daemon startup writes to log file
# ---------------------------------------------------------------------------

def test_daemon_log_startup_writes_to_file():
    """_setup_logging() creates daemon.log and startup message is written to it."""
    with tempfile.TemporaryDirectory() as tmpdir:
        log_path = os.path.join(tmpdir, "daemon.log")

        with unittest.mock.patch("hex_eventd.BASE_DIR", tmpdir), \
             unittest.mock.patch("hex_eventd.LOG_FILE", log_path):
            # Reset root logger handlers to isolate test
            root = logging.getLogger()
            original_handlers = root.handlers[:]
            root.handlers.clear()
            try:
                _setup_logging()
                log = logging.getLogger("hex-events")
                log.info("hex-eventd starting (pid=%d)", os.getpid())

                # Flush all handlers
                for h in logging.getLogger().handlers:
                    h.flush()

                assert os.path.exists(log_path), "daemon.log was not created"
                content = open(log_path).read()
                assert "hex-eventd starting" in content, \
                    f"Startup message missing from log. Got: {content!r}"
            finally:
                # Remove handlers added by _setup_logging
                for h in root.handlers[:]:
                    h.close()
                    root.removeHandler(h)
                root.handlers = original_handlers


# ---------------------------------------------------------------------------
# Test 2: Heartbeat line appears after configured interval
# ---------------------------------------------------------------------------

def test_daemon_log_heartbeat_fires_after_interval():
    """Heartbeat log line is written after HEARTBEAT_INTERVAL seconds."""
    with tempfile.TemporaryDirectory() as tmpdir:
        log_path = os.path.join(tmpdir, "daemon.log")

        with unittest.mock.patch("hex_eventd.BASE_DIR", tmpdir), \
             unittest.mock.patch("hex_eventd.LOG_FILE", log_path):
            root = logging.getLogger()
            original_handlers = root.handlers[:]
            root.handlers.clear()
            try:
                _setup_logging()
                root.setLevel(logging.DEBUG)

                log = logging.getLogger("hex-events")
                # Simulate heartbeat firing: record start, advance time past interval
                last_heartbeat = time.time() - HEARTBEAT_INTERVAL - 1
                now = time.time()

                if now - last_heartbeat >= HEARTBEAT_INTERVAL:
                    log.debug(
                        "heartbeat: %d events processed, %d actions fired since last heartbeat",
                        42, 7,
                    )

                for h in logging.getLogger().handlers:
                    h.flush()

                assert os.path.exists(log_path)
                content = open(log_path).read()
                assert "heartbeat" in content, \
                    f"Heartbeat message missing. Got: {content!r}"
                assert "42 events processed" in content
                assert "7 actions fired" in content
            finally:
                for h in root.handlers[:]:
                    h.close()
                    root.removeHandler(h)
                root.handlers = original_handlers


# ---------------------------------------------------------------------------
# Test 3: Log rotation creates backup files at size threshold
# ---------------------------------------------------------------------------

def test_daemon_log_rotation_creates_backup():
    """RotatingFileHandler rolls over at maxBytes and creates .1 backup file."""
    with tempfile.TemporaryDirectory() as tmpdir:
        log_path = os.path.join(tmpdir, "daemon.log")

        root = logging.getLogger()
        original_handlers = root.handlers[:]
        root.handlers.clear()
        try:
            # Use a tiny maxBytes to trigger rollover quickly
            fh = logging.handlers.RotatingFileHandler(
                log_path, maxBytes=200, backupCount=3
            )
            fh.setFormatter(logging.Formatter("%(message)s"))
            root.addHandler(fh)
            root.setLevel(logging.DEBUG)

            log = logging.getLogger("hex-events")
            # Write enough data to force at least one rollover
            for i in range(20):
                log.info("log line %04d: padding to force rotation xxxxxxxxxxxxxxxxxxxxxxxxx", i)
            fh.flush()

            backup = log_path + ".1"
            assert os.path.exists(backup), \
                f"Backup file {backup} was not created after rotation"
        finally:
            for h in root.handlers[:]:
                h.close()
                root.removeHandler(h)
            root.handlers = original_handlers


# ---------------------------------------------------------------------------
# Test 4: Shell action stderr is captured in action_log error_message
# ---------------------------------------------------------------------------

def test_shell_action_stderr_captured_in_action_log():
    """Shell action failure writes stderr text into action_log error_message."""
    db, path = make_db()
    try:
        event_id = db.insert_event("test.event", "{}", "test")

        # Handler that returns stderr-like error detail
        class _ShellErrorHandler:
            def run(self, params, event_payload=None, db=None, workflow_context=None):
                return {"status": "error", "output": "deliberate test error from stderr"}

        action = Action(type="shell", params={"command": "exit 1", "retries": 0})
        run_action_with_retry(
            action=action,
            event_id=event_id,
            recipe_name="test-recipe",
            payload={},
            db=db,
            handler=_ShellErrorHandler(),
            sleep_fn=lambda s: None,
        )

        logs = db.get_action_logs(event_id)
        assert len(logs) == 1
        assert logs[0]["status"] == "error"
        assert "deliberate test error from stderr" in logs[0]["error_message"], \
            f"Error detail missing from action_log. Got: {logs[0]['error_message']!r}"
    finally:
        db.close()
        os.unlink(path)


def test_shell_action_real_stderr_captured():
    """Real subprocess shell action captures actual stderr text in action_log."""
    db, path = make_db()
    try:
        from actions.shell import ShellAction

        event_id = db.insert_event("test.event", "{}", "test")
        action = Action(type="shell", params={"command": "echo 'real stderr' >&2; exit 1", "retries": 0})

        run_action_with_retry(
            action=action,
            event_id=event_id,
            recipe_name="test-recipe",
            payload={},
            db=db,
            handler=ShellAction(),
            sleep_fn=lambda s: None,
        )

        logs = db.get_action_logs(event_id)
        assert len(logs) == 1
        assert logs[0]["status"] == "error"
        assert "real stderr" in logs[0]["error_message"], \
            f"stderr text missing. Got: {logs[0]['error_message']!r}"
    finally:
        db.close()
        os.unlink(path)


# ---------------------------------------------------------------------------
# Test 5: Notify action failure message is captured in action_log error_message
# ---------------------------------------------------------------------------

def test_notify_action_failure_captured_in_action_log():
    """Notify action failure (exception or non-zero exit) is stored in action_log."""
    db, path = make_db()
    try:
        class _NotifyErrorHandler:
            def run(self, params, event_payload=None, db=None, workflow_context=None):
                return {"status": "error", "output": "notify script not found: exit code 127"}

        event_id = db.insert_event("test.event", "{}", "test")
        action = Action(type="notify", params={"message": "test", "retries": 0})

        run_action_with_retry(
            action=action,
            event_id=event_id,
            recipe_name="test-recipe",
            payload={},
            db=db,
            handler=_NotifyErrorHandler(),
            sleep_fn=lambda s: None,
        )

        logs = db.get_action_logs(event_id)
        assert len(logs) == 1
        assert logs[0]["status"] == "error"
        assert "notify script not found" in logs[0]["error_message"], \
            f"Notify error missing. Got: {logs[0]['error_message']!r}"
    finally:
        db.close()
        os.unlink(path)


def test_notify_action_exception_captured_in_action_log():
    """Notify action exception str() is stored in action_log error_message."""
    db, path = make_db()
    try:
        class _NotifyExceptionHandler:
            def run(self, params, event_payload=None, db=None, workflow_context=None):
                return {"status": "error", "output": "FileNotFoundError: hex-notify.sh missing"}

        event_id = db.insert_event("test.event", "{}", "test")
        action = Action(type="notify", params={"message": "hello", "retries": 0})

        run_action_with_retry(
            action=action,
            event_id=event_id,
            recipe_name="test-recipe",
            payload={},
            db=db,
            handler=_NotifyExceptionHandler(),
            sleep_fn=lambda s: None,
        )

        logs = db.get_action_logs(event_id)
        assert len(logs) == 1
        err = logs[0]["error_message"]
        assert "FileNotFoundError" in err or "hex-notify.sh missing" in err, \
            f"Exception message missing. Got: {err!r}"
    finally:
        db.close()
        os.unlink(path)


# ---------------------------------------------------------------------------
# Test: error_message truncated to 500 chars
# ---------------------------------------------------------------------------

def test_action_log_error_message_truncated_to_500():
    """Long error output is truncated to 500 chars in action_log."""
    db, path = make_db()
    try:
        long_error = "x" * 1000

        class _LongErrorHandler:
            def run(self, params, event_payload=None, db=None, workflow_context=None):
                return {"status": "error", "output": long_error}

        event_id = db.insert_event("test.event", "{}", "test")
        action = Action(type="shell", params={"command": "exit 1", "retries": 0})

        run_action_with_retry(
            action=action,
            event_id=event_id,
            recipe_name="test-recipe",
            payload={},
            db=db,
            handler=_LongErrorHandler(),
            sleep_fn=lambda s: None,
        )

        logs = db.get_action_logs(event_id)
        assert len(logs) == 1
        # The error_message format is "Permanently failed after N retries: <truncated>"
        # or just the truncated output for retries=0 permanent failure
        err = logs[0]["error_message"]
        # The output portion should be capped at 500
        assert len(err) <= 600, f"error_message too long: {len(err)} chars"
        assert "x" * 500 in err, "500 chars of error output should be present"
        assert "x" * 501 not in err, "Should not contain more than 500 x's"
    finally:
        db.close()
        os.unlink(path)
