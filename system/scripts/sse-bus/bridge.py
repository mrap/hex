#!/usr/bin/env python3
"""Bridge hex-events to the SSE bus.

Usage: bridge.py <hex_event_name> <payload_json>
"""

import fnmatch
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

AGENT_DIR = os.environ.get("AGENT_DIR", os.path.expanduser("~/mrap-hex"))
MANIFESTS_DIR = Path(AGENT_DIR) / ".hex" / "sse" / "topics"
BUS_URL = os.environ.get("SSE_BUS_URL", "http://127.0.0.1:8880")


def _load_manifests():
    """Return mapping: hex_event_name -> {topic, type}."""
    mapping = {}
    try:
        import yaml
        use_yaml = True
    except ImportError:
        use_yaml = False

    for path in MANIFESTS_DIR.glob("*.yaml"):
        try:
            if use_yaml:
                with open(path) as f:
                    manifest = yaml.safe_load(f)
            else:
                # Minimal fallback: extract bridge entries with regex
                import re
                text = path.read_text()
                topic_m = re.search(r"^topic:\s*(.+)$", text, re.MULTILINE)
                bridge_entries = re.findall(r"^\s+-\s+(\S+)$", text, re.MULTILINE)
                if not topic_m:
                    continue
                manifest = {"topic": topic_m.group(1).strip(), "bridge": bridge_entries, "events": []}

            topic = manifest.get("topic", "")
            events = manifest.get("events", [])
            # Build type list: first event type is the default, or derive from event name
            event_types = [e.get("type", "") for e in events]

            for bridge_entry in manifest.get("bridge", []):
                # Map bridge entry to (topic, event_type)
                # Use the first event type whose name matches the suffix of the hex event
                matched_type = _match_event_type(bridge_entry, event_types)
                mapping[bridge_entry] = {"topic": topic, "type": matched_type}
        except Exception as e:
            print(f"warning: failed to load {path}: {e}", file=sys.stderr)

    return mapping


def _match_event_type(hex_event: str, event_types: list) -> str:
    """Guess the SSE event type from the hex event name and available types."""
    suffix = hex_event.rsplit(".", 1)[-1]
    # Direct match
    if suffix in event_types:
        return suffix
    # Common mappings
    mappings = {
        "created": "created",
        "updated": "status_changed",
        "woke": "wake_started",
        "failed": "wake_failed",
        "dispatched": "dispatched",
        "completed": "completed",
        "registered": "registered",
        "removed": "removed",
    }
    for key, val in mappings.items():
        if key in suffix or fnmatch.fnmatch(suffix, key):
            if val in event_types:
                return val
    # Return first event type or the suffix as fallback
    return event_types[0] if event_types else suffix


def _resolve_wildcard_match(hex_event_name: str, mapping: dict):
    """Find the best mapping entry for a hex event, supporting glob wildcards."""
    # Exact match first
    if hex_event_name in mapping:
        return mapping[hex_event_name]
    # Wildcard match: check if any key pattern matches the event name
    for pattern, info in mapping.items():
        if "*" in pattern and fnmatch.fnmatch(hex_event_name, pattern):
            return info
    return None


def main():
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <hex_event_name> <payload_json>", file=sys.stderr)
        sys.exit(1)

    hex_event_name = sys.argv[1]
    try:
        raw_payload = json.loads(sys.argv[2])
    except json.JSONDecodeError as e:
        print(f"warning: invalid payload JSON: {e}", file=sys.stderr)
        raw_payload = {}

    mapping = _load_manifests()
    info = _resolve_wildcard_match(hex_event_name, mapping)

    if info is None:
        print(f"warning: no manifest mapping for {hex_event_name!r}, publishing to raw topic", file=sys.stderr)
        # Publish anyway with topic derived from event name
        parts = hex_event_name.split(".")
        topic = ".".join(parts[:2]) if len(parts) >= 2 else hex_event_name
        event_type = parts[-1] if len(parts) >= 2 else "unknown"
        info = {"topic": topic, "type": event_type}

    body = json.dumps({
        "topic": info["topic"],
        "type": info["type"],
        "payload": raw_payload,
    }).encode()

    try:
        req = urllib.request.Request(
            f"{BUS_URL}/events/publish",
            data=body,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=4) as resp:
            print(f"bridge: {hex_event_name} → {info['topic']}/{info['type']} ({resp.status})", file=sys.stderr)
    except (urllib.error.URLError, OSError) as e:
        print(f"warning: SSE bus unreachable ({BUS_URL}): {e}", file=sys.stderr)


if __name__ == "__main__":
    main()
