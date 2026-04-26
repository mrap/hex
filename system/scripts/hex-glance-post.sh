#!/usr/bin/env bash
set -uo pipefail

AGENT_DIR="${AGENT_DIR:-${AGENT_DIR:-$HOME/hex}}"
PROJECT_ID="${1:-}"
CHANNEL_ID="${2:-}"

if [[ -z "$PROJECT_ID" || -z "$CHANNEL_ID" ]]; then
  echo "Usage: hex-glance-post.sh <project-id> <channel-id>" >&2
  exit 1
fi

SECRETS_FILE="$AGENT_DIR/.hex/secrets/slack-bot.env"
if [[ ! -f "$SECRETS_FILE" ]]; then
  echo "ERROR: secrets file not found: $SECRETS_FILE" >&2
  exit 1
fi
# shellcheck source=/dev/null
source "$SECRETS_FILE"

PINS_DIR="$AGENT_DIR/.hex/runtime/glance-pins"
mkdir -p "$PINS_DIR"

PIN_FILE="$PINS_DIR/${CHANNEL_ID}.json"

# Get state JSON
STATE_JSON=$(AGENT_DIR="$AGENT_DIR" bash "$AGENT_DIR/.hex/scripts/hex-glance-derivation.sh" "$PROJECT_ID")

# Build Block Kit payload + decide post vs update, all in python
RESULT=$(SLACK_TOKEN="$MRAP_HEX_SLACK_BOT_TOKEN" python3 - \
  "$STATE_JSON" "$CHANNEL_ID" "$PIN_FILE" <<'PYEOF'
import json, os, sys, urllib.request, urllib.error
from datetime import datetime, timezone

state     = json.loads(sys.argv[1])
channel   = sys.argv[2]
pin_file  = sys.argv[3]
token     = os.environ["SLACK_TOKEN"]

now_utc = datetime.now(timezone.utc)
time_str = now_utc.strftime("%H:%M")

project  = state.get("project", "")
lands    = state.get("lands", [])
boi_cnt  = state.get("active_boi_count", 0)
last_dec = state.get("last_decision_date", "none")

blocks = [
    {"type": "header", "text": {"type": "plain_text", "text": f"\U0001f4cc Workstream Glance — {project} · {time_str}", "emoji": True}},
    {"type": "divider"},
]
for land in lands:
    holder = land.get("holder", "")
    lid    = land.get("id", "")
    title  = land.get("title", "")
    stt    = land.get("state", "")
    wt     = land.get("weekly_target", "")
    wt_part = f"  `{wt}`" if wt else ""
    blocks.append({
        "type": "context",
        "elements": [{"type": "mrkdwn", "text": f"{holder} *{lid}.* {title}  _{stt}_{wt_part}"}]
    })
blocks.append({"type": "divider"})
blocks.append({
    "type": "context",
    "elements": [{"type": "mrkdwn", "text": f"BOI in-flight: {boi_cnt}  Last decision: {last_dec}"}]
})

headers = {
    "Authorization": f"Bearer {token}",
    "Content-Type": "application/json",
}

def slack_call(url, payload):
    data = json.dumps(payload).encode()
    req  = urllib.request.Request(url, data=data, headers=headers, method="POST")
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read().decode())

# Decide: update or post
if os.path.exists(pin_file):
    with open(pin_file) as f:
        pin = json.load(f)
    existing_ts = pin.get("ts", "")
    payload = {"channel": channel, "ts": existing_ts, "blocks": blocks, "text": f"Workstream Glance — {project}"}
    resp = slack_call("https://slack.com/api/chat.update", payload)
    if not resp.get("ok"):
        print(json.dumps(resp), file=sys.stderr)
        sys.exit(1)
    ts = resp.get("ts", existing_ts)
    print(f"updated existing card ts={ts}")
else:
    payload = {"channel": channel, "blocks": blocks, "text": f"Workstream Glance — {project}"}
    resp = slack_call("https://slack.com/api/chat.postMessage", payload)
    if not resp.get("ok"):
        print(json.dumps(resp), file=sys.stderr)
        sys.exit(1)
    ts = resp["ts"]
    pin_data = {
        "ts": ts,
        "channel": channel,
        "project": project,
        "last_updated": now_utc.strftime("%Y-%m-%dT%H:%M:%SZ"),
    }
    tmp = pin_file + ".tmp"
    with open(tmp, "w") as f:
        json.dump(pin_data, f)
    os.rename(tmp, pin_file)
    print("posted new card")
PYEOF
)

echo "$RESULT"
