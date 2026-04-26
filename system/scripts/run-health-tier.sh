#!/usr/bin/env bash
# run-health-tier.sh — run health/check-*.sh scripts for a tier, emit integrations.health.* events
# Usage: run-health-tier.sh <critical|important|standard>
# Exit:  0 = all healthy, 1 = any failed

set -uo pipefail

TIER="${1:-}"
HEALTH_DIR="${AGENT_DIR:-$HOME/hex}/.hex/scripts/health"
HEX_EMIT="python3 $HOME/.hex-events/hex_emit.py"

case "$TIER" in
  critical)  NAMES="check-cc-connect check-slack-bot check-hex-eventd" ;;
  important) NAMES="check-mcp-servers check-secrets check-tailscale" ;;
  standard)  NAMES="check-kalshi" ;;
  *)
    echo "Usage: $(basename "$0") <critical|important|standard>" >&2
    exit 1
    ;;
esac

OVERALL=0

for NAME in $NAMES; do
  SCRIPT="$HEALTH_DIR/${NAME}.sh"
  if [[ ! -f "$SCRIPT" ]]; then
    echo "[WARN] health script not found: $SCRIPT, skipping" >&2
    continue
  fi

  set +e
  if command -v timeout &>/dev/null; then
    OUTPUT="$(timeout 10 bash "$SCRIPT" 2>&1)"
  else
    OUTPUT="$(perl -e 'alarm(10); exec @ARGV' -- bash "$SCRIPT" 2>&1)"
  fi
  EXIT_CODE=$?
  set -e

  PAYLOAD="$(CHECK="$NAME" SCRIPT_PATH="$SCRIPT" OUT="$OUTPUT" EXIT="$EXIT_CODE" python3 -c "
import json, os
payload = {
    'integration': os.environ['CHECK'],
    'check': os.environ['SCRIPT_PATH'],
    'output': os.environ.get('OUT', ''),
    'exit_code': int(os.environ['EXIT']),
}
print(json.dumps(payload))
")"

  if [[ $EXIT_CODE -eq 0 ]]; then
    $HEX_EMIT "integrations.health.ok" "$PAYLOAD" "hex:integrations-health-monitor" 2>/dev/null || true
  else
    $HEX_EMIT "integrations.health.failed" "$PAYLOAD" "hex:integrations-health-monitor" 2>/dev/null || true
    OVERALL=1
  fi
done

exit $OVERALL
