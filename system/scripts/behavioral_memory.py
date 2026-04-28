#!/usr/bin/env python3
"""Behavioral memory layer — error-to-lesson loop.

Provides:
  - Schema creation in memory.db
  - Bootstrap from existing feedback_*.md files
  - Live pattern upsert (store correction)
  - Query interface: check_behavior(query) → matches + risk_level
  - Recurrence tracking and escalation detection
"""

import hashlib
import json
import math
import os
import re
import sqlite3
import uuid
from datetime import datetime, timezone
from pathlib import Path

HEX_ROOT = os.environ.get("HEX_ROOT", os.path.expanduser("~/hex"))
MEMORY_DB = os.path.join(HEX_ROOT, ".hex", "memory.db")
_AGENT_DIR = os.environ.get("AGENT_DIR", os.path.expanduser("~/hex"))
FEEDBACK_DIR = Path(os.environ.get(
    "CLAUDE_PROJECT_MEMORY",
    str(Path.home() / ".claude" / "projects" / ("-" + _AGENT_DIR.replace("/", "-").lstrip("-")) / "memory")
))
ESCALATIONS_FILE = os.path.join(HEX_ROOT, "evolution", "behavioral-escalations.md")

HOOK_SIGNALS = [
    (r"cron(?:create)?", "settings.json block"),
    (r"slack", "hex-events:before_slack_message"),
    (r"publish", "hex-events:before_publish"),
    (r"send.*email", "hex-events:before_email_send"),
    (r"markdown.*table.*slack|slack.*markdown", "SO_S8"),
    (r"force.*push", "git pre-push hook"),
    (r"rm\s+-rf", "shell allowlist"),
    (r"delete.*branch", "git hook"),
]

MEMORY_SIGNALS = [
    r"answer.*question",
    r"explain.*before",
    r"product.*judgment",
    r"voice.*tone",
    r"summarize",
    r"verbose|brief",
    r"context",
    r"ask.*when",
]


def _db_connect(readonly: bool = False) -> sqlite3.Connection:
    uri = f"file:{MEMORY_DB}{'?mode=ro' if readonly else ''}".replace(" ", "%20")
    conn = sqlite3.connect(uri, uri=True, check_same_thread=False)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA journal_mode=WAL")
    return conn


def ensure_schema(conn: sqlite3.Connection):
    conn.executescript("""
CREATE TABLE IF NOT EXISTS behavioral_patterns (
    id                  TEXT PRIMARY KEY,
    pattern_text        TEXT NOT NULL,
    rule_text           TEXT NOT NULL,
    source_file         TEXT,
    source_session      TEXT,
    guard_type          TEXT NOT NULL DEFAULT 'unclassified'
                        CHECK (guard_type IN ('hook','memory','both','unclassified')),
    hook_ref            TEXT,
    lifecycle_state     TEXT NOT NULL DEFAULT 'active'
                        CHECK (lifecycle_state IN ('active','guarded','resolved')),
    correction_count    INTEGER NOT NULL DEFAULT 1,
    recurrence_rate     REAL,
    last_recurrence     TEXT,
    first_seen          TEXT NOT NULL,
    last_updated        TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_bp_guard_type ON behavioral_patterns(guard_type);
CREATE INDEX IF NOT EXISTS idx_bp_lifecycle  ON behavioral_patterns(lifecycle_state);
CREATE INDEX IF NOT EXISTS idx_bp_rate       ON behavioral_patterns(recurrence_rate);

CREATE VIRTUAL TABLE IF NOT EXISTS behavioral_patterns_fts
USING fts5(pattern_text, rule_text, content='behavioral_patterns', content_rowid='rowid');

CREATE TABLE IF NOT EXISTS behavioral_incidences (
    id              TEXT PRIMARY KEY,
    pattern_id      TEXT NOT NULL REFERENCES behavioral_patterns(id),
    detected_at     TEXT NOT NULL,
    detection_type  TEXT NOT NULL
                    CHECK (detection_type IN ('agent_self_check','transcript_scan','live_correction')),
    session_id      TEXT,
    agent_id        TEXT,
    context_snippet TEXT,
    was_prevented   INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_bi_pattern_id  ON behavioral_incidences(pattern_id);
CREATE INDEX IF NOT EXISTS idx_bi_detected_at ON behavioral_incidences(detected_at);
""")
    conn.commit()


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def _today() -> str:
    return datetime.now(timezone.utc).date().isoformat()


