#!/usr/bin/env bash
# watchdog-run-full.sh — run the initiative watchdog full check (all checks).
# Called from the initiative-watchdog-12h policy on each 6h tick.
# Uses hour-of-day modulo to fire approximately every 12h (at 00:xx and 12:xx).
set -uo pipefail

HEX_ROOT="${HEX_ROOT:-${AGENT_DIR:-$HOME/hex}}"
CURRENT_HOUR=$(date +%H | sed 's/^0//')  # strip leading zero so 09 -> 9

# Only run at hours 0 and 12 (approx every 12h)
if [ "$CURRENT_HOUR" != "0" ] && [ "$CURRENT_HOUR" != "12" ]; then
    echo "[watchdog-12h] Hour $CURRENT_HOUR — skipping (only runs at hour 0 and 12)"
    exit 0
fi

echo "[watchdog-12h] Hour $CURRENT_HOUR — running full watchdog check"
python3 "$HEX_ROOT/.hex/scripts/initiative-watchdog.py" --full >> ~/.hex/audit/initiative-watchdog-runs.log 2>&1
