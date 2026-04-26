#!/usr/bin/env bash
# slack-post.sh — Post a message to a Slack channel via chat.postMessage
# Usage: bash slack-post.sh --channel <channel_id> --text <message>
set -uo pipefail

SECRETS_FILE="$(dirname "$0")/../secrets/slack-bot.env"
if [[ ! -f "$SECRETS_FILE" ]]; then
  echo "[slack-post] ERROR: secrets file not found: $SECRETS_FILE" >&2
  exit 1
fi

# shellcheck source=/dev/null
source "$SECRETS_FILE"

CHANNEL=""
TEXT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --channel) CHANNEL="$2"; shift 2 ;;
    --text)    TEXT="$2";    shift 2 ;;
    *) echo "[slack-post] ERROR: unknown arg: $1" >&2; exit 1 ;;
  esac
done

if [[ -z "$CHANNEL" || -z "$TEXT" ]]; then
  echo "[slack-post] ERROR: --channel and --text are required" >&2
  exit 1
fi

RESPONSE=$(curl -s -X POST "https://slack.com/api/chat.postMessage" \
  -H "Authorization: Bearer ${MRAP_HEX_SLACK_BOT_TOKEN}" \
  -H "Content-Type: application/json" \
  --data "$(python3 -c "
import json, sys
print(json.dumps({'channel': sys.argv[1], 'text': sys.argv[2]}))
" "$CHANNEL" "$TEXT")")

OK=$(python3 -c "import json,sys; d=json.loads(sys.argv[1]); print(d.get('ok','false'))" "$RESPONSE" 2>/dev/null)

if [[ "$OK" != "True" && "$OK" != "true" ]]; then
  ERR=$(python3 -c "import json,sys; d=json.loads(sys.argv[1]); print(d.get('error','unknown'))" "$RESPONSE" 2>/dev/null)
  echo "[slack-post] ERROR: Slack API error: $ERR" >&2
  exit 1
fi

echo "[slack-post] ok channel=$CHANNEL"