def _pat_id(text: str) -> str:
    return hashlib.sha256(text.lower().strip().encode()).hexdigest()[:16]


def classify_pattern(pattern_text: str, rule_text: str) -> tuple[str, str | None]:
    combined = (pattern_text + " " + rule_text).lower()
    for signal, hook_ref in HOOK_SIGNALS:
        if re.search(signal, combined):
            return "hook", hook_ref
    for signal in MEMORY_SIGNALS:
        if re.search(signal, combined):
            return "memory", None
    return "unclassified", None


def _recurrence_rate(correction_count: int, first_seen: str) -> float | None:
    try:
        first = datetime.fromisoformat(first_seen.replace("Z", "+00:00"))
        days = max(1, (datetime.now(timezone.utc) - first).days)
        return round(correction_count / days * 7, 3)
    except Exception:
        return None


def _parse_feedback_file(path: Path) -> dict | None:
    try:
        text = path.read_text(encoding="utf-8")
    except Exception:
        return None

    # Extract YAML frontmatter
    fm: dict = {}
    body = text
    if text.startswith("---"):
        parts = text.split("---", 2)
        if len(parts) >= 3:
            try:
                import yaml  # optional — fall back gracefully
                fm = yaml.safe_load(parts[1]) or {}
            except Exception:
                pass
            body = parts[2].strip()

    name = fm.get("name") or path.stem.replace("feedback_", "").replace("_", " ").strip()
    description = fm.get("description", "")
    source_session = fm.get("originSessionId") or fm.get("source_session")

    # Extract rule_text
    rule_match = re.search(r"\*\*(?:Rule|How to apply):\*\*\s*(.+?)(?=\n\n|\*\*|\Z)", body, re.DOTALL)
    rule_text = rule_match.group(1).strip() if rule_match else body[:400].strip()

    pattern_text = f"{name}. {description}".strip(". ") if description else name

    first_seen_ts = datetime.fromtimestamp(path.stat().st_mtime, tz=timezone.utc).isoformat(timespec="seconds")

    guard_type, hook_ref = classify_pattern(pattern_text, rule_text)

    return {
        "id": _pat_id(pattern_text),
        "pattern_text": pattern_text[:500],
        "rule_text": rule_text[:1000],
        "source_file": str(path),
        "source_session": source_session,
        "guard_type": guard_type,
        "hook_ref": hook_ref,
        "lifecycle_state": "active",
        "correction_count": 1,
        "recurrence_rate": None,
        "last_recurrence": None,
        "first_seen": first_seen_ts,
        "last_updated": first_seen_ts,
    }


def bootstrap(conn: sqlite3.Connection, feedback_dir: Path = FEEDBACK_DIR) -> dict:
    """Load existing feedback_*.md files into behavioral_patterns. Idempotent."""
    ensure_schema(conn)
    imported = 0
    skipped = 0
    errors = 0
    if not feedback_dir.exists():
        return {"imported": 0, "skipped": 0, "errors": 0, "note": "feedback dir not found"}

    for path in sorted(feedback_dir.glob("feedback_*.md")):
        rec = _parse_feedback_file(path)
        if not rec:
            errors += 1
            continue
        try:
            cur = conn.execute(
                "INSERT OR IGNORE INTO behavioral_patterns VALUES "
                "(:id,:pattern_text,:rule_text,:source_file,:source_session,"
                ":guard_type,:hook_ref,:lifecycle_state,:correction_count,"
                ":recurrence_rate,:last_recurrence,:first_seen,:last_updated)",
                rec,
            )
            if cur.rowcount:
                imported += 1
                # Sync FTS manually (trigger may not fire for virtual table)
                conn.execute(
                    "INSERT INTO behavioral_patterns_fts(rowid, pattern_text, rule_text) "
                    "SELECT rowid, pattern_text, rule_text FROM behavioral_patterns WHERE id=?",
                    (rec["id"],),
                )
            else:
                skipped += 1
        except Exception:
            errors += 1

    conn.commit()
    return {"imported": imported, "skipped": skipped, "errors": errors}


