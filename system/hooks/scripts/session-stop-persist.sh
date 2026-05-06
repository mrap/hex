#!/usr/bin/env bash
# Hex Stop hook — persists per-session summary keyed by CC_SESSION_KEY.
# Runs at every Claude Code Stop event for the hex instance.
# When CC_SESSION_KEY is set (cc-connect Slack session), extracts the latest
# session summary for this worktree and writes a channel-scoped copy so
# that session-start.sh can inject it on the next session for this channel only.
#
# Summary source priority:
#   1. Non-JSON stdin content (test/simulation mode — t-6 smoke test pipes here)
#   2. Most recent *-session.tmp file whose **Worktree:** matches HEX_DIR
# If neither is available, exits silently (no-op).

set -uo pipefail

HEX_DIR="${CLAUDE_PROJECT_DIR:-${HEX_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}}"

{
  _ch="${CC_SESSION_KEY:-local-dev}"
  _ts="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  _payload=$(printf '{"channel":"%s","stop_ts":"%s","stop_reason":"hook"}' "$_ch" "$_ts")
  bash "$HEX_DIR/.hex/bin/hex-emit.sh" "session.stop" "$_payload" "claude-code"
} 2>/dev/null &

SUMMARIES_DIR="${HEX_SESSIONS_DIR:-$HEX_DIR/.hex/sessions/summaries}"
SESSION_DATA_DIR="${CLAUDE_SESSION_DATA_DIR:-}"
if [[ -z "$SESSION_DATA_DIR" ]]; then
  for _candidate in "$HOME/.claude/session-data" "$HOME/.codex/session-data"; do
    [[ -d "$_candidate" ]] && SESSION_DATA_DIR="$_candidate" && break
  done
fi
LOG_FILE="$HEX_DIR/.hex/hooks/logs/session-stop.log"
WORKTREE_PATH="$HEX_DIR"
MAX_BYTES=4096

# No CC_SESSION_KEY → local dev session; no-op.
if [[ -z "${CC_SESSION_KEY:-}" ]]; then
  exit 0
fi

# Compute 16-char sha1 slug from the session key (same algo as session-start.sh).
KEY=$(printf '%s' "$CC_SESSION_KEY" | shasum -a 1 | head -c 16)
SUMMARY_FILE="${SUMMARIES_DIR}/${KEY}.md"

mkdir -p "$SUMMARIES_DIR" "$(dirname "$LOG_FILE")"

SUMMARY=""

# --- Source 1: stdin (non-JSON piped content; used by t-6 smoke tests) ---
# Claude Code's Stop hook runtime pipes a JSON payload; discard it (starts with '{').
# Plain-text stdin (e.g. from test harnesses) is treated as the raw summary.
if [[ ! -t 0 ]]; then
  STDIN_CONTENT=$(cat 2>/dev/null) || STDIN_CONTENT=""
  if [[ -n "$STDIN_CONTENT" ]] && [[ "${STDIN_CONTENT:0:1}" != "{" ]]; then
    SUMMARY="$STDIN_CONTENT"
  fi
fi

# --- Source 2: session .tmp file for this worktree ---
if [[ -z "$SUMMARY" ]]; then
  ECC_SESSION_FILE=""
  # ls -t gives newest-first; we want the first match for our worktree.
  while IFS= read -r fname; do
    fpath="${SESSION_DATA_DIR}/${fname}"
    [[ -f "$fpath" ]] || continue
    if grep -q "^\*\*Worktree:\*\* ${WORKTREE_PATH}$" "$fpath" 2>/dev/null; then
      ECC_SESSION_FILE="$fpath"
      break
    fi
  done < <(ls -t "$SESSION_DATA_DIR" 2>/dev/null | grep -E '.*-session\.tmp$')

  if [[ -n "$ECC_SESSION_FILE" ]]; then
    SUMMARY=$(python3 - "$ECC_SESSION_FILE" <<'PYEOF'
import sys, re
path = sys.argv[1]
try:
    text = open(path).read()
    m = re.search(r'<!-- HEX:SUMMARY:START -->(.*?)<!-- HEX:SUMMARY:END -->', text, re.DOTALL)
    if m:
        print(m.group(1).strip())
except Exception:
    pass
PYEOF
) || SUMMARY=""
  fi
fi

# Nothing to persist.
if [[ -z "$SUMMARY" ]]; then
  printf '%s [session-stop] key=%s... no summary found, skipped\n' \
    "$(date '+%Y-%m-%dT%H:%M:%S')" "${KEY:0:8}" >> "$LOG_FILE" 2>/dev/null || true
  exit 0
fi

# Cap at ~4 KB; keep the tail (most recent content) if truncating.
if [[ ${#SUMMARY} -gt $MAX_BYTES ]]; then
  SUMMARY="${SUMMARY: -$MAX_BYTES}"
fi

# Atomic write: temp file → mv.
TMPFILE="${SUMMARIES_DIR}/.${KEY}.tmp.$$"
printf '%s\n' "$SUMMARY" > "$TMPFILE" && mv "$TMPFILE" "$SUMMARY_FILE"
BYTES="${#SUMMARY}"

printf '%s [session-stop] key=%s... wrote %d bytes to %s\n' \
  "$(date '+%Y-%m-%dT%H:%M:%S')" "${KEY:0:8}" "$BYTES" "$SUMMARY_FILE" \
  >> "$LOG_FILE" 2>/dev/null || true

exit 0
