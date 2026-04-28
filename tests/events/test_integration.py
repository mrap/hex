"""Integration test for hex-events v2 (t-9).

Exercises the full pipeline end-to-end:
- Policy loading and matching
- Condition evaluation
- Action execution
- Delayed emit + cancel_group (debounce)
- Dedup key deduplication
- Rate limiting
- Scheduler ticks
- hex-events validate (function-level)
- hex_emit.py subprocess usage
- Backwards compatibility with old recipe format

Design: Uses a DaemonContext helper that wrapps the real daemon functions
(process_event, drain_deferred, SchedulerAdapter) with a temp DB and temp
policies directory — simulating "start daemon with test DB + test policies dir"
and "stop daemon cleanly" without starting a subprocess daemon (which cannot
accept alternate config paths without code changes).
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta

import pytest
import yaml

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, _REPO_ROOT)

from db import EventsDB
from policy import Policy, Rule, load_policies
from recipe import Action, Condition
from conditions import evaluate_conditions
from hex_eventd import process_event, drain_deferred
from validator import build_static_graph, validate_graph, load_adapter_events

HEX_DIR = _REPO_ROOT
HEX_EMIT = os.path.join(HEX_DIR, "hex_emit.py")


# ---------------------------------------------------------------------------
# DaemonContext: simulates a running daemon with isolated DB + policies dir
# ---------------------------------------------------------------------------

class DaemonContext:
    """Simulates the hex-events daemon for one test.

    Usage::

        with DaemonContext() as ctx:
            ctx.add_policy(yaml_text)
            ctx.emit("event.type", {"key": "val"})
            ctx.process_all()       # one daemon poll tick
            ctx.drain_deferred()    # drain deferred events
            ctx.tick_scheduler()    # fire scheduler adapter
    """

    def __init__(self):
        self.tmp_dir = tempfile.mkdtemp(prefix="hex-int-test-")
        db_fd, self.db_path = tempfile.mkstemp(suffix=".db", dir=self.tmp_dir)
        os.close(db_fd)
        self.policies_dir = os.path.join(self.tmp_dir, "policies")
        os.makedirs(self.policies_dir)
        self.db = EventsDB(self.db_path)
        self.policies = []

    def add_policy_file(self, filename: str, content: str):
        """Write a YAML policy file to the test policies dir."""
        path = os.path.join(self.policies_dir, filename)
        with open(path, "w") as f:
            f.write(content)

    def reload_policies(self):
        """Reload policies from the policies dir (like the daemon hot-reload)."""
        self.policies = load_policies(self.policies_dir)
        return self.policies

    def emit(self, event_type: str, payload: dict = None, source: str = "test",
             dedup_key: str = None) -> int | None:
        """Insert an event into the test DB."""
        return self.db.insert_event(
            event_type,
            json.dumps(payload or {}),
            source,
            dedup_key=dedup_key,
        )

    def process_all(self):
        """Process all unprocessed events (one daemon tick).

        Flattens Policy objects to their Rule lists, since process_event uses
        duck-typing that works with Recipe or Rule objects (both have
        matches_event_type, conditions, actions, name attributes).
        """
        rules = []
        for policy in self.policies:
            rules.extend(policy.rules)
        events = self.db.get_unprocessed()
        for event in events:
            process_event(event, rules, self.db)
        return len(events)

    def drain(self):
        """Drain due deferred events into the events table."""
        drain_deferred(self.db)

    def fast_forward_deferred(self, seconds: int = 0):
        """Set all deferred fire_at timestamps to N seconds ago."""
        self.db.conn.execute(
            f"UPDATE deferred_events SET fire_at = datetime('now', '-{seconds} seconds')"
        )
        self.db.conn.commit()

    def count_events(self, event_type: str) -> int:
        """Count events of a given type in the DB."""
        row = self.db.conn.execute(
            "SELECT COUNT(*) as cnt FROM events WHERE event_type = ?",
            (event_type,),
        ).fetchone()
        return row["cnt"]

    def count_deferred(self, event_type: str = None) -> int:
        """Count pending deferred events."""
        if event_type:
            row = self.db.conn.execute(
                "SELECT COUNT(*) as cnt FROM deferred_events WHERE event_type = ?",
                (event_type,),
            ).fetchone()
        else:
            row = self.db.conn.execute(
                "SELECT COUNT(*) as cnt FROM deferred_events"
            ).fetchone()
        return row["cnt"]

    def get_action_logs(self, event_type: str = None) -> list:
        """Return action_log rows optionally filtered by event type."""
        if event_type:
            rows = self.db.conn.execute(
                "SELECT a.* FROM action_log a "
                "JOIN events e ON e.id = a.event_id "
                "WHERE e.event_type = ?",
                (event_type,),
            ).fetchall()
        else:
            rows = self.db.conn.execute("SELECT * FROM action_log").fetchall()
        return [dict(r) for r in rows]

    def stop(self):
        """Clean up — simulates daemon shutdown."""
        self.db.close()
        shutil.rmtree(self.tmp_dir, ignore_errors=True)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.stop()


# ---------------------------------------------------------------------------
# Test policies (minimal, self-contained)
# ---------------------------------------------------------------------------

EMIT_CHAIN_POLICY = """
name: emit-chain
description: A→B→C chain for integration testing
provides:
  events:
    - test.middle
    - test.output