def store_correction(
    conn: sqlite3.Connection,
    pattern_text: str,
    rule_text: str,
    session_id: str = "",
    agent_id: str = "hex",
    source_file: str = "",
    detection_type: str = "live_correction",
) -> dict:
    """Upsert a behavioral pattern and record an incidence."""
    ensure_schema(conn)
    pat_id = _pat_id(pattern_text)
    now = _now_iso()

    existing = conn.execute(
        "SELECT id, correction_count, first_seen FROM behavioral_patterns WHERE id=?",
        (pat_id,),
    ).fetchone()

    if existing:
        new_count = existing["correction_count"] + 1
        rate = _recurrence_rate(new_count, existing["first_seen"])
        conn.execute(
            "UPDATE behavioral_patterns SET correction_count=?, last_recurrence=?, "
            "recurrence_rate=?, last_updated=? WHERE id=?",
            (new_count, _today(), rate, now, pat_id),
        )
        action = "incremented"
        # Check escalation
        if rate and rate > 2.0 and new_count > 5:
            _write_escalation(pattern_text, rule_text, new_count, rate)
    else:
        guard_type, hook_ref = classify_pattern(pattern_text, rule_text)
        conn.execute(
            "INSERT OR IGNORE INTO behavioral_patterns VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (pat_id, pattern_text[:500], rule_text[:1000], source_file, session_id,
             guard_type, hook_ref, "active", 1, None, None, now, now),
        )
        # Sync FTS
        try:
            conn.execute(
                "INSERT INTO behavioral_patterns_fts(rowid, pattern_text, rule_text) "
                "SELECT rowid, pattern_text, rule_text FROM behavioral_patterns WHERE id=?",
                (pat_id,),
            )
        except Exception:
            pass
        action = "created"

    # Record incidence
    inc_id = str(uuid.uuid4())
    conn.execute(
        "INSERT INTO behavioral_incidences VALUES (?,?,?,?,?,?,?,?)",
        (inc_id, pat_id, now, detection_type, session_id, agent_id, pattern_text[:200], 0),
    )
    conn.commit()
    return {"pattern_id": pat_id, "action": action}


def check_behavior(query: str, limit: int = 5) -> dict:
    """Query behavioral_patterns for relevant corrections before acting.

    Returns matches with risk_level so callers can gate or log.
    """
    if not os.path.exists(MEMORY_DB):
        return {"query": query, "matches": [], "risk_level": "NONE", "note": "memory.db not found"}

    try:
        conn = _db_connect(readonly=True)
        ensure_schema(conn)
    except Exception as exc:
        return {"query": query, "matches": [], "risk_level": "NONE", "error": str(exc)}

    # FTS5 keyword match
    words = re.sub(r"[^\w\s]", " ", query).split()
    fts_query = " OR ".join(words[:6]) if words else query[:80]

    try:
        rows = conn.execute(
            """
            SELECT bp.id, bp.pattern_text, bp.rule_text, bp.guard_type,
                   bp.hook_ref, bp.correction_count, bp.last_recurrence,
                   bp.recurrence_rate, bp.lifecycle_state,
                   bm25(behavioral_patterns_fts) AS score
            FROM behavioral_patterns bp
            JOIN behavioral_patterns_fts ON bp.rowid = behavioral_patterns_fts.rowid
            WHERE behavioral_patterns_fts MATCH ?
              AND bp.lifecycle_state != 'resolved'
            ORDER BY score
            LIMIT ?
            """,
            (fts_query, limit),
        ).fetchall()
    except Exception:
        rows = []

    matches = []
    for row in rows:
        count = row["correction_count"] or 1
        rate = row["recurrence_rate"]
        # Recurrence weight — surfaces persistent mistakes higher
        weight = 1 + math.log1p(count) * 0.2
        matches.append({
            "pattern_id": row["id"],
            "pattern": row["pattern_text"],
            "pattern_text": row["pattern_text"],
            "rule": row["rule_text"],
            "correction_count": count,
            "last_recurrence": row["last_recurrence"],
            "recurrence_rate": rate,
            "guard_type": row["guard_type"],
            "hook_ref": row["hook_ref"],
            "lifecycle_state": row["lifecycle_state"],
            "score": round(abs(row["score"]) * weight, 4),
        })

    matches.sort(key=lambda m: m["score"], reverse=True)

    # Compute risk_level from top match
    risk_level = "NONE"
    if matches:
        top = matches[0]
        c, r, g = top["correction_count"], top["recurrence_rate"] or 0, top["guard_type"]
        if c >= 5 or r > 1.0 or g == "hook":
            risk_level = "HIGH"
        elif c >= 2 or r >= 0.3:
            risk_level = "MEDIUM"
        else:
            risk_level = "LOW"

    conn.close()
    return {
        "query": query,
        "checked_at": _now_iso(),
        "matches": matches,
        "risk_level": risk_level,
    }


