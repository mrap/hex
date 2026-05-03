#!/usr/bin/env python3
"""hex-eventd — persistent daemon for hex-events."""
import fcntl
import json
import logging
import logging.handlers
import os
import signal
import subprocess
import sqlite3
import sys
import time
import warnings
from contextlib import contextmanager
from datetime import datetime
from pathlib import Path

import yaml

# Add parent to path
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from db import EventsDB, parse_duration
from recipe import Recipe
from policy import load_policies, check_rate_limit, record_fire
from conditions import evaluate_conditions, evaluate_conditions_with_details
from actions import get_action_handler
from adapters.scheduler import SchedulerAdapter
from policy_validator import validate_policy

os.environ.setdefault("HEX_ROOT", str(Path.home() / "hex"))

BASE_DIR = os.path.expanduser("~/.hex-events")
DB_PATH = os.path.join(BASE_DIR, "events.db")
PID_FILE = os.path.join(BASE_DIR, "hex_eventd.pid")
LOCK_FILE = os.path.join(BASE_DIR, "hex_eventd.lock")
LOG_FILE = os.path.join(BASE_DIR, "daemon.log")
HEALTH_FILE = os.path.join(BASE_DIR, "health.json")
POLICIES_DIR = os.path.join(BASE_DIR, "policies")
SCHEDULER_CONFIG = os.path.join(BASE_DIR, "adapters", "scheduler.yaml")
POLL_INTERVAL = 2  # seconds
JANITOR_INTERVAL = 3600  # run janitor every hour
SCHEDULER_RELOAD_INTERVAL = 60  # reload scheduler config every minute
HEARTBEAT_INTERVAL = 300  # heartbeat log every 5 minutes

# Database lock recovery thresholds
DB_LOCK_CONSECUTIVE_THRESHOLD = 10   # consecutive lock errors before recovery attempt
DB_LOCK_RECOVERY_BACKOFF = 30       # seconds to wait after recovery attempt
DB_LOCK_MAX_RECOVERY_ATTEMPTS = 3   # max recovery attempts before giving up and restarting

# Stall detection: warn if no events processed in this many seconds while events are pending
STALL_THRESHOLD_SECONDS = 300  # 5 minutes

log = logging.getLogger("hex-events")

# Module-level policy reload cache: {filepath: mtime} and the cached list
_policy_mtimes: dict = {}
_policy_list_cache: list = []


# ---------------------------------------------------------------------------
# Singleton and startup safety
# ---------------------------------------------------------------------------

def _kill_competing_hex_eventd_processes(my_pid: int):
    """Find and kill any other hex_eventd.py processes.

    Uses /proc to find processes without shelling out to find/ps with broad
    globs. Only kills processes matching our exact script name.
    """
    killed = []
    script_name = "hex_eventd.py"
    try:
        for entry in os.listdir("/proc"):
            if not entry.isdigit():
                continue
            pid = int(entry)
            if pid == my_pid:
                continue
            try:
                cmdline_path = f"/proc/{pid}/cmdline"
                with open(cmdline_path, "rb") as f:
                    cmdline = f.read().decode("utf-8", errors="replace")
                # cmdline uses null bytes as separators
                if script_name in cmdline:
                    os.kill(pid, signal.SIGTERM)
                    killed.append(pid)
            except (OSError, PermissionError):
                continue
    except OSError:
        pass

    if killed:
        # Give them a moment to exit cleanly
        time.sleep(1)
        for pid in killed:
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass

    return killed


def _clean_stale_db_files():
    """Remove stale WAL and SHM files if no process holds the database open.

    After killing competing processes, leftover -wal and -shm files can cause
    lock issues. We verify no one holds the DB by attempting an exclusive lock
    on the DB file itself, then remove the journal files.
    """
    wal_path = DB_PATH + "-wal"
    shm_path = DB_PATH + "-shm"

    stale_files = [p for p in [wal_path, shm_path] if os.path.exists(p)]
    if not stale_files:
        return

    # Try to open the DB exclusively to verify it's free
    try:
        test_conn = sqlite3.connect(DB_PATH, timeout=2)
        # Force a checkpoint to flush WAL contents into the main DB
        test_conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        test_conn.close()
        log.info("Startup: checkpointed WAL successfully")
    except sqlite3.OperationalError as e:
        # Can't checkpoint. Remove stale files if they exist.
        log.warning("Startup: could not checkpoint WAL (%s), removing stale files", e)
        for path in stale_files:
            try:
                os.remove(path)
                log.info("Startup: removed stale file %s", path)
            except OSError:
                pass