requires:
  events:
    - test.input
    - test.middle
rules:
  - name: step-1
    trigger:
      event: test.input
    actions:
      - type: emit
        event: test.middle

  - name: step-2
    trigger:
      event: test.middle
    actions:
      - type: emit
        event: test.output
"""

RATE_LIMITED_POLICY = """
name: rate-limited
description: Fires at most 2 times per hour
rate_limit:
  max_fires: 2
  window: 1h
provides:
  events:
    - rate.fired
requires:
  events:
    - rate.input
rules:
  - name: fire
    trigger:
      event: rate.input
    actions:
      - type: emit
        event: rate.fired
"""

DEFERRED_POLICY = """
name: deferred-test
description: Arms a deferred check with cancel_group
provides:
  events:
    - deferred.check-due
requires:
  events:
    - deferred.trigger
rules:
  - name: arm-check
    trigger:
      event: deferred.trigger
    actions:
      - type: emit
        event: deferred.check-due
        delay: 10m
        cancel_group: test-check
"""

OLD_RECIPE_YAML = """
name: old-style-recipe
trigger:
  event: old.event
conditions:
  - field: path
    op: contains
    value: important/
actions:
  - type: emit
    event: old.processed
"""


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

def test_emit_subprocess_inserts_event():
    """hex_emit.py subprocess with --db inserts event into the test DB."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    try:
        db = EventsDB(db_path)
        db.close()

        result = subprocess.run(
            [sys.executable, HEX_EMIT, "test.subprocess", '{"msg":"hello"}', "test",
             "--db", db_path],
            capture_output=True, text=True, timeout=10,
        )
        assert result.returncode == 0, f"hex_emit failed: {result.stderr}"

        db = EventsDB(db_path)
        events = db.conn.execute(
            "SELECT * FROM events WHERE event_type = 'test.subprocess'"
        ).fetchall()
        assert len(events) == 1
        assert events[0]["source"] == "test"
        assert json.loads(events[0]["payload"]) == {"msg": "hello"}
        db.close()
    finally:
        if os.path.exists(db_path):
            os.unlink(db_path)


def test_policy_matching_and_event_chaining():
    """Policy matching: test.input → emit test.middle → emit test.output."""
    with DaemonContext() as ctx:
        ctx.add_policy_file("chain.yaml", EMIT_CHAIN_POLICY)
        ctx.reload_policies()

        # Emit test.input
        ctx.emit("test.input", {"x": 1})
        ctx.process_all()   # step-1 fires: emits test.middle

        assert ctx.count_events("test.middle") == 1

        ctx.process_all()   # step-2 fires: emits test.output
        assert ctx.count_events("test.output") == 1


