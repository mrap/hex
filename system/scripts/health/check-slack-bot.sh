#!/usr/bin/env bash
# check-slack-bot.sh — health check for the Slack bot integration.
# Healthy = secrets file present + HEX_SLACK_BOT_TOKEN set + Slack auth.test ok.
# Exit 0 = healthy, non-zero = unhealthy. Prints one-line status to stdout.
# Max runtime: 5 seconds (curl timeout 4s).

set -uo pipefail

NAME="slack-bot"
SECRETS_FILE="${HEX_DIR}/.hex/secrets/slack-bot.env"

# 1. Secrets file must exist and be non-empty.
if [[ ! -s "$SECRETS_FILE" ]]; then
  echo "$NAME: FAIL - secrets file missing or empty: $SECRETS_FILE"
  exit 1
fi

# shellcheck disable=SC1090
source "$SECRETS_FILE"

# 2. Token must be set.
if [[ -z "${HEX_SLACK_BOT_TOKEN:-}" ]]; then
  echo "$NAME: FAIL - HEX_SLACK_BOT_TOKEN not set in $SECRETS_FILE"
  exit 1
fi

# 3. Call Slack auth.test with a tight timeout to stay under 5s.
RESPONSE="$(curl -sf --max-time 4 \
  -H "Authorization: Bearer $HEX_SLACK_BOT_TOKEN" \
  https://slack.com/api/auth.test 2>/dev/null)" || {
  echo "$NAME: FAIL - curl to Slack API failed (timeout or network error)"
  exit 1
}

OK="$(printf '%s' "$RESPONSE" | python3 -c "import sys,json; d=json.load(sys.stdin); print('true' if d.get('ok') else 'false')" 2>/dev/null || echo "false")"

if [[ "$OK" != "true" ]]; then
  ERROR="$(printf '%s' "$RESPONSE" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('error','unknown'))" 2>/dev/null || echo "parse error")"
  echo "$NAME: FAIL - auth.test returned error: $ERROR"
  exit 1
fi

echo "$NAME: ok (auth.test passed)"
exit 0
