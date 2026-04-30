#!/usr/bin/env bash
# slack-bot.sh — health check for the Slack bot integration.
#
# Healthy = token present + auth.test returns ok:true.
# Exit 0 = healthy, 1 = unhealthy.  Failure reason written to stderr.

set -uo pipefail

INTEGRATION="slack-bot"
SECRETS_FILE="${AGENT_DIR}/.hex/secrets/slack-bot.env"

# 1. Secrets file must exist and be non-empty.
if [[ ! -s "$SECRETS_FILE" ]]; then
  echo "$INTEGRATION: secrets file missing or empty: $SECRETS_FILE" >&2
  exit 1
fi

# shellcheck disable=SC1090
source "$SECRETS_FILE"

# 2. Token must be set (actual var name in slack-bot.env is HEX_SLACK_BOT_TOKEN).
if [[ -z "${HEX_SLACK_BOT_TOKEN:-}" ]]; then
  echo "$INTEGRATION: HEX_SLACK_BOT_TOKEN not set in $SECRETS_FILE" >&2
  exit 1
fi

# 3. Call auth.test and verify ok:true.
RESPONSE="$(curl -sf --max-time 10 \
  -H "Authorization: Bearer $HEX_SLACK_BOT_TOKEN" \
  https://slack.com/api/auth.test 2>&1)" || {
  echo "$INTEGRATION: curl failed (timeout or network error)" >&2
  exit 1
}

OK="$(echo "$RESPONSE" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('ok',''))" 2>/dev/null)"

if [[ "$OK" != "True" ]]; then
  ERROR="$(echo "$RESPONSE" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('error','unknown'))" 2>/dev/null || echo "parse error")"
  echo "$INTEGRATION: auth.test failed: $ERROR" >&2
  exit 1
fi

echo "$INTEGRATION: ok" >&2
exit 0
