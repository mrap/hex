#!/usr/bin/env bash
# probe.sh — REPLACE_ME integration health check
# Replace the check logic below with your integration's actual probe.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# HEX_ROOT auto-derives from bundle location: integrations/<name>/probe.sh → instance root is ../../
HEX_ROOT="${HEX_ROOT:-$(cd "${SCRIPT_DIR}/../.." && pwd)}"
SECRETS_FILE="$HEX_ROOT/.hex/secrets/REPLACE_ME.env"

# ─── Load secrets ─────────────────────────────────────────────────────────────
[[ -f "$SECRETS_FILE" ]] && source "$SECRETS_FILE"

TIMEOUT=10

emit_event() {
  local event="$1" status="$2" msg="$3"
  printf '{"event":"%s","status":"%s","message":"%s","ts":"%s"}\n' \
    "$event" "$status" "$msg" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >&2
}

# ─── Probe logic (replace with real check) ────────────────────────────────────
echo "[REPLACE_ME/probe] checking..."

# Example: HTTP endpoint check
# HTTP_CODE=$(curl -sf --max-time "$TIMEOUT" -o /dev/null -w "%{http_code}" \
#   -H "Authorization: Bearer ${REPLACE_ME_API_KEY:-}" \
#   "https://api.example.com/health" 2>/dev/null) || HTTP_CODE="000"
#
# if [[ "$HTTP_CODE" == "200" ]]; then
#   emit_event "hex.integration.REPLACE_ME.probe_ok" "ok" "HTTP 200"
#   echo "[REPLACE_ME/probe] OK"
#   exit 0
# else
#   emit_event "hex.integration.REPLACE_ME.probe_fail" "fail" "HTTP $HTTP_CODE"
#   echo "[REPLACE_ME/probe] FAIL: HTTP $HTTP_CODE" >&2
#   exit 1
# fi

echo "[REPLACE_ME/probe] TODO: implement probe logic"
emit_event "hex.integration.REPLACE_ME.probe_ok" "ok" "stub"
echo "[REPLACE_ME/probe] OK (stub)"
exit 0
