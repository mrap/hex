#!/usr/bin/env bash
# probe.sh — Kalshi integration health check
# Two-legged: public /exchange/status + signed /portfolio/balance
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SECRETS_FILE="${HEX_SECRETS_DIR:-.hex/secrets}/kalshi.env"
HEX_ROOT="${HEX_ROOT:-${AGENT_DIR}}"

# ─── Load secrets ─────────────────────────────────────────────────────────────
ENV_FILE="$HEX_ROOT/$SECRETS_FILE"
if [[ -f "$ENV_FILE" ]]; then
  # shellcheck source=/dev/null
  source "$ENV_FILE"
fi

KALSHI_ENV="${KALSHI_ENV:-prod}"
if [[ "$KALSHI_ENV" == "demo" ]]; then
  BASE_URL="https://demo-api.kalshi.co/trade-api/v2"
else
  BASE_URL="https://api.elections.kalshi.com/trade-api/v2"
fi

TIMEOUT=10
EXIT_CODE=0

emit_event() {
  local event="$1" status="$2" msg="$3"
  printf '{"event":"%s","status":"%s","message":"%s","ts":"%s"}\n' \
    "$event" "$status" "$msg" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >&2
}

# ─── Leg 1: Public /exchange/status ──────────────────────────────────────────
echo "[kalshi/probe] leg1: GET /exchange/status"
STATUS_RESP=$(curl -sf --max-time "$TIMEOUT" \
  -H "Accept: application/json" \
  "$BASE_URL/exchange/status" 2>/dev/null) || {
  emit_event "hex.integration.kalshi.probe_fail" "fail" "leg1: curl failed"
  echo "[kalshi/probe] FAIL: could not reach $BASE_URL/exchange/status" >&2
  exit 1
}

EXCHANGE_ACTIVE=$(echo "$STATUS_RESP" | python3 -c \
  'import json,sys; d=json.load(sys.stdin); print(d.get("exchange_active","false"))' 2>/dev/null || echo "false")

if [[ "$EXCHANGE_ACTIVE" != "True" && "$EXCHANGE_ACTIVE" != "true" ]]; then
  emit_event "hex.integration.kalshi.probe_fail" "fail" "leg1: exchange_active=$EXCHANGE_ACTIVE"
  echo "[kalshi/probe] FAIL: exchange not active (response: $STATUS_RESP)" >&2
  EXIT_CODE=1
else
  echo "[kalshi/probe] leg1: OK (exchange_active=$EXCHANGE_ACTIVE)"
fi

# ─── Leg 2: Signed /portfolio/balance ────────────────────────────────────────
# Skip if secrets not present (probe degrades gracefully to leg1-only)
if [[ -z "${KALSHI_KEY_ID:-}" || -z "${KALSHI_PRIVATE_KEY_PATH:-}" ]]; then
  echo "[kalshi/probe] leg2: SKIP (no credentials configured)"
  if [[ $EXIT_CODE -eq 0 ]]; then
    emit_event "hex.integration.kalshi.probe_ok" "ok" "leg1 only (no credentials)"
    echo "[kalshi/probe] OK (leg1 only)"
  fi
  exit $EXIT_CODE
fi

# Check key file exists and is readable
if [[ ! -f "$KALSHI_PRIVATE_KEY_PATH" ]]; then
  emit_event "hex.integration.kalshi.probe_fail" "fail" "leg2: key file not found: $KALSHI_PRIVATE_KEY_PATH"
  echo "[kalshi/probe] FAIL: private key not found at $KALSHI_PRIVATE_KEY_PATH" >&2
  exit 1
fi

python3 -c "import cryptography" 2>/dev/null || {
  echo "ERROR: 'cryptography' package required. Run: pip install cryptography" >&2
  exit 1
}

echo "[kalshi/probe] leg2: signed GET /portfolio/balance"
TIMESTAMP_MS=$(python3 -c 'import time; print(int(time.time()*1000))')
METHOD="GET"
PATH_ONLY="/trade-api/v2/portfolio/balance"

SIG=$(python3 "$SCRIPT_DIR/lib/kalshi_sign.py" \
  --key "$KALSHI_PRIVATE_KEY_PATH" \
  --timestamp "$TIMESTAMP_MS" \
  --method "$METHOD" \
  --path "$PATH_ONLY" 2>/dev/null) || {
  emit_event "hex.integration.kalshi.probe_fail" "fail" "leg2: signing failed"
  echo "[kalshi/probe] FAIL: RSA signing error" >&2
  exit 1
}

HTTP_CODE=$(curl -sf --max-time "$TIMEOUT" -o /dev/null -w "%{http_code}" \
  -H "Accept: application/json" \
  -H "KALSHI-ACCESS-KEY: $KALSHI_KEY_ID" \
  -H "KALSHI-ACCESS-TIMESTAMP: $TIMESTAMP_MS" \
  -H "KALSHI-ACCESS-SIGNATURE: $SIG" \
  "$BASE_URL/portfolio/balance" 2>/dev/null) || HTTP_CODE="000"

if [[ "$HTTP_CODE" == "200" ]]; then
  echo "[kalshi/probe] leg2: OK (HTTP $HTTP_CODE)"
elif [[ "$HTTP_CODE" == "401" ]]; then
  emit_event "hex.integration.kalshi.probe_fail" "fail" "leg2: 401 Unauthorized — check key ID, clock skew, or revocation"
  echo "[kalshi/probe] FAIL: 401 — check KALSHI_KEY_ID, clock skew (<5s), or key revocation" >&2
  EXIT_CODE=1
else
  emit_event "hex.integration.kalshi.probe_fail" "fail" "leg2: HTTP $HTTP_CODE"
  echo "[kalshi/probe] FAIL: HTTP $HTTP_CODE from /portfolio/balance" >&2
  EXIT_CODE=1
fi

if [[ $EXIT_CODE -eq 0 ]]; then
  emit_event "hex.integration.kalshi.probe_ok" "ok" "both legs passed"
  echo "[kalshi/probe] OK"
fi

exit $EXIT_CODE
