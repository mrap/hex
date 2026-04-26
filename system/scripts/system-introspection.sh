#!/bin/bash
# system-introspection.sh — Nightly system health audit
# Triggered by hex-events policy. Runs claude -p to audit and write report.

set -uo pipefail

CLAUDE_BIN="/Users/mrap/.local/bin/claude"
AGENT_DIR="${AGENT_DIR:-$HOME/mrap-hex}"
REPORT_DIR="$AGENT_DIR/raw/research/introspection"
LOG_DIR="$AGENT_DIR/raw/research/introspection/logs"
mkdir -p "$REPORT_DIR" "$LOG_DIR"

# Use configured timezone (SO #17)
if [ -z "${TZ:-}" ] && [ -f "$AGENT_DIR/.hex/timezone" ]; then
  TZ="$(tr -d '[:space:]' < "$AGENT_DIR/.hex/timezone")"; export TZ
fi
DATE=$(date +%Y-%m-%d)
REPORT="$REPORT_DIR/$DATE.md"
ERROR_LOG="$LOG_DIR/$DATE.err.log"

log_err() {
    echo "[$(date +%Y-%m-%dT%H:%M:%S)] $*" >> "$ERROR_LOG"
    echo "$*" >&2
}

if [ ! -x "$CLAUDE_BIN" ]; then
    log_err "[introspection] ERROR: claude binary not found at $CLAUDE_BIN"
    exit 1
fi

PROMPT="You are hex's system introspection agent. Perform a thorough nightly audit. Your working directory is $AGENT_DIR.

Audit checklist:
1. Log Audit: Read the last 50 lines of the 3 most recent files in ~/.boi/logs/ and ~/github.com/mrap/hex-events/daemon.log. Flag errors, crashes, recurring warnings.
2. Repo Hygiene: For each repo in ~/github.com/mrap/ (hex, hex-core, hex-ui, hex-events, boi), run git status --short. Flag uncommitted changes.
3. Daemon Health: Run bash $AGENT_DIR/.hex/scripts/hex-daemons.sh status. Flag daemons down.
4. Research Feed Health: Check $AGENT_DIR/raw/research/bookmarks/ and $AGENT_DIR/raw/research/scout/ for files modified in last 48h.
5. Stale Work: Check $AGENT_DIR/todo.md for items with stale since older than 30 days.

Output a markdown report with: date, issues found (CRITICAL/WARNING/INFO), recommendations, and a health score 1-10. Be concise — max 100 lines."

REPORT_TMP="${REPORT}.tmp"
TIMEOUT_SECS=300
TIMED_OUT_FLAG="/tmp/introspection-timeout-$$"

# Run claude in background with shell-native watchdog (macOS lacks timeout command).
# Redirect stdin from /dev/null to avoid "no stdin data" warning in non-interactive env.
"$CLAUDE_BIN" --dangerously-skip-permissions -p "$PROMPT" < /dev/null > "$REPORT_TMP" 2>>"$ERROR_LOG" &
CLAUDE_PID=$!

( sleep $TIMEOUT_SECS && kill "$CLAUDE_PID" 2>/dev/null && touch "$TIMED_OUT_FLAG" ) &
WATCHDOG_PID=$!

wait "$CLAUDE_PID"
CLAUDE_EXIT=$?

# Kill watchdog if claude finished before timeout
kill "$WATCHDOG_PID" 2>/dev/null
wait "$WATCHDOG_PID" 2>/dev/null || true

if [ -f "$TIMED_OUT_FLAG" ]; then
    rm -f "$TIMED_OUT_FLAG"
    log_err "[introspection] ERROR: claude timed out after ${TIMEOUT_SECS}s"
    rm -f "$REPORT_TMP"
    exit 1
fi

if [ $CLAUDE_EXIT -ne 0 ]; then
    log_err "[introspection] ERROR: claude exited with code $CLAUDE_EXIT"
    rm -f "$REPORT_TMP"
    exit 1
fi

# Validate report has meaningful content (>50 non-whitespace chars)
CONTENT_LEN=$(tr -d '[:space:]' < "$REPORT_TMP" | wc -c | tr -d ' ')
if [ "$CONTENT_LEN" -lt 50 ]; then
    log_err "[introspection] ERROR: report has insufficient content ($CONTENT_LEN non-whitespace chars)"
    rm -f "$REPORT_TMP"
    exit 1
fi

mv "$REPORT_TMP" "$REPORT"

ISSUES=$(grep -c "CRITICAL\|WARNING" "$REPORT" 2>/dev/null || echo 0)
echo "[introspection] Report written: $REPORT ($ISSUES issues)"
