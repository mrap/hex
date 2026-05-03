# db.py
"""SQLite event bus for hex-events."""
import json
import sqlite3
from datetime import datetime

SCHEMA = """
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at TEXT DEFAULT (datetime('now')),
    processed_at TEXT,
    recipe TEXT,
    dedup_key TEXT
);
CREATE TABLE IF NOT EXISTS action_log (
    id INTEGER PRIMARY KEY,
    event_id INTEGER REFERENCES events(id),
    recipe TEXT NOT NULL,
    action_type TEXT NOT NULL,
    action_detail TEXT,
    status TEXT NOT NULL,
    error_message TEXT,
    executed_at TEXT DEFAULT (datetime('now')),
    action_result TEXT
);
CREATE TABLE IF NOT EXISTS deferred_events (
    id INTEGER PRIMARY KEY,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    source TEXT NOT NULL,
    fire_at TEXT NOT NULL,
    cancel_group TEXT,
    created_at TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_events_unprocessed ON events(processed_at) WHERE processed_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);
CREATE INDEX IF NOT EXISTS idx_events_created ON events(created_at);
CREATE INDEX IF NOT EXISTS idx_events_dedup ON events(dedup_key) WHERE dedup_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_deferred_fire_at ON deferred_events(fire_at);
CREATE TABLE IF NOT EXISTS policy_eval_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id INTEGER NOT NULL REFERENCES events(id),
    policy_name TEXT NOT NULL,
    rule_name TEXT NOT NULL,
    matched BOOLEAN NOT NULL,
    conditions_passed BOOLEAN,
    condition_details TEXT,
    rate_limited BOOLEAN DEFAULT 0,
    action_taken BOOLEAN DEFAULT 0,
    evaluated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_policy_eval_event ON policy_eval_log(event_id);
CREATE INDEX IF NOT EXISTS idx_policy_eval_policy ON policy_eval_log(policy_name);
"""


def parse_duration(s) -> int:
    """Parse a duration string to seconds. Supports s, m, h, d suffixes.

    Examples:
        '30s' -> 30
        '10m' -> 600
        '2h'  -> 7200
        '1d'  -> 86400
        '1'   -> 3600  (bare integer treated as hours, backwards compat)

    Raises ValueError on invalid input (None, empty string, bad suffix, non-numeric prefix).
    """
    if s is None:
        raise ValueError(f"Invalid duration string None: expected format like 10m, 2h, 1d, 30s")
    s = str(s).strip()
    if not s:
        raise ValueError(f"Invalid duration string '': expected format like 10m, 2h, 1d, 30s")
    suffixes = {"s": 1, "m": 60, "h": 3600, "d": 86400}
    if s[-1] in suffixes:
        numeric = s[:-1]
        try:
            return int(numeric) * suffixes[s[-1]]
        except ValueError:
            raise ValueError(f"Invalid duration string {s!r}: expected format like 10m, 2h, 1d, 30s")
    # Bare integer: treat as hours (backwards compatibility)
    try:
        return int(s) * 3600
    except ValueError:
        raise ValueError(f"Invalid duration string {s!r}: expected format like 10m, 2h, 1d, 30s")


