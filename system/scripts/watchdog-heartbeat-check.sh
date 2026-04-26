#!/usr/bin/env bash
# watchdog-heartbeat-check.sh — check that the initiative watchdog has run recently.
# Emits hex.watchdog.down if no heartbeat found in the last 12h.
set -uo pipefail

HEARTBEAT_FILE="$HOME/.hex/audit/watchdog-heartbeat.jsonl"
HEX_EMIT="$HOME/.hex-events/hex_emit.py"

if [ ! -f "$HEARTBEAT_FILE" ]; then
    echo "[watchdog-heartbeat] No heartbeat file — watchdog has never run"
    python3 "$HEX_EMIT" hex.watchdog.down \
        '{"reason": "no heartbeat file", "source": "initiative-watchdog-heartbeat"}' \
        initiative-watchdog-heartbeat || true
    exit 0
fi

STALE=$(tail -50 "$HEARTBEAT_FILE" | python3 -c "
import json, sys
from datetime import datetime, timezone, timedelta
threshold = datetime.now(timezone.utc) - timedelta(hours=12)
found = False
for line in sys.stdin:
    try:
        d = json.loads(line.strip())
        ts_str = d.get('ts', '')
        if ts_str:
            ts = datetime.fromisoformat(ts_str.replace('Z', '+00:00'))
            if ts > threshold:
                found = True
                break
    except Exception:
        pass
print('0' if found else '1')
" 2>/dev/null || echo "1")

if [ "${STALE:-1}" = "1" ]; then
    echo "[watchdog-heartbeat] No heartbeat in last 12h — emitting hex.watchdog.down"
    python3 "$HEX_EMIT" hex.watchdog.down \
        '{"reason": "no heartbeat in 12h", "source": "initiative-watchdog-heartbeat"}' \
        initiative-watchdog-heartbeat || true
else
    echo "[watchdog-heartbeat] Heartbeat OK"
fi
