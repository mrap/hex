#!/usr/bin/env bash
# probe.sh — secrets-pipeline health check.
#
# Healthy = all required secret files exist and are non-empty;
#           slack-bot.env contains a non-empty HEX_SLACK_BOT_TOKEN.
# Exit 0 = ok, Exit 1 = fail.

set -uo pipefail

HEX_ROOT="${HEX_ROOT:-${AGENT_DIR}}"
SECRETS_DIR="$HEX_ROOT/.hex/secrets"

REQUIRED_FILES=(
  "slack-bot.env"
  "x-api.env"
  "fal.env"
  "openrouter.env"
  "excalidraw.env"
)

echo "[secrets-pipeline/probe] checking secret files..."

for fname in "${REQUIRED_FILES[@]}"; do
  fpath="$SECRETS_DIR/$fname"
  if [[ ! -s "$fpath" ]]; then
    echo "FAIL: secret file missing or empty: $fpath" >&2
    exit 1
  fi
done

# Spot-check: slack-bot.env must contain a non-empty HEX_SLACK_BOT_TOKEN.
# shellcheck disable=SC1090
source "$SECRETS_DIR/slack-bot.env"
if [[ -z "${HEX_SLACK_BOT_TOKEN:-}" ]]; then
  echo "FAIL: HEX_SLACK_BOT_TOKEN is empty in slack-bot.env" >&2
  exit 1
fi

echo "[secrets-pipeline/probe] ok (all ${#REQUIRED_FILES[@]} secret files present and non-empty)"
exit 0