class EventsDB:
    def __init__(self, path: str):
        self.conn = sqlite3.connect(path)
        self.conn.row_factory = sqlite3.Row
        self.conn.execute("PRAGMA journal_mode=WAL")
        self.conn.execute("PRAGMA busy_timeout=30000")
        self.conn.executescript(SCHEMA)
        self._migrate()

    def _migrate(self):
        """Add columns to existing tables that predate schema additions."""
        cols = [r[1] for r in self.conn.execute("PRAGMA table_info(events)").fetchall()]
        if "dedup_key" not in cols:
            self.conn.execute("ALTER TABLE events ADD COLUMN dedup_key TEXT")
            self.conn.commit()
        pel_cols = [r[1] for r in self.conn.execute("PRAGMA table_info(policy_eval_log)").fetchall()]
        if "workflow" not in pel_cols:
            self.conn.execute("ALTER TABLE policy_eval_log ADD COLUMN workflow TEXT")
            self.conn.commit()
        al_cols = [r[1] for r in self.conn.execute("PRAGMA table_info(action_log)").fetchall()]
        if "action_result" not in al_cols:
            self.conn.execute("ALTER TABLE action_log ADD COLUMN action_result TEXT")
            self.conn.commit()

    def close(self):
        self.conn.close()

    def insert_event(self, event_type: str, payload: str, source: str,
                     dedup_key: str | None = None) -> int | None:
        """Insert an event. Returns row id, or None if deduped."""
        if dedup_key:
            existing = self.conn.execute(
                "SELECT id FROM events WHERE dedup_key = ? AND processed_at IS NOT NULL",
                (dedup_key,),
            ).fetchone()
            if existing:
                return None  # already processed, skip
        cur = self.conn.execute(
            "INSERT INTO events (event_type, payload, source, dedup_key) VALUES (?, ?, ?, ?)",
            (event_type, payload, source, dedup_key),
        )
        self.conn.commit()
        return cur.lastrowid

    def get_unprocessed(self) -> list[dict]:
        rows = self.conn.execute(
            "SELECT * FROM events WHERE processed_at IS NULL ORDER BY id"
        ).fetchall()
        return [dict(r) for r in rows]

    def count_unprocessed(self) -> int:
        row = self.conn.execute(
            "SELECT COUNT(*) as cnt FROM events WHERE processed_at IS NULL"
        ).fetchone()
        return row["cnt"]

    def mark_processed(self, event_id: int, recipe: str | None = None):
        self.conn.execute(
            "UPDATE events SET processed_at = datetime('now'), recipe = ? WHERE id = ?",
            (recipe, event_id),
        )
        self.conn.commit()

    def log_action(self, event_id: int, recipe: str, action_type: str,
                   action_detail: str, status: str, error_message: str | None = None,
                   action_result: str | None = None):
        self.conn.execute(
            "INSERT INTO action_log (event_id, recipe, action_type, action_detail, status, error_message, action_result) "
            "VALUES (?, ?, ?, ?, ?, ?, ?)",
            (event_id, recipe, action_type, action_detail, status, error_message, action_result),
        )
        self.conn.commit()

    def get_action_logs(self, event_id: int) -> list[dict]:
        rows = self.conn.execute(
            "SELECT * FROM action_log WHERE event_id = ? ORDER BY id", (event_id,)
        ).fetchall()
        return [dict(r) for r in rows]

    def get_rate_limited_by_event(self, event_ids: list[int]) -> dict[int, str]:
        """Return {event_id: policy_name} for events with rate_limited action_log entries."""
        if not event_ids:
            return {}
        placeholders = ",".join("?" * len(event_ids))
        rows = self.conn.execute(
            f"SELECT event_id, recipe FROM action_log WHERE action_type = 'rate_limited' "
            f"AND event_id IN ({placeholders})",
            event_ids,
        ).fetchall()
        return {r["event_id"]: r["recipe"] for r in rows}

    def count_events(self, event_type: str, seconds: int | None = None,
                     hours: int | None = None,
                     payload_filter: tuple[str, str] | None = None) -> int:
        """Count events of event_type within a time window.

        Pass either seconds= or hours= (hours kept for backwards compat).
        If neither is provided, defaults to 1 hour.
        Pass payload_filter=(field, value) to filter by a JSON payload field
        using json_extract (e.g. payload_filter=("rule", "R-033")).
        """
        if seconds is None:
            if hours is not None:
                seconds = hours * 3600
            else:
                seconds = 3600
        if payload_filter:
            field, value = payload_filter
            row = self.conn.execute(
                "SELECT COUNT(*) as cnt FROM events WHERE event_type = ? "
                "AND created_at >= datetime('now', ?) "
                "AND json_extract(payload, ?) = ?",
                (event_type, f"-{seconds} seconds", f"$.{field}", value),
            ).fetchone()
        else:
            row = self.conn.execute(
                "SELECT COUNT(*) as cnt FROM events WHERE event_type = ? "
                "AND created_at >= datetime('now', ?)",
                (event_type, f"-{seconds} seconds"),
            ).fetchone()
        return row["cnt"]

    def history(self, limit: int = 50, since_hours: int | None = None) -> list[dict]:
        if since_hours:
            rows = self.conn.execute(
                "SELECT * FROM events WHERE created_at >= datetime('now', ?) ORDER BY id DESC LIMIT ?",
                (f"-{since_hours} hours", limit),
            ).fetchall()
        else:
            rows = self.conn.execute(
                "SELECT * FROM events ORDER BY id DESC LIMIT ?", (limit,)
            ).fetchall()
        return [dict(r) for r in rows]

    def janitor(self, days: int = 7, vacuum: bool = False) -> int:
        cur = self.conn.execute(
            "DELETE FROM events WHERE created_at < datetime('now', ?)",
            (f"-{days} days",),
        )
        self.conn.execute(
            "DELETE FROM action_log WHERE event_id NOT IN (SELECT id FROM events)"
        )
        self.conn.execute(
            "DELETE FROM policy_eval_log WHERE event_id NOT IN (SELECT id FROM events)"
        )
        self.conn.commit()
        if vacuum and cur.rowcount > 0:
            self.conn.execute("VACUUM")
        return cur.rowcount

    # -----------------------------------------------------------------------
    # Deferred events
    # -----------------------------------------------------------------------

    def insert_deferred(self, event_type: str, payload: str, source: str,
                        fire_at: str, cancel_group: str | None = None):
        """Insert a deferred event. cancel_group replaces any existing row with same group."""
        if cancel_group:
            self.conn.execute(
                "DELETE FROM deferred_events WHERE cancel_group = ?", (cancel_group,)
            )
        self.conn.execute(
            "INSERT INTO deferred_events (event_type, payload, source, fire_at, cancel_group) "
            "VALUES (?, ?, ?, ?, ?)",
            (event_type, payload, source, fire_at, cancel_group),
        )
        self.conn.commit()

    def get_due_deferred(self, now: str | None = None) -> list[dict]:
        """Return deferred events whose fire_at <= now."""
        if now is None:
            now = datetime.utcnow().isoformat()
        rows = self.conn.execute(
            "SELECT * FROM deferred_events WHERE fire_at <= ? ORDER BY fire_at",
            (now,),
        ).fetchall()
        return [dict(r) for r in rows]

    def delete_deferred(self, row_id: int):
        """Delete a deferred event by id (dual-write safety: delete before promoting)."""
        self.conn.execute("DELETE FROM deferred_events WHERE id = ?", (row_id,))
        self.conn.commit()

    # -----------------------------------------------------------------------
    # Policy evaluation log
    # -----------------------------------------------------------------------

    def log_policy_evals(self, rows: list[dict]):
        """Batch insert policy evaluation log entries."""
        if not rows:
            return
        now = datetime.utcnow().isoformat()
        self.conn.executemany(
            "INSERT INTO policy_eval_log "
            "(event_id, policy_name, rule_name, matched, conditions_passed, "
            "condition_details, rate_limited, action_taken, evaluated_at, workflow) "
            "VALUES (:event_id, :policy_name, :rule_name, :matched, :conditions_passed, "
            ":condition_details, :rate_limited, :action_taken, :evaluated_at, :workflow)",
            [
                {
                    "event_id": r["event_id"],
                    "policy_name": r["policy_name"],
                    "rule_name": r["rule_name"],
                    "matched": r["matched"],
                    "conditions_passed": r.get("conditions_passed"),
                    "condition_details": r.get("condition_details"),
                    "rate_limited": r.get("rate_limited", 0),
                    "action_taken": r.get("action_taken", 0),
                    "evaluated_at": r.get("evaluated_at", now),
                    "workflow": r.get("workflow"),
                }
                for r in rows
            ],
        )
        self.conn.commit()

    def get_policy_evals(self, event_id: int, policy_name: str | None = None) -> list[dict]:
        """Return policy eval log entries for an event, optionally filtered by policy name."""
        if policy_name:
            rows = self.conn.execute(
                "SELECT * FROM policy_eval_log WHERE event_id = ? AND policy_name = ? ORDER BY id",
                (event_id, policy_name),
            ).fetchall()
        else:
            rows = self.conn.execute(
                "SELECT * FROM policy_eval_log WHERE event_id = ? ORDER BY id",
                (event_id,),
            ).fetchall()
        return [dict(r) for r in rows]

    def get_rule_first_fire(self, policy_name: str, rule_name: str) -> str | None:
        """Return the earliest evaluated_at timestamp where the rule took action, or None."""
        row = self.conn.execute(
            "SELECT MIN(evaluated_at) as first_fire FROM policy_eval_log "
            "WHERE policy_name = ? AND rule_name = ? AND action_taken = 1",
            (policy_name, rule_name),
        ).fetchone()
        return row["first_fire"] if row else None

    def count_policy_fires(self, policy_name: str) -> int:
        """Count distinct events where the policy took action (for max_fires tracking)."""
        row = self.conn.execute(
            "SELECT COUNT(DISTINCT event_id) as cnt FROM policy_eval_log "
            "WHERE policy_name = ? AND action_taken = 1",
            (policy_name,),
        ).fetchone()
        return row["cnt"] if row else 0

    def get_policy_evals_since(self, policy_name: str, since_hours: int) -> list[dict]:
        """Return policy eval log entries for a policy within the last N hours."""
        rows = self.conn.execute(
            "SELECT pel.*, e.event_type, e.created_at as event_created_at "
            "FROM policy_eval_log pel JOIN events e ON e.id = pel.event_id "
            "WHERE pel.policy_name = ? AND pel.evaluated_at >= datetime('now', ?) "
            "ORDER BY pel.id DESC",
            (policy_name, f"-{since_hours} hours"),
        ).fetchall()
        return [dict(r) for r in rows]
