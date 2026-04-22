"""Telemetry wrapper: emit hex.integration.* events via hex_emit.py."""
import json
import os
import subprocess
import sys
from typing import Any

HEX_EMIT = os.path.expanduser("~/.hex-events/hex_emit.py")


def emit(event_type: str, payload: dict[str, Any] | None = None, source: str = "hex-integration") -> None:
    """
    Emit a telemetry event. Non-fatal: exceptions are caught and logged to stderr.
    event_type: e.g. "hex.integration.installed.ok"
    """
    if not os.path.isfile(HEX_EMIT):
        return
    payload_str = json.dumps(payload or {})
    try:
        subprocess.run(
            [sys.executable, HEX_EMIT, event_type, payload_str, source],
            capture_output=True,
            timeout=5,
        )
    except Exception as e:
        print(f"[hex-integration] WARN: telemetry emit failed: {e}", file=sys.stderr)