def test_action_log_recorded():
    """Action execution is logged to action_log."""
    with DaemonContext() as ctx:
        ctx.add_policy_file("chain.yaml", EMIT_CHAIN_POLICY)
        ctx.reload_policies()

        ctx.emit("test.input", {})
        ctx.process_all()

        logs = ctx.get_action_logs("test.input")
        assert len(logs) >= 1
        assert any(log["action_type"] == "emit" for log in logs)
        assert all(log["status"] == "success" for log in logs)


def test_delayed_emit_creates_deferred_event():
    """Delayed emit inserts into deferred_events table, not events table."""
    with DaemonContext() as ctx:
        ctx.add_policy_file("deferred.yaml", DEFERRED_POLICY)
        ctx.reload_policies()

        ctx.emit("deferred.trigger", {})
        ctx.process_all()

        # Deferred event created, not yet in events
        assert ctx.count_deferred("deferred.check-due") == 1
        assert ctx.count_events("deferred.check-due") == 0


def test_cancel_group_debounces_rapid_triggers():
    """cancel_group ensures only one pending deferred event per group."""
    with DaemonContext() as ctx:
        ctx.add_policy_file("deferred.yaml", DEFERRED_POLICY)
        ctx.reload_policies()

        # Emit 3 rapid triggers
        for i in range(3):
            ctx.emit("deferred.trigger", {"seq": i})
        ctx.process_all()

        # cancel_group should collapse to 1 pending deferred
        assert ctx.count_deferred("deferred.check-due") == 1


def test_drain_deferred_promotes_due_events():
    """drain_deferred fires due deferred events into the events table."""
    with DaemonContext() as ctx:
        ctx.add_policy_file("deferred.yaml", DEFERRED_POLICY)
        ctx.reload_policies()

        ctx.emit("deferred.trigger", {})
        ctx.process_all()

        assert ctx.count_deferred("deferred.check-due") == 1

        # Fast-forward: make the deferred event due now
        ctx.fast_forward_deferred(seconds=1)
        ctx.drain()

        # Now in events table, not in deferred
        assert ctx.count_deferred("deferred.check-due") == 0
        assert ctx.count_events("deferred.check-due") == 1


def test_dedup_key_prevents_double_insert():
    """Same dedup_key (after processing) → second insert is dropped."""
    with DaemonContext() as ctx:
        eid1 = ctx.emit("test.dedup", {"n": 1}, dedup_key="key:2026")
        assert eid1 is not None

        # Mark processed so dedup logic kicks in
        ctx.db.mark_processed(eid1)

        eid2 = ctx.emit("test.dedup", {"n": 2}, dedup_key="key:2026")
        assert eid2 is None, "Duplicate dedup_key should be rejected after processing"

        total = ctx.count_events("test.dedup")
        assert total == 1


def test_dedup_key_allows_before_processing():
    """Same dedup_key that hasn't been processed yet is NOT blocked."""
    with DaemonContext() as ctx:
        eid1 = ctx.emit("test.dedup2", {"n": 1}, dedup_key="unprocessed-key")
        assert eid1 is not None

        # Second insert with same key — not yet processed
        eid2 = ctx.emit("test.dedup2", {"n": 2}, dedup_key="unprocessed-key")
        # insert_event only dedupes on processed rows, so this is allowed
        assert eid2 is not None


