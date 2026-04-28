"""Scheduler adapter for hex-events — emits timer events on cron schedules.

Reads adapters/scheduler.yaml, evaluates cron expressions via croniter, and
emits timer events into the event bus with a dedup_key so multiple daemon ticks
within the same cron window don't double-fire.

On startup, `startup_catchup()` emits at most ONE catch-up tick per schedule
(emit_latest_only) for any tick that was missed while the daemon was down.
"""
import json
import logging
import os
from datetime import datetime

import yaml

log = logging.getLogger("hex-events")

BASE_DIR = os.path.expanduser("~/.hex-events")
DEFAULT_CONFIG_PATH = os.path.join(BASE_DIR, "adapters", "scheduler.yaml")


def _iso_minute(dt: datetime) -> str:
    """Format a datetime to ISO minute precision (seconds stripped)."""
    return dt.strftime("%Y-%m-%dT%H:%M")


def _make_dedup_key(event_type: str, tick_time: datetime) -> str:
    return f"{event_type}:{_iso_minute(tick_time)}"


class SchedulerAdapter:
    def __init__(self, config_path: str = DEFAULT_CONFIG_PATH):
        self.config_path = config_path
        self.schedules: list[dict] = []
        self._load()

    def _load(self):
        if not os.path.exists(self.config_path):
            self.schedules = []
            return
        try:
            with open(self.config_path) as f:
                data = yaml.safe_load(f) or {}
            self.schedules = data.get("schedules", []) or []
        except Exception as e:
            log.error("Failed to load scheduler config %s: %s", self.config_path, e)
            self.schedules = []
            return
        seen = {}
        deduped = []
        for s in self.schedules:
            evt = s.get("event")
            if evt in seen:
                log.warning("Scheduler: duplicate schedule for %s, keeping first", evt)
                continue
            seen[evt] = len(deduped)
            deduped.append(s)
        self.schedules = deduped

    def reload(self):
        """Reload config from disk (called on hot-reload interval)."""
        self._load()

    def _get_last_tick(self, cron_expr: str, now: datetime) -> datetime:
        """Return the most recent past cron occurrence <= now."""
        from croniter import croniter
        cron = croniter(cron_expr, now)
        return cron.get_prev(datetime)

    def _dedup_key_exists(self, db, key: str) -> bool:
        """Return True if any event with this dedup_key exists (processed or not).

        More restrictive than the DB insert_event dedup (which only checks
        processed rows). This prevents double-emitting within the same daemon
        session when the event is still in the unprocessed queue.
        """
        row = db.conn.execute(
            "SELECT id FROM events WHERE dedup_key = ?", (key,)
        ).fetchone()
        return row is not None

    def tick(self, db, now: datetime | None = None) -> list[str]:
        """Check all schedules and emit timer events for any due ticks.

        Called on every daemon poll iteration. Uses dedup_key to ensure each
        cron window fires exactly once regardless of how often the daemon loops.

        Returns the list of event_types that were newly emitted.
        """
        if now is None:
            now = datetime.utcnow()
        emitted = []
        seen_keys = set()
        for sched in self.schedules:
            cron_expr = sched.get("cron")
            event_type = sched.get("event")
            if not cron_expr or not event_type:
                continue
            try:
                last_tick = self._get_last_tick(cron_expr, now)
                key = _make_dedup_key(event_type, last_tick)
                if key in seen_keys:
                    continue
                if self._dedup_key_exists(db, key):
                    continue
                payload = json.dumps({"scheduled_at": _iso_minute(last_tick)})
                result = db.insert_event(event_type, payload, "scheduler", dedup_key=key)
                if result is not None:
                    seen_keys.add(key)
                log.info("Scheduler: emitted %s (dedup_key=%s)", event_type, key)
                emitted.append(event_type)
            except Exception as e:
                log.error("Scheduler error for schedule %s: %s", event_type, e)
        return emitted

    def startup_catchup(self, db, now: datetime | None = None) -> list[str]:
        """On daemon startup, emit one catch-up tick per schedule if missed.

        Hardcoded emit_latest_only: only the most recent missed tick is emitted,
        not every tick that fired while the daemon was down. This prevents
        restart storms on long outages.

        Uses insert_event dedup (processed-only check) so already-processed
        ticks are skipped. Also uses _dedup_key_exists to skip queued-but-not-
        processed ticks from a prior session.

        Returns the list of event_types that were newly emitted.
        """
        if now is None:
            now = datetime.utcnow()
        emitted = []
        for sched in self.schedules:
            cron_expr = sched.get("cron")
            event_type = sched.get("event")
            if not cron_expr or not event_type:
                continue
            try:
                last_tick = self._get_last_tick(cron_expr, now)
                key = _make_dedup_key(event_type, last_tick)
                # Skip if already queued OR already processed
                if self._dedup_key_exists(db, key):
                    continue
                payload = json.dumps({
                    "scheduled_at": _iso_minute(last_tick),
                    "catchup": True,
                })
                result = db.insert_event(
                    event_type, payload, "scheduler-catchup", dedup_key=key
                )
                if result is not None:
                    log.info(
                        "Scheduler catchup: emitted %s (dedup_key=%s)", event_type, key
                    )
                    emitted.append(event_type)
            except Exception as e:
                log.error("Scheduler catchup error for %s: %s", event_type, e)
        return emitted