def get_behavioral_health() -> dict:
    """Return summary stats about behavioral_patterns for Pulse system health."""
    if not os.path.exists(MEMORY_DB):
        return {"status": "no_db", "total": 0}
    try:
        conn = _db_connect(readonly=True)
        ensure_schema(conn)
        row = conn.execute(
            "SELECT COUNT(*) as total, "
            "SUM(CASE WHEN guard_type='hook' THEN 1 ELSE 0 END) as hooked, "
            "SUM(CASE WHEN guard_type='memory' THEN 1 ELSE 0 END) as memory_only, "
            "SUM(CASE WHEN guard_type='unclassified' THEN 1 ELSE 0 END) as unclassified, "
            "MAX(recurrence_rate) as max_rate, "
            "SUM(correction_count) as total_corrections "
            "FROM behavioral_patterns WHERE lifecycle_state != 'resolved'"
        ).fetchone()

        high_recurrence = conn.execute(
            "SELECT COUNT(*) FROM behavioral_patterns "
            "WHERE recurrence_rate > 1.0 AND lifecycle_state='active'"
        ).fetchone()[0]

        conn.close()
        return {
            "status": "ok",
            "total_patterns": row["total"] or 0,
            "hooked": row["hooked"] or 0,
            "memory_only": row["memory_only"] or 0,
            "unclassified": row["unclassified"] or 0,
            "total_corrections": row["total_corrections"] or 0,
            "max_recurrence_rate": row["max_rate"],
            "high_recurrence_count": high_recurrence or 0,
        }
    except Exception as exc:
        return {"status": "error", "error": str(exc), "total": 0}


def _write_escalation(pattern_text: str, rule_text: str, count: int, rate: float):
    os.makedirs(os.path.dirname(ESCALATIONS_FILE), exist_ok=True)
    entry = (
        f"\n## Escalation: {_today()}\n"
        f"**Pattern:** {pattern_text[:200]}\n"
        f"**Rule:** {rule_text[:300]}\n"
        f"**Recurrence:** {count}× total, {rate:.2f}/week\n"
        f"**Recommendation:** Implement a mechanical hook guard for this pattern.\n"
    )
    with open(ESCALATIONS_FILE, "a", encoding="utf-8") as fh:
        fh.write(entry)


class BehavioralMemory:
    """Object-oriented wrapper around behavioral memory functions.

    Manages its own connection and provides the same operations as the
    module-level functions, but scoped to a single DB handle for convenience.
    """

    def __init__(self, db_path: str | None = None):
        self._db_path = db_path or MEMORY_DB
        self._conn: sqlite3.Connection | None = None

    def _get_conn(self) -> sqlite3.Connection:
        if self._conn is None:
            os.makedirs(os.path.dirname(self._db_path), exist_ok=True)
            uri = f"file:{self._db_path}".replace(" ", "%20")
            self._conn = sqlite3.connect(uri, uri=True, check_same_thread=False)
            self._conn.row_factory = sqlite3.Row
            self._conn.execute("PRAGMA journal_mode=WAL")
        return self._conn

    def ensure_schema(self):
        ensure_schema(self._get_conn())

    def load_feedback_from_files(self, feedback_dir: Path | str | None = None) -> dict:
        """Bootstrap from feedback_*.md files. Alias for module-level bootstrap()."""
        fd = Path(feedback_dir) if feedback_dir else FEEDBACK_DIR
        return bootstrap(self._get_conn(), fd)

    def store_correction(
        self,
        pattern_text: str,
        rule_text: str,
        session_id: str = "",
        agent_id: str = "hex",
        source_file: str = "",
        detection_type: str = "live_correction",
    ) -> dict:
        return store_correction(
            self._get_conn(), pattern_text, rule_text,
            session_id=session_id, agent_id=agent_id,
            source_file=source_file, detection_type=detection_type,
        )

    def check_behavior(self, query: str, limit: int = 5) -> list[dict]:
        """Query for relevant corrections. Returns list of match dicts."""
        result = check_behavior(query, limit=limit)
        return result.get("matches", [])

    def get_health(self) -> dict:
        return get_behavioral_health()

    def close(self):
        if self._conn:
            self._conn.close()
            self._conn = None
