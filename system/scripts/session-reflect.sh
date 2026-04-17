#!/usr/bin/env bash
# session-reflect — post-session reflection orchestrator
# Runs after a session ends: summarises observations, updates reflection-log.md,
# and optionally calls session-delta.py to persist eval records.
#
# Usage: bash session-reflect.sh [--session-id ID] [--quiet]
set -uo pipefail

HEX_DIR="${HEX_DIR:-$HOME/hex}"
QUIET=0
SESSION_ID=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --session-id) SESSION_ID="$2"; shift 2 ;;
        --quiet)      QUIET=1; shift ;;
        *)            shift ;;
    esac
done

log() { [ "$QUIET" -eq 0 ] && echo "$@" || true; }

log "session-reflect: starting post-session reflection"

REFLECTION_LOG="$HEX_DIR/evolution/reflection-log.md"
DELTA_SCRIPT="$HEX_DIR/evolution/eval/session-delta.py"

# Append timestamped entry to reflection log
mkdir -p "$(dirname "$REFLECTION_LOG")"
{
    echo ""
    echo "## $(date '+%Y-%m-%d %H:%M') — session reflection"
    [ -n "$SESSION_ID" ] && echo "Session: $SESSION_ID"
    echo "(reflection placeholder — see observations.md)"
} >> "$REFLECTION_LOG"

# Call session-delta.py if present and memory.db exists
if [ -f "$DELTA_SCRIPT" ] && [ -f "$HEX_DIR/.hex/memory.db" ]; then
    python3 "$DELTA_SCRIPT" --session-id "$SESSION_ID" || true
fi

log "session-reflect: done"