def test_rate_limit_allows_then_blocks():
    """Policy rate_limit: check_rate_limit enforces max_fires per window."""
    from policy import check_rate_limit, record_fire, load_policies
    import tempfile, os

    tmp = tempfile.mkdtemp()
    try:
        pdir = os.path.join(tmp, "policies")
        os.makedirs(pdir)
        with open(os.path.join(pdir, "rate.yaml"), "w") as f:
            f.write(RATE_LIMITED_POLICY)

        policies = load_policies(pdir)
        assert len(policies) == 1
        policy = policies[0]
        assert policy.name == "rate-limited"
        assert policy.rate_limit["max_fires"] == 2

        # Initially allowed
        assert check_rate_limit(policy)

        # Record 2 fires → hit the limit
        record_fire(policy)
        record_fire(policy)
        assert not check_rate_limit(policy), "Should be rate-limited after 2 fires/1h"

        # Clear fires → allowed again
        policy.last_fires.clear()
        assert check_rate_limit(policy), "Should be allowed with no recent fires"
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_scheduler_adapter_tick_emits_timer_event():
    """SchedulerAdapter.tick() emits timer events with dedup_key."""
    try:
        import croniter  # noqa: F401
    except ImportError:
        pytest.skip("croniter not installed — scheduler tests skipped")
    from adapters.scheduler import SchedulerAdapter

    # Create a temp scheduler config with a cron that's always in the past
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    fd2, sched_path = tempfile.mkstemp(suffix=".yaml")
    os.close(fd2)
    try:
        db = EventsDB(db_path)

        sched_config = {
            "schedules": [
                {"name": "test-tick", "cron": "* * * * *", "event": "timer.tick.1m"},
            ]
        }
        with open(sched_path, "w") as f:
            yaml.dump(sched_config, f)

        adapter = SchedulerAdapter(config_path=sched_path)

        # tick() should emit the timer event
        now = datetime.utcnow()
        emitted = adapter.tick(db, now=now)
        assert "timer.tick.1m" in emitted

        # Verify dedup_key is set
        row = db.conn.execute(
            "SELECT dedup_key FROM events WHERE event_type = 'timer.tick.1m'"
        ).fetchone()
        assert row is not None
        assert "timer.tick.1m:" in row["dedup_key"]

        # tick() again at the same minute — dedup prevents double-emit
        emitted2 = adapter.tick(db, now=now)
        assert "timer.tick.1m" not in emitted2

        count = db.conn.execute(
            "SELECT COUNT(*) as c FROM events WHERE event_type = 'timer.tick.1m'"
        ).fetchone()["c"]
        assert count == 1

        db.close()
    finally:
        if os.path.exists(db_path):
            os.unlink(db_path)
        if os.path.exists(sched_path):
            os.unlink(sched_path)


def test_scheduler_startup_catchup():
    """SchedulerAdapter.startup_catchup() emits one catch-up tick per missed schedule."""
    try:
        import croniter  # noqa: F401
    except ImportError:
        pytest.skip("croniter not installed — scheduler tests skipped")
    from adapters.scheduler import SchedulerAdapter

    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    fd2, sched_path = tempfile.mkstemp(suffix=".yaml")
    os.close(fd2)
    try:
        db = EventsDB(db_path)
        sched_config = {
            "schedules": [
                {"name": "hourly", "cron": "0 * * * *", "event": "timer.tick.1h"},
            ]
        }
        with open(sched_path, "w") as f:
            yaml.dump(sched_config, f)

        adapter = SchedulerAdapter(config_path=sched_path)

        # Startup catchup — emits one catch-up event
        now = datetime.utcnow()
        caught_up = adapter.startup_catchup(db, now=now)
        assert "timer.tick.1h" in caught_up

        # Second catchup at same time — dedup prevents double emit
        caught_up2 = adapter.startup_catchup(db, now=now)
        assert "timer.tick.1h" not in caught_up2

        db.close()
    finally:
        if os.path.exists(db_path):
            os.unlink(db_path)
        if os.path.exists(sched_path):
            os.unlink(sched_path)


def test_validate_passes_complete_graph():
    """Validate returns valid for a self-contained policy graph."""
    # Create two policies: A provides test.mid, B requires test.mid + provides test.out
    policies = [
        Policy(
            name="policy-a",
            rules=[Rule(name="r", trigger_event="external.input",
                        actions=[Action(type="emit", params={"event": "test.mid"})])],
            provides={"events": ["test.mid"]},
            requires={"events": ["external.input"]},
        ),
        Policy(
            name="policy-b",
            rules=[Rule(name="r", trigger_event="test.mid",
                        actions=[Action(type="emit", params={"event": "test.out"})])],
            provides={"events": ["test.out"]},
            requires={"events": ["test.mid"]},
        ),
    ]
    # Mark external.input as provided by adapter
    adapter_events = {"external.input"}
    graph = build_static_graph(policies, adapter_events=adapter_events)
    result = validate_graph(graph)

    assert result["valid"], (
        f"Expected valid graph but got: unsatisfied={result['unsatisfied']}, "
        f"cycles={result['cycles']}"
    )
    assert result["unsatisfied"] == []
    assert result["cycles"] == []