def _verify_db_writable() -> bool:
    """Verify the database is writable before entering the event loop.

    Opens a connection, writes a test row, deletes it. Returns True on success.
    """
    try:
        conn = sqlite3.connect(DB_PATH, timeout=5)
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA busy_timeout=5000")
        conn.execute(
            "CREATE TABLE IF NOT EXISTS _health_check "
            "(id INTEGER PRIMARY KEY, ts TEXT)"
        )
        conn.execute(
            "INSERT OR REPLACE INTO _health_check (id, ts) VALUES (1, ?)",
            (datetime.utcnow().isoformat(),),
        )
        conn.commit()
        conn.close()
        return True
    except sqlite3.OperationalError as e:
        log.error("Startup: DB not writable: %s", e)
        return False


def _acquire_singleton_lock():
    """Acquire an exclusive lock to prevent multiple daemon instances.

    Uses a dedicated .lock file (not the PID file) with fcntl.flock().
    The lock auto-releases on process death. No stale lock file issues.
    Writes PID to the PID file separately for diagnostics.
    Exits with code 0 if another instance already holds the lock.
    """
    try:
        os.makedirs(BASE_DIR, exist_ok=True)
    except FileExistsError:
        print(
            f"hex-eventd: {BASE_DIR} exists but is not a directory; cannot start",
            file=sys.stderr,
        )
        sys.exit(1)

    def _try_acquire(retry=True):
        lock_fh = open(LOCK_FILE, "w")
        try:
            fcntl.flock(lock_fh, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return lock_fh
        except OSError:
            lock_fh.close()
            if retry:
                # If the holding process is stopped (T state), it will never release
                # the lock on its own — kill it and retry once.
                try:
                    pid = int(open(PID_FILE).read().strip())
                    state = subprocess.check_output(
                        ["ps", "-p", str(pid), "-o", "state="], text=True
                    ).strip()
                    if state.startswith("T"):
                        os.kill(pid, signal.SIGKILL)
                        time.sleep(0.5)
                        for stale in (LOCK_FILE, PID_FILE):
                            try:
                                os.unlink(stale)
                            except OSError:
                                pass
                        return _try_acquire(retry=False)
                except Exception:
                    pass
            print("hex-eventd: another instance is already running, exiting", file=sys.stderr)
            sys.exit(0)

    lock_fh = _try_acquire()
    lock_fh.write(str(os.getpid()))
    lock_fh.flush()

    # Also write PID file for diagnostic tools
    with open(PID_FILE, "w") as f:
        f.write(str(os.getpid()))

    return lock_fh


# ---------------------------------------------------------------------------
# Health monitoring
# ---------------------------------------------------------------------------

class HealthMonitor:
    """Tracks daemon health and writes status to a JSON file for external checks."""

    def __init__(self):
        self.consecutive_db_lock_errors = 0
        self.total_db_lock_errors = 0
        self.last_successful_cycle = time.time()
        self.last_event_processed = 0.0
        self.daemon_start = time.time()
        self.recovery_attempts = 0
        self.events_processed_total = 0
        self.actions_fired_total = 0
        self.state = "starting"  # starting, healthy, degraded, recovering
        self.processing_stalled = False

    def record_success(self, events_count: int = 0, actions_count: int = 0):
        """Record a successful event loop cycle."""
        self.consecutive_db_lock_errors = 0
        self.last_successful_cycle = time.time()
        self.events_processed_total += events_count
        self.actions_fired_total += actions_count
        if events_count > 0:
            self.last_event_processed = time.time()
        if self.state != "healthy":
            log.info("Health: recovered, state -> healthy")
        self.state = "healthy"

    def record_db_lock_error(self):
        """Record a database lock error. Returns True if recovery should be attempted."""
        self.consecutive_db_lock_errors += 1
        self.total_db_lock_errors += 1

        if self.consecutive_db_lock_errors == 1:
            log.warning("Database lock error detected (1st)")
        elif self.consecutive_db_lock_errors == DB_LOCK_CONSECUTIVE_THRESHOLD:
            log.error(
                "Database lock error threshold reached (%d consecutive). "
                "Attempting recovery.",
                DB_LOCK_CONSECUTIVE_THRESHOLD,
            )
            self.state = "degraded"
            return True
        elif self.consecutive_db_lock_errors % DB_LOCK_CONSECUTIVE_THRESHOLD == 0:
            # Repeated threshold crossings
            self.state = "degraded"
            return True

        return False

    def write_health_file(self, unprocessed_count: int = -1):
        """Write current health status to a JSON file for external monitoring."""
        lep = (
            datetime.utcfromtimestamp(self.last_event_processed).isoformat()
            if self.last_event_processed > 0
            else None
        )
        data = {
            "pid": os.getpid(),
            "state": self.state,
            "timestamp": datetime.utcnow().isoformat(),
            "last_successful_cycle": self.last_successful_cycle,
            "seconds_since_success": time.time() - self.last_successful_cycle,
            "consecutive_db_lock_errors": self.consecutive_db_lock_errors,
            "total_db_lock_errors": self.total_db_lock_errors,
            "recovery_attempts": self.recovery_attempts,
            "events_processed_total": self.events_processed_total,
            "actions_fired_total": self.actions_fired_total,
            "last_event_processed": lep,
            "processing_stalled": self.processing_stalled,
            "unprocessed_count": unprocessed_count,
        }
        tmp = HEALTH_FILE + ".tmp"
        try:
            with open(tmp, "w") as f:
                json.dump(data, f, indent=2)
            os.rename(tmp, HEALTH_FILE)
        except OSError as e:
            log.error("Failed to write health file: %s", e)


# ---------------------------------------------------------------------------
# Database lock recovery
# ---------------------------------------------------------------------------

def _find_db_lock_holder() -> dict | None:
    """Identify which process holds a lock on the events database.

    Checks /proc/*/fd/ for file descriptors pointing to events.db.
    Returns {"pid": int, "cmdline": str} or None.
    """
    my_pid = os.getpid()
    db_realpath = os.path.realpath(DB_PATH)

    try:
        for entry in os.listdir("/proc"):
            if not entry.isdigit():
                continue
            pid = int(entry)
            if pid == my_pid:
                continue
            fd_dir = f"/proc/{pid}/fd"
            try:
                for fd in os.listdir(fd_dir):
                    try:
                        target = os.readlink(f"{fd_dir}/{fd}")
                        if target == db_realpath or target.startswith(db_realpath):
                            # Found it. Read cmdline for diagnostics.
                            with open(f"/proc/{pid}/cmdline", "rb") as f:
                                cmdline = f.read().decode("utf-8", errors="replace")
                                cmdline = cmdline.replace("\x00", " ").strip()
                            return {"pid": pid, "cmdline": cmdline}
                    except OSError:
                        continue
            except OSError:
                continue
    except OSError:
        pass
    return None


def _attempt_db_recovery(db_obj: EventsDB, health: HealthMonitor) -> EventsDB | None:
    """Attempt to recover from a persistent database lock.

    Strategy:
    1. Identify the process holding the lock
    2. If it's another hex_eventd.py, kill it (shouldn't happen with flock, but safety net)
    3. If it's something else (e.g., a Claude session doing hex_emit.py), wait with backoff
    4. Checkpoint WAL to flush any pending writes
    5. Reconnect to the database

    Returns a new EventsDB instance on success, or None on failure.
    """
    health.recovery_attempts += 1
    health.state = "recovering"

    holder = _find_db_lock_holder()
    if holder:
        log.warning(
            "DB lock held by PID %d: %s",
            holder["pid"], holder["cmdline"][:200],
        )

        # If it's another daemon instance, kill it
        if "hex_eventd.py" in holder["cmdline"]:
            log.warning("Killing competing daemon PID %d", holder["pid"])
            try:
                os.kill(holder["pid"], signal.SIGTERM)
                time.sleep(2)
                os.kill(holder["pid"], signal.SIGKILL)
            except OSError:
                pass
        else:
            # It's a transient process (hex_emit.py, hex-events CLI, etc.)
            # Wait for it to finish rather than killing it
            log.info(
                "Lock holder is not a daemon (PID %d). Waiting %ds for it to release.",
                holder["pid"], DB_LOCK_RECOVERY_BACKOFF,
            )
            time.sleep(DB_LOCK_RECOVERY_BACKOFF)
    else:
        log.info("No lock holder found. Stale WAL/SHM likely. Cleaning up.")

    # Close existing connection
    try:
        db_obj.close()
    except Exception:
        pass

    # Clean stale files and checkpoint
    _clean_stale_db_files()

    # Try to reconnect
    try:
        new_db = EventsDB(DB_PATH)
        # Verify writable
        new_db.conn.execute(
            "INSERT OR REPLACE INTO _health_check (id, ts) VALUES (1, ?)",
            (datetime.utcnow().isoformat(),),
        )
        new_db.conn.commit()
        log.info("DB recovery successful. Reconnected.")
        health.state = "healthy"
        health.consecutive_db_lock_errors = 0
        return new_db
    except sqlite3.OperationalError as e:
        log.error("DB recovery failed: %s", e)
        return None


# ---------------------------------------------------------------------------
# Event processing (unchanged logic, extracted for clarity)
# ---------------------------------------------------------------------------

def drain_deferred(db: EventsDB):
    """Drain due deferred events into the main events table.

    Dual-write safety: delete from deferred_events FIRST, then insert into events.
    This means a crash between the two steps loses the event (acceptable: lost > doubled).
    """
    due = db.get_due_deferred()
    for row in due:
        db.delete_deferred(row["id"])
        db.insert_event(row["event_type"], row["payload"], row["source"])


def match_policies(policies: list[Recipe], event_type: str) -> list[Recipe]:
    return [r for r in policies if r.matches_event_type(event_type)]


def run_action_with_retry(action, event_id: int, recipe_name: str, payload: dict,
                          db: EventsDB, handler=None, sleep_fn=None,
                          workflow_context=None):
    """Run an action with exponential backoff retry.

    Retries up to action.params.get('retries', 3) times on failure.
    Backoff: 1s, 2s, 4s, ...

    Args:
        handler: Override the action handler (for testing). If None, looks up via registry.
        sleep_fn: Override time.sleep (for testing).
        workflow_context: Optional dict {"name": ..., "config": {...}} for Jinja2 templates.
    Returns:
        The final result dict from the handler.
    """
    if sleep_fn is None:
        sleep_fn = time.sleep

    max_retries = action.params.get("retries", 3)

    if handler is None:
        handler = get_action_handler(action.type)
    if not handler:
        msg = f"Unknown action type: {action.type}"
        db.log_action(event_id, recipe_name, action.type,
                      json.dumps(action.params), "error", msg)
        return {"status": "error", "output": msg}

    backoff = 1
    for attempt in range(max_retries + 1):
        result = handler.run(action.params, event_payload=payload, db=db,
                             workflow_context=workflow_context)
        status = result.get("status", "error")

        if status != "error":
            action_result_json = json.dumps({"retry_count": attempt})
            db.log_action(event_id, recipe_name, action.type,
                          json.dumps(action.params), status,
                          result.get("output", ""),
                          action_result=action_result_json)
            _dispatch_sub_actions(action.params.get("on_success"), payload,
                                  result, db, workflow_context=workflow_context)
            return result

        # Action failed
        if attempt < max_retries:
            retry_label = f"retry_{attempt + 1}"
            err_detail = (result.get("output") or "")[:500]
            db.log_action(event_id, recipe_name, action.type,
                          json.dumps(action.params), retry_label,
                          f"Retry {attempt + 1}/{max_retries}: {err_detail}")
            sleep_fn(backoff)
            backoff *= 2
        else:
            # Final failure after all retries exhausted
            err_detail = (result.get("output") or "")[:500]
            db.log_action(event_id, recipe_name, action.type,
                          json.dumps(action.params), "error",
                          f"Permanently failed after {max_retries} retries: {err_detail}")
            _dispatch_sub_actions(action.params.get("on_failure"), payload,
                                  result, db, workflow_context=workflow_context)

    return result


def _dispatch_sub_actions(sub_actions, event_payload, action_result, db,
                          workflow_context=None):
    """Dispatch on_success or on_failure sub-actions. Called exactly once."""
    if not sub_actions:
        return
    from actions import get_action_handler
    from actions.render import render_templates

    action_ctx = action_result.get("_action_result", action_result)
    tpl_ctx = {"event": event_payload, "action": action_ctx, "now": datetime.utcnow()}
    if workflow_context:
        tpl_ctx["workflow"] = workflow_context

    for raw in (sub_actions or []):
        atype = raw.get("type")
        handler = get_action_handler(atype)
        if not handler:
            msg = f"[SUB-ACTION ERROR] Unknown sub-action type: {atype!r} (params={raw})"
            log.error(msg)
            print(msg, file=sys.stderr)
            continue
        raw_params = {k: v for k, v in raw.items() if k != "type"}
        try:
            params = render_templates(raw_params, tpl_ctx)
            result = handler.run(params, event_payload=event_payload, db=db,
                                 workflow_context=workflow_context)
            if result and result.get("status") == "error":
                msg = f"[SUB-ACTION ERROR] type={atype!r} failed: {result.get('output', '')}"
                log.error(msg)
                print(msg, file=sys.stderr)
        except Exception as e:
            msg = f"[SUB-ACTION ERROR] type={atype!r} raised: {e}"
            log.error(msg)
            print(msg, file=sys.stderr)


def _check_rule_ttl(rule, policy_name: str, db: EventsDB) -> bool:
    """Return True if the rule is within TTL (or has no TTL). False = expired, skip rule.

    TTL clock starts from the rule's first action_taken=1 entry in policy_eval_log.
    If the rule has never fired, the clock hasn't started and the rule is allowed.
    """
    if not rule.ttl:
        return True
    try:
        ttl_secs = parse_duration(rule.ttl)
    except ValueError as e:
        log.warning("TTL parse error for policy=%s rule=%s ttl=%r: %s — ignoring TTL",
                    policy_name, rule.name, rule.ttl, e)
        return True

    first_fire_str = db.get_rule_first_fire(policy_name, rule.name)
    if not first_fire_str:
        return True  # never fired, TTL clock hasn't started

    from datetime import timezone
    first_fire = datetime.fromisoformat(first_fire_str).replace(tzinfo=timezone.utc)
    age_secs = (datetime.now(timezone.utc) - first_fire).total_seconds()
    if age_secs > ttl_secs:
        log.info(
            "TTL expired: policy=%s rule=%s ttl=%s age=%.0fs — skipping",
            policy_name, rule.name, rule.ttl, age_secs,
        )
        return False
    return True


def _disable_policy_file(path: str):
    """Rewrite a policy YAML file with enabled: false."""
    with open(path) as f:
        data = yaml.safe_load(f)
    data["enabled"] = False
    tmp_path = path + ".tmp"
    with open(tmp_path, "w") as f:
        yaml.dump(data, f, default_flow_style=False)
    os.rename(tmp_path, path)


def _handle_policy_limits(policy, db: EventsDB):
    """Handle max_fires + after_limit cleanup after successful policy fire."""
    max_fires = getattr(policy, "max_fires", None)
    path = policy.source_file

    if max_fires is not None:
        fires_so_far = db.count_policy_fires(policy.name)
        total = fires_so_far + 1  # +1 for the current fire (not yet logged)
        if total >= max_fires:
            if path and os.path.exists(path):
                after_limit = getattr(policy, "after_limit", "disable")
                if after_limit == "delete":
                    os.remove(path)
                    log.info("Policy %s reached max_fires=%d and was auto-deleted",
                             policy.name, max_fires)
                else:
                    _disable_policy_file(path)
                    log.info("Policy %s reached max_fires=%d and was auto-disabled",
                             policy.name, max_fires)


def process_event(event: dict, policies: list[Recipe], db: EventsDB):
    warnings.warn(
        "process_event() is deprecated; the daemon uses _process_event_policies() directly.",
        DeprecationWarning,
        stacklevel=2,
    )
    event_type = event["event_type"]
    event_id = event["id"]
    try:
        payload = json.loads(event["payload"])
    except json.JSONDecodeError as e:
        log.error("Event %s has malformed JSON payload %r: %s", event_id, event["payload"], e)
        db.mark_processed(event_id, None)
        return 0

    matched = match_policies(policies, event_type)
    matched_names = []

    for recipe in matched:
        if not evaluate_conditions(recipe.conditions, payload, db=db):
            continue
        matched_names.append(recipe.name)
        for action in recipe.actions:
            run_action_with_retry(action, event_id, recipe.name, payload, db,
                                  workflow_context=None)

    recipe_column = ",".join(matched_names) if matched_names else None
    db.mark_processed(event_id, recipe_column)

def _make_eval_row(event_id, policy_name, rule_name, now_ts, **overrides) -> dict:
    """Factory for policy eval log rows. Callers pass keyword overrides for non-default fields."""
    row = {
        "event_id": event_id,
        "policy_name": policy_name,
        "rule_name": rule_name,
        "matched": 1,
        "conditions_passed": None,
        "condition_details": None,
        "rate_limited": 0,
        "action_taken": 0,
        "evaluated_at": now_ts,
        "workflow": None,
        "ttl_expired": 0,
    }
    row.update(overrides)
    return row


def _evaluate_rule(rule, policy, event_id, payload, db, now_ts) -> dict:
    """Evaluate a single rule: TTL check then condition evaluation.

    Returns an eval_row dict with action_taken=1 if conditions passed, 0 otherwise.
    """
    if rule.ttl and not _check_rule_ttl(rule, policy.name, db):
        db.log_action(event_id, policy.name, "ttl_expired",
                      json.dumps({"policy": policy.name, "rule": rule.name,
                                  "ttl": rule.ttl}),
                      "suppressed", f"TTL expired for rule {rule.name}")
        return _make_eval_row(event_id, policy.name, rule.name, now_ts,
                              workflow=policy.workflow, ttl_expired=1)

    conditions_passed, cond_details = evaluate_conditions_with_details(
        rule.conditions, payload, db=db
    )
    return _make_eval_row(
        event_id, policy.name, rule.name, now_ts,
        conditions_passed=1 if conditions_passed else 0,
        condition_details=json.dumps(cond_details) if cond_details else None,
        action_taken=1 if conditions_passed else 0,
        workflow=policy.workflow,
    )


def _fire_rule_actions(rule, policy, event_id, payload, db) -> tuple:
    """Dispatch all actions for a rule whose conditions have passed.

    Returns (actions_fired, all_succeeded).
    """
    wf_ctx = None
    if policy.workflow:
        wf_ctx = {"name": policy.workflow, "config": policy.workflow_config}
    actions_fired = 0
    all_succeeded = True
    for action in rule.actions:
        result = run_action_with_retry(action, event_id, rule.name,
                                       payload, db, workflow_context=wf_ctx)
        actions_fired += 1
        if result.get("status") == "error":
            all_succeeded = False
    return actions_fired, all_succeeded


def _process_event_policies(event: dict, policies: list, db: "EventsDB") -> int:
    """Process an event against Policy objects with per-policy rate limiting.

    Iterates each policy, checks its rate limit, then evaluates matching rules.
    Records a fire timestamp per policy only when at least one rule fires.

    Returns the number of actions dispatched.
    """
    event_type = event["event_type"]
    event_id = event["id"]
    try:
        payload = json.loads(event["payload"])
    except json.JSONDecodeError as e:
        log.error("Event %s has malformed JSON payload %r: %s", event_id, event["payload"], e)
        db.mark_processed(event_id, None)
        return 0

    matched_names = []
    eval_rows = []
    now_ts = datetime.utcnow().isoformat()
    actions_dispatched = 0

    for policy in policies:
        matching_rules = [r for r in policy.rules if r.matches_event_type(event_type)]
        if not matching_rules:
            continue
        if not check_rate_limit(policy):
            rl = policy.rate_limit or {}
            max_fires = rl.get("max_fires", 0)
            window_str = str(rl.get("window", "1h"))
            window_secs = parse_duration(window_str)
            cutoff = time.time() - window_secs
            fires_in_window = len([t for t in policy.last_fires if t >= cutoff])
            rule_names = ",".join(r.name for r in matching_rules)
            detail = json.dumps({
                "policy": policy.name,
                "rule": rule_names,
                "fires_in_window": fires_in_window,
                "max_fires": max_fires,
                "window": window_str,
            })
            err_msg = f"Rate limited: {fires_in_window}/{max_fires} fires in {window_str}"
            log.warning("Rate limited: policy %s skipped for event %s (%d/%d fires in %s)",
                        policy.name, event_type, fires_in_window, max_fires, window_str)
            db.log_action(event_id, policy.name, "rate_limited", detail, "suppressed", err_msg)
            for rule in matching_rules:
                eval_rows.append(_make_eval_row(event_id, policy.name, rule.name, now_ts,
                                                rate_limited=1, workflow=policy.workflow))
            continue

        fired = False
        all_actions_succeeded = True
        ttl_expired_any = False
        for rule in matching_rules:
            eval_row = _evaluate_rule(rule, policy, event_id, payload, db, now_ts)
            eval_rows.append(eval_row)
            if eval_row.get("ttl_expired"):
                ttl_expired_any = True
            if eval_row["action_taken"]:
                matched_names.append(rule.name)
                fired = True
                count, ok = _fire_rule_actions(rule, policy, event_id, payload, db)
                actions_dispatched += count
                if not ok:
                    all_actions_succeeded = False
        if fired:
            record_fire(policy)
            if all_actions_succeeded:
                _handle_policy_limits(policy, db)
        if ttl_expired_any and getattr(policy, "after_limit", "disable") == "delete":
            path = policy.source_file
            if path and os.path.exists(path):
                os.remove(path)
                log.info("Policy %s rules expired (TTL) with after_limit=delete — file removed",
                         policy.name)

    if eval_rows:
        try:
            db.log_policy_evals(eval_rows)
        except Exception as e:
            log.error("Failed to log policy evals for event %s: %s", event_id, e)

    recipe_column = ",".join(matched_names) if matched_names else None
    db.mark_processed(event_id, recipe_column)
    return actions_dispatched


def _collect_policy_mtimes(policies_dir: str) -> dict:
    """Return {filepath: mtime} for all YAML files under policies_dir."""
    mtimes = {}
    if not os.path.isdir(policies_dir):
        return mtimes
    for entry in os.listdir(policies_dir):
        entry_path = os.path.join(policies_dir, entry)
        if os.path.isfile(entry_path) and entry.endswith((".yaml", ".yml")):
            mtimes[entry_path] = os.path.getmtime(entry_path)
        elif os.path.isdir(entry_path):
            for fname in os.listdir(entry_path):
                fpath = os.path.join(entry_path, fname)
                if os.path.isfile(fpath) and fname.endswith((".yaml", ".yml")):
                    mtimes[fpath] = os.path.getmtime(fpath)
    return mtimes


def _load_policies_validated(policies_dir: str) -> list:
    """Load policies from directory, validate each, skip invalid ones.

    Uses mtime-based caching: if no YAML files have changed since the last
    load, returns the cached policy list immediately (avoids N file reads and
    YAML parses on every tick).

    Logs errors to both daemon.log and stderr. Prints a startup summary.
    """
    global _policy_mtimes, _policy_list_cache

    current_mtimes = _collect_policy_mtimes(policies_dir)
    if current_mtimes == _policy_mtimes:
        return _policy_list_cache

    skipped = [0]

    def on_invalid(fpath, errors):
        for err in errors:
            msg = f"[POLICY VALIDATION ERROR] {err}"
            log.error(msg)
            print(msg, file=sys.stderr)
        skipped[0] += 1

    policies = load_policies(policies_dir, on_invalid=on_invalid)
    n_valid = len(policies)
    n_skipped = skipped[0]
    summary = f"Loaded {n_valid} policies ({n_skipped} skipped due to validation errors)"
    log.info(summary)
    if n_skipped > 0:
        print(summary, file=sys.stderr)

    _policy_mtimes = current_mtimes
    _policy_list_cache = policies
    return policies


class _DatabaseBusyError(Exception):
    """Raised by _db_op when sqlite3 reports 'database is locked'."""


@contextmanager
def _db_op(label: str):
    """Context manager that converts 'database is locked' errors into _DatabaseBusyError."""
    try:
        yield
    except sqlite3.OperationalError as e:
        if "database is locked" in str(e):
            raise _DatabaseBusyError() from e
        raise


def _setup_logging():
    """Configure logging to write directly to daemon.log via RotatingFileHandler.

    This works regardless of how the daemon is launched (launchd or manual).
    """
    os.makedirs(BASE_DIR, exist_ok=True)
    fmt = logging.Formatter("%(asctime)s %(name)s %(levelname)s %(message)s")
    fh = logging.handlers.RotatingFileHandler(
        LOG_FILE, maxBytes=5 * 1024 * 1024, backupCount=3
    )
    fh.setFormatter(fmt)
    root = logging.getLogger()
    root.setLevel(logging.INFO)
    root.addHandler(fh)


# ---------------------------------------------------------------------------
# Main daemon loop
# ---------------------------------------------------------------------------

def run_daemon():
    # Phase 1: Acquire singleton lock (exits if another instance running)
    lock_fh = _acquire_singleton_lock()
    _setup_logging()
    my_pid = os.getpid()
    log.info("hex-eventd starting (pid=%d)", my_pid)

    # Phase 2: Kill any competing hex_eventd.py processes (belt + suspenders)
    killed = _kill_competing_hex_eventd_processes(my_pid)
    if killed:
        log.warning("Startup: killed competing daemon PIDs: %s", killed)
        time.sleep(1)

    # Phase 3: Clean stale WAL/SHM files
    _clean_stale_db_files()

    # Phase 4: Verify DB is writable
    if not _verify_db_writable():
        log.error("Startup: DB not writable after cleanup. Exiting for systemd restart.")
        lock_fh.close()
        sys.exit(1)

    # Phase 5: Initialize
    health = HealthMonitor()
    db = EventsDB(DB_PATH)

    scheduler = SchedulerAdapter(config_path=SCHEDULER_CONFIG)
    try:
        caught_up = scheduler.startup_catchup(db)
        if caught_up:
            log.info("Scheduler catchup: emitted %s", caught_up)
    except Exception as e:
        log.error("Scheduler startup catchup failed: %s", e)

    running = True
    def handle_signal(signum, frame):
        nonlocal running
        log.info("Received signal %d, shutting down", signum)
        running = False

    signal.signal(signal.SIGTERM, handle_signal)
    signal.signal(signal.SIGINT, handle_signal)

    last_janitor = 0
    last_recipe_load = 0
    last_scheduler_reload = 0
    last_heartbeat = time.time()
    last_health_write = 0
    _hb_events = 0
    _hb_actions = 0
    _policies = []
    cycle_had_db_error = False
    log.info("hex-eventd ready (pid=%d)", my_pid)
    health.state = "healthy"

    while running:
        now = time.time()
        cycle_had_db_error = False

        # Reload policies every 10 seconds (hot-reload), with validation.
        if now - last_recipe_load > 10:
            _policies = _load_policies_validated(POLICIES_DIR)
            last_recipe_load = now

        # Reload scheduler config periodically
        if now - last_scheduler_reload > SCHEDULER_RELOAD_INTERVAL:
            scheduler.reload()
            last_scheduler_reload = now

        # Tick the scheduler -- emits timer events for due cron windows
        try:
            with _db_op("scheduler tick"):
                scheduler.tick(db, now=datetime.utcnow())
        except _DatabaseBusyError:
            cycle_had_db_error = True
        except Exception as e:
            log.error("Scheduler tick error: %s", e)

        # Drain deferred events whose fire_at has passed
        if not cycle_had_db_error:
            try:
                with _db_op("drain deferred"):
                    drain_deferred(db)
            except _DatabaseBusyError:
                cycle_had_db_error = True
            except Exception as e:
                log.error("Error draining deferred events: %s", e)

        # Process unprocessed events
        cycle_events = 0
        cycle_actions = 0
        if not cycle_had_db_error:
            try:
                with _db_op("process events"):
                    events = db.get_unprocessed()
                    cycle_events = len(events)
                    for event in events:
                        actions = _process_event_policies(event, _policies, db)
                        if actions:
                            cycle_actions += actions
            except _DatabaseBusyError:
                cycle_had_db_error = True
            except Exception as e:
                log.error("Error processing events: %s", e)

        # Health tracking
        if cycle_had_db_error:
            should_recover = health.record_db_lock_error()
            if should_recover:
                if health.recovery_attempts >= DB_LOCK_MAX_RECOVERY_ATTEMPTS:
                    log.error(
                        "DB lock recovery failed %d times. Exiting for systemd restart.",
                        health.recovery_attempts,
                    )
                    health.write_health_file()
                    db.close()
                    lock_fh.close()
                    sys.exit(1)

                new_db = _attempt_db_recovery(db, health)
                if new_db:
                    db = new_db
                else:
                    # Recovery failed. Back off before retrying.
                    log.warning(
                        "DB recovery attempt %d failed. Backing off %ds.",
                        health.recovery_attempts, DB_LOCK_RECOVERY_BACKOFF,
                    )
                    time.sleep(DB_LOCK_RECOVERY_BACKOFF)
        else:
            _hb_events += cycle_events
            _hb_actions += cycle_actions
            health.record_success(cycle_events, cycle_actions)

        # Heartbeat (at INFO level so it always shows in logs)
        if now - last_heartbeat >= HEARTBEAT_INTERVAL:
            log.info(
                "heartbeat: pid=%d state=%s events=%d actions=%d db_locks=%d",
                my_pid,
                health.state,
                _hb_events,
                _hb_actions,
                health.consecutive_db_lock_errors,
            )
            _hb_events = 0
            _hb_actions = 0
            last_heartbeat = now

        # Write health file every 60 seconds; check for processing stall
        if now - last_health_write >= 60:
            try:
                unprocessed_count = db.count_unprocessed()
            except Exception:
                unprocessed_count = -1
            ref_time = health.last_event_processed if health.last_event_processed > 0 else health.daemon_start
            health.processing_stalled = (
                unprocessed_count > 0
                and (now - ref_time) > STALL_THRESHOLD_SECONDS
            )
            if health.processing_stalled:
                msg = (
                    f"[PROCESSING STALL] {unprocessed_count} unprocessed events, "
                    f"last processed {now - ref_time:.0f}s ago"
                )
                log.error(msg)
                print(msg, file=sys.stderr)
                try:
                    db.insert_event(
                        "hex.eventd.processing.stalled",
                        json.dumps({"unprocessed_count": unprocessed_count,
                                    "stalled_seconds": int(now - ref_time)}),
                        "hex_eventd",
                    )
                except Exception:
                    pass
            health.write_health_file(unprocessed_count=unprocessed_count)
            last_health_write = now

        # Janitor
        if now - last_janitor > JANITOR_INTERVAL:
            try:
                deleted = db.janitor(days=7, vacuum=True)
                if deleted > 0:
                    log.info("Janitor: deleted %d old events (+ orphan logs, vacuumed)", deleted)
            except Exception as e:
                log.error("Janitor error: %s", e)
            last_janitor = now

        time.sleep(POLL_INTERVAL)

    db.close()
    lock_fh.close()
    # Clean up PID and health files
    for f in [PID_FILE, HEALTH_FILE]:
        try:
            os.remove(f)
        except OSError:
            pass
    log.info("hex-eventd stopped")

if __name__ == "__main__":
    run_daemon()
