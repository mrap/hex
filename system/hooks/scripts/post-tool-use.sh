#!/usr/bin/env bash
# Hex PostToolUse hook — records tool usage events into the telemetry store.
# Skips high-volume core tools (Read, Edit, Write, Bash, Grep, Glob) since
# logging "Bash success" thousands of times is pure noise. Only tracks MCP,
# Agent, and other tools where usage patterns are worth analyzing.

set -uo pipefail

HEX_DIR="${CLAUDE_PROJECT_DIR:-${HEX_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}}"
HEX_EMIT="$HEX_DIR/.hex/bin/hex-emit.sh"
LOG_DIR="$HEX_DIR/.hex/hooks/logs"
mkdir -p "$LOG_DIR"

HOOK_PAYLOAD=""
if [[ ! -t 0 ]]; then
  HOOK_PAYLOAD=$(cat 2>/dev/null) || HOOK_PAYLOAD=""
fi

[[ -z "$HOOK_PAYLOAD" ]] && exit 0

read -r TOOL_NAME OUTCOME < <(printf '%s' "$HOOK_PAYLOAD" | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
    tool_name = d.get('tool_name', 'unknown')
    resp = d.get('tool_response', {})
    if isinstance(resp, dict) and resp.get('is_error'):
        outcome = 'error'
    elif isinstance(resp, str) and 'error' in resp.lower():
        outcome = 'error'
    else:
        outcome = 'success'
    print(tool_name, outcome)
except Exception:
    print('unknown success')
" 2>/dev/null) || { TOOL_NAME="unknown"; OUTCOME="unknown"; }

# Skip high-volume core tools — no signal in logging these
case "$TOOL_NAME" in
  Read|Edit|Write|Bash|Grep|Glob|MultiEdit|TodoRead|TodoWrite) exit 0 ;;
esac

{
  _payload=$(printf '{"tool_name":"%s","outcome":"%s"}' "$TOOL_NAME" "$OUTCOME")
  bash "$HEX_EMIT" "tool.post_use" "$_payload" "claude-code"
} 2>>"$LOG_DIR/post-tool-use.log" &

exit 0
