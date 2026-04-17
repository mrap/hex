#!/bin/bash
# Returns today's date in the user's configured timezone.
# Use this ALWAYS when creating date-stamped files (landings, meetings, etc.)
# Usage: $(bash $HEX_DIR/.hex/scripts/today.sh)
#   or:  $(bash $HEX_DIR/.hex/scripts/today.sh +%a)  # day name

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HEX_DIR="${HEX_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

if [ -f "$HEX_DIR/.hex/timezone" ]; then
  TZ="$(cat "$HEX_DIR/.hex/timezone" | tr -d '[:space:]')"; export TZ
fi

if [ -n "$1" ]; then
  date "$1"
else
  date +%Y-%m-%d
fi
