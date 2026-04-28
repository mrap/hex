#!/usr/bin/env python3
"""hex-emit — lightweight event emitter for trigger adapters.

Usage: hex-emit <event_type> [payload_json] [source]
       hex-emit --db /path/to/db <event_type> [payload_json] [source]

Tries the hex server HTTP endpoint first (fast path). Falls back to direct
SQLite write when the server is not running.
"""
import argparse
import json
import os
import sys
import urllib.request
import urllib.error

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from db import EventsDB

DEFAULT_DB = os.path.expanduser("~/.hex-events/events.db")
HEX_SERVER_URL = "http://127.0.0.1:8880/events/ingest"

# Known valid source patterns. Validation is advisory only (warning, not error).
# Pattern: exact string or prefix ending with ":"
VALID_SOURCE_PREFIXES = ("hex:", "user", "unknown")


def _validate_source(source: str) -> None:
    """Warn if source doesn't match known patterns. Does not block emission."""
    for prefix in VALID_SOURCE_PREFIXES:
        if source == prefix or source.startswith("hex:"):
            return
    print(
        f"[hex-emit] WARNING: unrecognized source '{source}'. "
        f"Expected: user, unknown, or hex:<name>. Event will still be emitted.",
        file=sys.stderr,
    )


def _emit_http(event_type: str, payload: str, source: str) -> bool:
    """Try to emit via hex server HTTP endpoint. Returns True on success."""
    try:
        body = json.dumps({
            "event_type": event_type,
            "payload": json.loads(payload) if payload else {},
            "source": source,
        }).encode()
        req = urllib.request.Request(
            HEX_SERVER_URL,
            data=body,
            headers={"Content-Type": "application/json"},
        )
        urllib.request.urlopen(req, timeout=5)
        return True
    except Exception:
        return False


def _emit_sqlite(event_type: str, payload: str, source: str, db_path: str) -> int:
    """Fallback: write directly to SQLite."""
    db = EventsDB(db_path)
    eid = db.insert_event(event_type, payload, source)
    db.close()
    return eid


def main():
    parser = argparse.ArgumentParser(description="Emit a hex-event")
    parser.add_argument("event_type", help="Event type (e.g., boi.spec.completed)")
    parser.add_argument("payload", nargs="?", default="{}", help="JSON payload")
    parser.add_argument("source", nargs="?", default="unknown", help="Event source")
    parser.add_argument("--db", default=DEFAULT_DB, help="Database path (fallback)")
    args = parser.parse_args()

    _validate_source(args.source)

    try:
        json.loads(args.payload)
    except json.JSONDecodeError as e:
        print(f"[hex-emit] WARNING: payload is not valid JSON: {e}. Storing raw string.", file=sys.stderr)

    if _emit_http(args.event_type, args.payload, args.source):
        print(f"Event emitted via server: {args.event_type}")
    else:
        eid = _emit_sqlite(args.event_type, args.payload, args.source, args.db)
        print(f"Event {eid}: {args.event_type}")

if __name__ == "__main__":
    main()
