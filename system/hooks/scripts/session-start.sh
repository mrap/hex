#!/usr/bin/env bash
# Hex SessionStart hook — channel-scoped context injection.
# Fires on every Claude Code SessionStart. When CC_SESSION_KEY is set (cc-connect
# Slack session), injects only the summary for that specific channel+user key.
# When CC_SESSION_KEY is unset (local dev), emits nothing and exits.
#
# TODO: Once a channel-name→topic map exists, surface projects/{channel-topic}/checkpoint.md
#       in addition to (or instead of) the raw summary file. Tracking: q-538 future task.

set -uo pipefail

HEX_DIR="${CLAUDE_PROJECT_DIR:-${HEX_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}}"

{
  _ch="${CC_SESSION_KEY:-local-dev}"
  _pid=$$
  _ts="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  _payload=$(printf '{"channel":"%s","agent":"claude-code","pid":%d,"start_ts":"%s"}' \
    "$_ch" "$_pid" "$_ts")
  bash "$HEX_DIR/.hex/bin/hex-emit.sh" "session.start" "$_payload" "claude-code"
} 2>/dev/null &

OVERDUE_FLAG="$HEX_DIR/okrs/_state/overdue.flag"
if [[ -f "$OVERDUE_FLAG" ]]; then
  python3 -c "
import json
detail = open('$OVERDUE_FLAG').read().strip()
msg = (
    '*** OKR REVIEW OVERDUE — ADDRESS BEFORE ANYTHING ELSE ***\n\n'
    + detail + '\n\n'
    'Surface this at the very top of your first response. Do not proceed with '
    'normal session startup until the review is completed or explicitly deferred.\n'
    'See okrs/README.md for the acknowledgment protocol.'
)
print(json.dumps({
    'hookSpecificOutput': {
        'hookEventName': 'SessionStart',
        'additionalContext': msg
    }
}))
"
  exit 0
fi

# Optional override for testability; default is the canonical summaries dir.
SUMMARIES_DIR="${HEX_SESSIONS_DIR:-$HEX_DIR/.hex/sessions/summaries}"

# No CC_SESSION_KEY → local dev session; no-op.
if [[ -z "${CC_SESSION_KEY:-}" ]]; then
  exit 0
fi

# Compute 16-char sha1 slug from the session key.
KEY=$(printf '%s' "$CC_SESSION_KEY" | shasum -a 1 | head -c 16)
SUMMARY_FILE="${SUMMARIES_DIR}/${KEY}.md"

# No summary for this channel yet → blank slate.
if [[ ! -f "$SUMMARY_FILE" ]]; then
  exit 0
fi

# Read summary; on any read error, stay silent (never inject foreign content).
CONTENT=$(cat "$SUMMARY_FILE" 2>/dev/null) || exit 0

# Emit the Claude Code SessionStart JSON contract.
# additionalContext injects content into the session system prompt.
printf '%s' "$CONTENT" | python3 -c "
import sys, json
content = sys.stdin.read()
payload = {
    'hookSpecificOutput': {
        'hookEventName': 'SessionStart',
        'additionalContext': content
    }
}
print(json.dumps(payload))
"