def test_validate_detects_unsatisfied_requires():
    """Validate reports unsatisfied when a required event has no provider."""
    policies = [
        Policy(
            name="orphan-consumer",
            rules=[Rule(name="r", trigger_event="ghost.event")],
            provides={},
            requires={"events": ["ghost.event"]},
        ),
    ]
    graph = build_static_graph(policies, adapter_events=set())
    result = validate_graph(graph)

    assert not result["valid"]
    assert any(u["event"] == "ghost.event" for u in result["unsatisfied"])


def test_backwards_compatible_old_recipe_format():
    """Old recipe YAML (trigger + actions) auto-wraps into a single-rule Policy."""
    with DaemonContext() as ctx:
        ctx.add_policy_file("old-recipe.yaml", OLD_RECIPE_YAML)
        policies = ctx.reload_policies()

        assert len(policies) == 1
        policy = policies[0]
        assert policy.name == "old-style-recipe"
        assert len(policy.rules) == 1
        rule = policy.rules[0]
        assert rule.trigger_event == "old.event"
        assert len(rule.conditions) == 1
        assert rule.conditions[0].field == "path"
        assert len(rule.actions) == 1
        assert rule.actions[0].type == "emit"


def test_condition_evaluation_with_count():
    """count(event_type, window) condition works in the integrated pipeline."""
    with DaemonContext() as ctx:
        ctx.add_policy_file("chain.yaml", EMIT_CHAIN_POLICY)
        ctx.reload_policies()

        # Directly test condition evaluation against the DB
        cond = Condition(field="count(test.input, 10m)", op="eq", value=0)
        assert evaluate_conditions([cond], {}, db=ctx.db) is True

        # Emit an event and verify count condition detects it
        ctx.emit("test.input", {})
        assert evaluate_conditions([cond], {}, db=ctx.db) is False

        cond_gte = Condition(field="count(test.input, 10m)", op="gte", value=1)
        assert evaluate_conditions([cond_gte], {}, db=ctx.db) is True


def test_full_e2e_pipeline_landings_staleness():
    """Full pipeline: git.commit → deferred check → drain → violation fires.

    Uses the real landings-staleness policy from the repo's policies directory.
    """
    policies_dir = os.path.join(_REPO_ROOT, "policies")
    if not os.path.isdir(policies_dir):
        pytest.skip("No policies directory found")

    policies = load_policies(policies_dir)
    landings_policy = next(
        (p for p in policies if p.name == "landings-staleness"), None
    )
    if landings_policy is None:
        pytest.skip("landings-staleness policy not found")

    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    try:
        db = EventsDB(db_path)

        # Use policy.rules as the "recipe" list (duck-type compatible)
        rules = landings_policy.rules

        # Step 1: git.commit → arm deferred check
        db.insert_event("git.commit", json.dumps({"sha": "test1"}), "test")
        for ev in db.get_unprocessed():
            process_event(ev, rules, db)

        deferred = db.conn.execute("SELECT * FROM deferred_events").fetchall()
        assert len(deferred) == 1
        assert deferred[0]["event_type"] == "landings.check-due"
        assert deferred[0]["cancel_group"] == "landings-check"

        # Step 2: Fast-forward deferred time
        db.conn.execute("UPDATE deferred_events SET fire_at = datetime('now', '-1 second')")
        db.conn.commit()

        # Step 3: Drain deferred
        drain_deferred(db)
        assert db.conn.execute(
            "SELECT COUNT(*) as c FROM events WHERE event_type = 'landings.check-due'"
        ).fetchone()["c"] == 1

        # Step 4: Process the check-due event (no landings.updated → violation)
        pending = [e for e in db.get_unprocessed()
                   if e["event_type"] == "landings.check-due"]
        assert len(pending) == 1
        process_event(pending[0], rules, db)

        violations = db.conn.execute(
            "SELECT id FROM events WHERE event_type = 'policy.violation'"
        ).fetchall()
        assert len(violations) == 1

        db.close()
    finally:
        if os.path.exists(db_path):
            os.unlink(db_path)


