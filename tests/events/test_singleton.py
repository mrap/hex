"""Integration tests for daemon singleton enforcement.

These tests launch hex_eventd.py as real subprocesses to verify
that only one instance runs at a time.
"""
import os
import subprocess
import sys
import tempfile
import time

# Path to the daemon script
DAEMON_SCRIPT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "hex_eventd.py")
PYTHON = sys.executable


def _start_daemon(home_dir):
    """Launch hex_eventd.py subprocess with isolated HOME."""
    # Create the minimum directory structure the daemon expects
    hex_home = os.path.join(home_dir, ".hex-events")
    os.makedirs(os.path.join(hex_home, "policies"), exist_ok=True)

    env = os.environ.copy()
    env["HOME"] = home_dir
    return subprocess.Popen(
        [PYTHON, DAEMON_SCRIPT],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def test_only_one_daemon_runs():
    """Two daemon launches: second should exit quickly, first should keep running."""
    with tempfile.TemporaryDirectory() as home_dir:
        proc1 = _start_daemon(home_dir)
        try:
            # Give first daemon time to initialize and acquire lock
            time.sleep(2)
            assert proc1.poll() is None, "First daemon should still be running"

            proc2 = _start_daemon(home_dir)
            try:
                # Second daemon should exit quickly (within 5s) because lock is held
                try:
                    exit_code = proc2.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc2.kill()
                    proc2.wait()
                    raise AssertionError(
                        "Second daemon did not exit — singleton lock is not implemented"
                    )

                assert exit_code == 0, f"Second daemon should exit cleanly (code 0), got {exit_code}"
                assert proc1.poll() is None, "First daemon should still be running after second exits"
            finally:
                if proc2.poll() is None:
                    proc2.kill()
                    proc2.wait()
        finally:
            proc1.terminate()
            try:
                proc1.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc1.kill()
                proc1.wait()


def test_daemon_starts_after_previous_exits():
    """After first daemon dies, a second launch should succeed (lock released)."""
    with tempfile.TemporaryDirectory() as home_dir:
        proc1 = _start_daemon(home_dir)
        time.sleep(1)
        assert proc1.poll() is None, "First daemon should be running"

        # Kill the first daemon — lock should be released on death
        proc1.terminate()
        try:
            proc1.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc1.kill()
            proc1.wait()

        # Small pause for OS to release the lock
        time.sleep(0.5)

        # Second daemon should now start successfully
        proc2 = _start_daemon(home_dir)
        try:
            time.sleep(2)
            assert proc2.poll() is None, (
                "Second daemon should be running after first exited — "
                "lock should have been released"
            )
        finally:
            proc2.terminate()
            try:
                proc2.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc2.kill()
                proc2.wait()
