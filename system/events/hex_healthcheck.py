#!/usr/bin/env python3
"""hex-events health check.

Usage:
    hex_healthcheck.py --pre-start     Run before daemon starts (kill stale processes, clean DB)
    hex_healthcheck.py --check         Check if daemon is healthy (for monitoring)
    hex_healthcheck.py --watchdog      Run as a watchdog loop (restarts daemon if stuck)
"""
import json
import os
import signal
import sqlite3
import subprocess
import sys
import time

BASE_DIR = os.path.expanduser("~/.hex-events")
DB_PATH = os.path.join(BASE_DIR, "events.db")
PID_FILE = os.path.join(BASE_DIR, "hex_eventd.pid")
LOCK_FILE = os.path.join(BASE_DIR, "hex_eventd.lock")
HEALTH_FILE = os.path.join(BASE_DIR, "health.json")

# If no successful cycle in this many seconds, daemon is considered stuck
STUCK_THRESHOLD_SECONDS = 600  # 10 minutes


def _kill_stale_hex_eventd():
    """Kill any running hex_eventd.py processes."""
    killed = []
    my_pid = os.getpid()
    try:
        for entry in os.listdir("/proc"):
            if not entry.isdigit():
                continue
            pid = int(entry)
            if pid == my_pid:
                continue
            try:
                with open(f"/proc/{pid}/cmdline", "rb") as f:
                    cmdline = f.read().decode("utf-8", errors="replace")
                if "hex_eventd.py" in cmdline:
                    os.kill(pid, signal.SIGTERM)
                    killed.append(pid)
            except (OSError, PermissionError):
                continue
    except OSError:
        pass

    if killed:
        time.sleep(2)
        for pid in killed:
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        print(f"Killed stale daemon PIDs: {killed}", file=sys.stderr)

    return killed


def _clean_db_files():
    """Checkpoint WAL and clean stale journal files."""
    wal = DB_PATH + "-wal"
    shm = DB_PATH + "-shm"

    if not os.path.exists(DB_PATH):
        return

    try:
        conn = sqlite3.connect(DB_PATH, timeout=5)
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        conn.close()
        print("WAL checkpoint successful", file=sys.stderr)
    except sqlite3.OperationalError as e:
        print(f"WAL checkpoint failed ({e}), removing stale files", file=sys.stderr)
        for path in [wal, shm]:
            if os.path.exists(path):
                try:
                    os.remove(path)
                    print(f"Removed {path}", file=sys.stderr)
                except OSError:
                    pass


def _clean_lock_files():
    """Remove stale PID file. The .lock file is flock-based and self-cleans."""
    if os.path.exists(PID_FILE):
        try:
            with open(PID_FILE) as f:
                old_pid = int(f.read().strip())
            # Check if that PID is still alive
            try:
                os.kill(old_pid, 0)
                # Process exists. Don't remove. The flock will handle it.
            except OSError:
                # Process is dead. Remove stale PID file.
                os.remove(PID_FILE)
                print(f"Removed stale PID file (PID {old_pid} is dead)", file=sys.stderr)
        except (ValueError, OSError):
            os.remove(PID_FILE)


def pre_start():
    """Run before daemon starts. Ensures clean state."""
    print("hex-events pre-start check", file=sys.stderr)
    _kill_stale_hex_eventd()
    _clean_lock_files()
    _clean_db_files()
    print("Pre-start check complete", file=sys.stderr)


def check():
    """Check if daemon is healthy. Exit 0 = healthy, 1 = unhealthy."""
    if not os.path.exists(HEALTH_FILE):
        print("No health file found. Daemon may not be running.")
        return 1

    try:
        with open(HEALTH_FILE) as f:
            data = json.load(f)
    except (json.JSONDecodeError, OSError) as e:
        print(f"Cannot read health file: {e}")
        return 1

    state = data.get("state", "unknown")
    secs_since = data.get("seconds_since_success", float("inf"))
    db_locks = data.get("consecutive_db_lock_errors", 0)
    pid = data.get("pid", 0)
    processing_stalled = data.get("processing_stalled", False)
    last_event_processed = data.get("last_event_processed")
    unprocessed_count = data.get("unprocessed_count", -1)

    # Check if the PID is still alive
    if pid:
        try:
            os.kill(pid, 0)
        except OSError:
            print(f"Daemon PID {pid} is dead.")
            return 1

    # Check for stuck state
    if secs_since > STUCK_THRESHOLD_SECONDS:
        print(
            f"UNHEALTHY: no successful cycle in {secs_since:.0f}s "
            f"(threshold: {STUCK_THRESHOLD_SECONDS}s). "
            f"State: {state}, DB lock errors: {db_locks}"
        )
        return 1

    if state == "degraded":
        print(f"DEGRADED: {db_locks} consecutive DB lock errors. Recovery in progress.")
        return 1

    if processing_stalled:
        unprocessed_str = f", {unprocessed_count} unprocessed" if unprocessed_count >= 0 else ""
        last_str = f", last processed: {last_event_processed}" if last_event_processed else ""
        print(f"STALLED: daemon alive but not processing events{unprocessed_str}{last_str}")
        return 1

    print(
        f"HEALTHY: state={state}, last_success={secs_since:.0f}s ago, "
        f"events={data.get('events_processed_total', 0)}"
    )
    return 0


def watchdog():
    """Run as a watchdog. Checks health every 5 minutes, restarts if stuck.

    Designed to run as a separate systemd timer or cron job.
    """
    result = check()
    if result != 0:
        print("Watchdog: daemon is unhealthy. Restarting via systemd.", file=sys.stderr)
        subprocess.run(
            ["systemctl", "--user", "restart", "hex-events.service"],
            check=False,
        )


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(1)

    cmd = sys.argv[1]
    if cmd == "--pre-start":
        pre_start()
    elif cmd == "--check":
        sys.exit(check())
    elif cmd == "--watchdog":
        watchdog()
    else:
        print(f"Unknown command: {cmd}")
        print(__doc__)
        sys.exit(1)
