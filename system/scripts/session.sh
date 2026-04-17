#!/usr/bin/env bash
# session — manage active hex sessions
# Records session start/stop events to .hex/sessions.log for /hex-shutdown deregistration.
#
# Usage:
#   bash session.sh start [label]         — register a new session, prints SESSION_ID
#   bash session.sh stop  <SESSION_ID>    — deregister a session
#   bash session.sh check                 — list active sessions
set -uo pipefail

HEX_DIR="${HEX_DIR:-$HOME/hex}"
SESSIONS_LOG="$HEX_DIR/.hex/sessions.log"
COMMAND="${1:-check}"
shift 2>/dev/null || true

ensure_log() {
    mkdir -p "$(dirname "$SESSIONS_LOG")"
    touch "$SESSIONS_LOG"
}

cmd_start() {
    local label="${1:-unlabeled}"
    local session_id
    session_id="hex-$(date +%s)-$$"
    ensure_log
    echo "ACTIVE $session_id $(date '+%Y-%m-%d %H:%M:%S') $label" >> "$SESSIONS_LOG"
    echo "$session_id"
}

cmd_stop() {
    local session_id="${1:-}"
    if [ -z "$session_id" ]; then
        echo "session stop: SESSION_ID required" >&2
        exit 1
    fi
    ensure_log
    # Mark the session as stopped in the log (sed in-place replacement)
    if grep -q "ACTIVE $session_id" "$SESSIONS_LOG" 2>/dev/null; then
        sed -i.bak "s/^ACTIVE $session_id/STOPPED $session_id/" "$SESSIONS_LOG"
        rm -f "${SESSIONS_LOG}.bak"
        echo "session $session_id stopped"
    else
        echo "session $session_id not found in active sessions" >&2
        exit 1
    fi
}

cmd_check() {
    ensure_log
    local active
    active=$(grep '^ACTIVE ' "$SESSIONS_LOG" 2>/dev/null || true)
    if [ -z "$active" ]; then
        echo "No active sessions."
    else
        echo "Active sessions:"
        echo "$active"
    fi
}

case "$COMMAND" in
    start) cmd_start "$@" ;;
    stop)  cmd_stop  "$@" ;;
    check) cmd_check ;;
    *)
        echo "Usage: session.sh {start|stop|check}" >&2
        exit 1
        ;;
esac
