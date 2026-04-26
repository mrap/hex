#!/usr/bin/env bash
# hex-ui-feedback-tick.sh
#
# Runs every 3 minutes via crontab. Checks the hex-ui comments API
# for comments with status=new. If any exist, invokes Claude headless
# to read them, make the requested changes to the demos page, post
# a reply, and mark built.
#
# Guardrails live in the prompt file: hex-ui-feedback-loop-prompt.txt

set -uo pipefail

PATH="/Users/mrap/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
export PATH
CLAUDE_BIN="/Users/mrap/.local/bin/claude"

# Load API key so headless `claude -p` can auth under cron (no interactive login available)
if [ -f "/Users/mrap/.hex-test.env" ]; then
  set -a
  . "/Users/mrap/.hex-test.env"
  set +a
fi

API="https://mac-mini.tailbd5748.ts.net/visions/api/comments"
LOG="/tmp/hex-ui-feedback-loop.log"
LOCK="/tmp/hex-ui-feedback-loop.lock"
PROMPT_FILE="/Users/mrap/mrap-hex/.hex/scripts/hex-ui-feedback-loop-prompt.txt"
PROJECT_DIR="/Users/mrap/mrap-hex"

# Simple lock so concurrent ticks don't race
if [ -e "$LOCK" ]; then
  LOCK_PID=$(cat "$LOCK" 2>/dev/null)
  if [ -n "$LOCK_PID" ] && kill -0 "$LOCK_PID" 2>/dev/null; then
    echo "=== $(date) === tick skipped (lock held by pid $LOCK_PID)" >> "$LOG"
    exit 0
  fi
fi
echo $$ > "$LOCK"
trap 'rm -f "$LOCK"' EXIT

NEW_COUNT=$(curl -sk --max-time 5 "$API" 2>/dev/null | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    print(len([c for c in d.get('comments', []) if c.get('status') == 'new']))
except Exception:
    print(0)
" 2>/dev/null)
NEW_COUNT=${NEW_COUNT:-0}

if [ "$NEW_COUNT" = "0" ]; then
  # no work; keep log tidy by only stamping every ~hour when idle
  MIN=$(date +%M)
  if [ "$MIN" = "00" ]; then
    echo "=== $(date) === idle (no new comments)" >> "$LOG"
  fi
  exit 0
fi

echo "=== $(date) === $NEW_COUNT new comment(s) — spawning claude" >> "$LOG"

cd "$PROJECT_DIR" || exit 1

if [ ! -f "$PROMPT_FILE" ]; then
  echo "ERROR: missing prompt file at $PROMPT_FILE" >> "$LOG"
  exit 1
fi

PROMPT=$(cat "$PROMPT_FILE")

# Headless one-shot. bypass permissions = full autonomy within the guardrails in the prompt.
# Portable timeout: launch in background, kill after 480s. macOS has no timeout binary.
"$CLAUDE_BIN" -p "$PROMPT" --permission-mode bypassPermissions --output-format text >> "$LOG" 2>&1 &
CLAUDE_PID=$!
(
  sleep 480
  if kill -0 "$CLAUDE_PID" 2>/dev/null; then
    echo "=== $(date) === claude still running after 480s — killing pid=$CLAUDE_PID" >> "$LOG"
    kill -TERM "$CLAUDE_PID" 2>/dev/null
    sleep 2
    kill -KILL "$CLAUDE_PID" 2>/dev/null
  fi
) &
WATCHDOG_PID=$!
wait "$CLAUDE_PID" 2>/dev/null
RC=$?
kill "$WATCHDOG_PID" 2>/dev/null
echo "=== $(date) === claude exited rc=$RC" >> "$LOG"
