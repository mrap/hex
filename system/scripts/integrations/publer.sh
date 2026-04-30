#!/usr/bin/env bash
# probe.sh — Publer integration health check
set -uo pipefail

HEX_ROOT="${HEX_ROOT:-${HEX_DIR}}"
SECRETS_FILE="$HEX_ROOT/.hex/secrets/publer.env"

[[ -f "$SECRETS_FILE" ]] && source "$SECRETS_FILE"

TIMEOUT=15

emit_event() {
  local event="$1" status="$2" msg="$3"
  printf '{"event":"%s","status":"%s","message":"%s","ts":"%s"}\n' \
    "$event" "$status" "$msg" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >&2
}

echo "[publer/probe] checking Publer API..."

HTTP_CODE=$(curl -s --max-time "$TIMEOUT" -o /dev/null -w "%{http_code}" \
  -H "Authorization: Bearer-API ${PUBLER_API_KEY:-}" \
  "https://app.publer.com/api/v1/workspaces" 2>/dev/null) || HTTP_CODE="000"

if [[ "$HTTP_CODE" =~ ^(200|401|403)$ ]]; then
  if [[ "$HTTP_CODE" == "200" ]]; then
    emit_event "hex.integration.publer.probe_ok" "ok" "HTTP $HTTP_CODE — authenticated"
    echo "[publer/probe] OK (HTTP $HTTP_CODE)"
  else
    emit_event "hex.integration.publer.probe_fail" "fail" "HTTP $HTTP_CODE — auth rejected"
    echo "[publer/probe] FAIL: HTTP $HTTP_CODE (bad credentials)" >&2
    exit 1
  fi
  exit 0
else
  emit_event "hex.integration.publer.probe_fail" "fail" "HTTP $HTTP_CODE — unreachable"
  echo "[publer/probe] FAIL: HTTP $HTTP_CODE" >&2
  exit 1
fi
