#!/usr/bin/env bash
# probe.sh — mcp-excalidraw health check
set -uo pipefail

BUNDLE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLAUDE_JSON="$HOME/.claude.json"
SECRETS_FILE="${AGENT_DIR}/.hex/secrets/excalidraw.env"
RESULT=0

emit_event() {
  local event="$1" status="$2" msg="$3"
  printf '{"event":"%s","status":"%s","message":"%s","ts":"%s"}\n' \
    "$event" "$status" "$msg" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >&2
}

echo "[mcp-excalidraw/probe] checking Excalidraw MCP..."

# 1. Check excalidraw MCP is configured in Claude settings
if [[ -f "$CLAUDE_JSON" ]]; then
  if python3 -c "
import json, sys
with open('$CLAUDE_JSON') as f:
    d = json.load(f)
text = json.dumps(d)
sys.exit(0 if 'excalidraw' in text.lower() else 1)
" 2>/dev/null; then
    echo "[mcp-excalidraw/probe] excalidraw MCP found in $CLAUDE_JSON"
  else
    echo "[mcp-excalidraw/probe] WARN: excalidraw not found in $CLAUDE_JSON" >&2
    RESULT=1
  fi
else
  echo "[mcp-excalidraw/probe] WARN: $CLAUDE_JSON not found" >&2
  RESULT=1
fi

# 2. Check secret env file has EXCALIDRAW_API_KEY set
if [[ -f "$SECRETS_FILE" ]]; then
  # shellcheck disable=SC1090
  set +u
  source "$SECRETS_FILE" 2>/dev/null || true
  set -u
  if [[ -n "${EXCALIDRAW_API_KEY:-}" ]]; then
    echo "[mcp-excalidraw/probe] EXCALIDRAW_API_KEY present (${#EXCALIDRAW_API_KEY} chars)"
  else
    echo "[mcp-excalidraw/probe] WARN: EXCALIDRAW_API_KEY empty in $SECRETS_FILE" >&2
    RESULT=1
  fi
else
  echo "[mcp-excalidraw/probe] WARN: secrets file not found: $SECRETS_FILE" >&2
  RESULT=1
fi

# 3. Light connectivity check to Excalidraw API (optional — failure is degraded, not fail)
if command -v curl &>/dev/null && [[ -n "${EXCALIDRAW_API_KEY:-}" ]]; then
  HTTP_STATUS="$(curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer ${EXCALIDRAW_API_KEY}" \
    --connect-timeout 5 --max-time 10 \
    "https://api.excalidraw.com/api/v2/workspaces/me" 2>/dev/null || echo "000")"
  if [[ "$HTTP_STATUS" == "200" ]]; then
    echo "[mcp-excalidraw/probe] workspace API: HTTP $HTTP_STATUS (ok)"
  elif [[ "$HTTP_STATUS" == "401" || "$HTTP_STATUS" == "403" ]]; then
    echo "[mcp-excalidraw/probe] WARN: workspace API: HTTP $HTTP_STATUS (auth error)" >&2
    RESULT=1
  else
    echo "[mcp-excalidraw/probe] WARN: workspace API: HTTP $HTTP_STATUS (degraded)" >&2
    RESULT=1
  fi
fi

if [[ "$RESULT" -eq 0 ]]; then
  emit_event "hex.integration.mcp-excalidraw.probe_ok" "ok" "config+token+api ok"
  echo "[mcp-excalidraw/probe] OK"
else
  emit_event "hex.integration.mcp-excalidraw.probe_fail" "fail" "one or more checks degraded"
  echo "[mcp-excalidraw/probe] DEGRADED (exit $RESULT)" >&2
fi

exit "$RESULT"