def test_no_violation_when_landings_updated():
    """No violation emitted when landings.updated is within the 10m window."""
    policies_dir = os.path.join(_REPO_ROOT, "policies")
    if not os.path.isdir(policies_dir):
        pytest.skip("No policies directory found")

    policies = load_policies(policies_dir)
    landings_policy = next(
        (p for p in policies if p.name == "landings-staleness"), None
    )
    if landings_policy is None:
        pytest.skip("landings-staleness policy not found")

    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    try:
        db = EventsDB(db_path)
        rules = landings_policy.rules

        # Insert a recent landings.updated (2 minutes ago, inside 10m window)
        recent_ts = (datetime.utcnow() - timedelta(minutes=2)).strftime("%Y-%m-%d %H:%M:%S")
        db.conn.execute(
            "INSERT INTO events (event_type, payload, source, created_at, processed_at) "
            "VALUES (?, ?, ?, ?, datetime('now'))",
            ("landings.updated", json.dumps({"path": "landings/main.md"}), "test", recent_ts),
        )
        db.conn.commit()

        # Emit landings.check-due and process it
        db.insert_event("landings.check-due", json.dumps({}), "test")
        pending = [e for e in db.get_unprocessed()
                   if e["event_type"] == "landings.check-due"]
        process_event(pending[0], rules, db)

        # No violation: landings.updated was within 10 minutes
        violations = db.conn.execute(
            "SELECT id FROM events WHERE event_type = 'policy.violation'"
        ).fetchall()
        assert len(violations) == 0

        db.close()
    finally:
        if os.path.exists(db_path):
            os.unlink(db_path)


def test_policy_loaded_and_fires_in_daemon():
    """Daemon hot-reload loads new-format policy from policies dir and fires it.

    Simulates the daemon loading a policies/ dir, then processing an event
    through _process_event_policies (policy mode with rate limiting).
    """
    from hex_eventd import _process_event_policies

    with DaemonContext() as ctx:
        ctx.add_policy_file("fire.yaml", EMIT_CHAIN_POLICY)
        ctx.reload_policies()

        assert len(ctx.policies) == 1
        policy = ctx.policies[0]
        assert policy.name == "emit-chain"
        assert len(policy.rules) == 2

        # Emit test.input and process through policies mode
        ctx.emit("test.input", {"x": 99})
        events = ctx.db.get_unprocessed()
        assert len(events) == 1

        _process_event_policies(events[0], ctx.policies, ctx.db)

        # step-1 should have fired, emitting test.middle
        assert ctx.count_events("test.middle") == 1

        # Rate limit fire recorded
        assert len(policy.last_fires) == 1


def test_policy_rate_limit_enforced_in_daemon():
    """_process_event_policies respects per-policy rate limits."""
    from hex_eventd import _process_event_policies

    with DaemonContext() as ctx:
        ctx.add_policy_file("rate.yaml", RATE_LIMITED_POLICY)
        ctx.reload_policies()

        policy = ctx.policies[0]

        # Pre-fill rate limit fires to exhaust the window
        from policy import record_fire
        record_fire(policy)
        record_fire(policy)  # max_fires=2 hit

        ctx.emit("rate.input", {})
        events = ctx.db.get_unprocessed()
        _process_event_policies(events[0], ctx.policies, ctx.db)

        # Rate limited: rate.fired should NOT have been emitted
        assert ctx.count_events("rate.fired") == 0


def test_daemon_context_lifecycle():
    """DaemonContext starts clean, processes events, and cleans up."""
    with DaemonContext() as ctx:
        ctx.add_policy_file("chain.yaml", EMIT_CHAIN_POLICY)
        ctx.reload_policies()

        assert ctx.count_events("test.input") == 0

        ctx.emit("test.input", {"n": 42})
        assert ctx.count_events("test.input") == 1

        n_processed = ctx.process_all()
        assert n_processed >= 1

        # test.middle emitted by the chain
        assert ctx.count_events("test.middle") == 1

        tmp_dir = ctx.tmp_dir

    # After context exit, tmp_dir cleaned up
    assert not os.path.exists(tmp_dir), "DaemonContext should clean up temp dir on exit"
