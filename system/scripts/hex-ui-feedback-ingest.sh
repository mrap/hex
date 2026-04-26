#!/usr/bin/env bash
# hex-ui-feedback-ingest.sh
# Reads hex-ui messages.json, extracts unprocessed "done" messages,
# appends structured entries to the feedback log, and marks them processed.

set -uo pipefail

MESSAGES_JSON="/Users/mrap/github.com/mrap/hex-ui/backend/state/messages.json"
PROCESSED_JSON="/Users/mrap/mrap-hex/.hex/state/hex-ui-processed-messages.json"
FEEDBACK_LOG="/Users/mrap/mrap-hex/projects/hex-ui/feedback/ui-feedback-log.md"

# Ensure state dir exists
mkdir -p "$(dirname "$PROCESSED_JSON")"
mkdir -p "$(dirname "$FEEDBACK_LOG")"

# Run the ingestion via Python (stdlib only)
MESSAGES_JSON="$MESSAGES_JSON" PROCESSED_JSON="$PROCESSED_JSON" FEEDBACK_LOG="$FEEDBACK_LOG" python3 - <<'PYEOF'
import json
import os
import sys
from datetime import datetime, timezone

messages_path = os.environ["MESSAGES_JSON"]
processed_path = os.environ["PROCESSED_JSON"]
feedback_log_path = os.environ["FEEDBACK_LOG"]

# Load messages
try:
    with open(messages_path) as f:
        messages = json.load(f)
except Exception as e:
    print(f"ERROR: could not read messages: {e}", file=sys.stderr)
    sys.exit(1)

# Normalize: unwrap dict wrapper if present (e.g., {"messages": [...]})
if isinstance(messages, dict):
    if "messages" in messages:
        messages = messages["messages"]
    else:
        print(f"ERROR: messages.json is a dict without a 'messages' key; got keys: {list(messages.keys())}", file=sys.stderr)
        sys.exit(1)
if not isinstance(messages, list):
    print(f"ERROR: messages.json must be a JSON array (or dict with 'messages' key), got {type(messages).__name__}", file=sys.stderr)
    sys.exit(1)

# Load processed IDs
processed_ids = set()
try:
    with open(processed_path) as f:
        data = json.load(f)
        processed_ids = set(data.get("processed_ids", []))
except FileNotFoundError:
    processed_ids = set()
except Exception as e:
    print(f"ERROR: corrupt processed-IDs state file ({processed_path}): {e}", file=sys.stderr)
    sys.exit(1)

# Filter: done messages not yet processed
new_messages = [
    m for m in messages
    if m.get("status") == "done" and m.get("id") not in processed_ids
]

if not new_messages:
    print("No new messages to process.")
    sys.exit(0)

print(f"Processing {len(new_messages)} new message(s)...")

# Append to feedback log
with open(feedback_log_path, "a") as log:
    for msg in new_messages:
        ts = datetime.fromtimestamp(
            msg.get("created_at", 0), tz=timezone.utc
        ).strftime("%Y-%m-%d %H:%M:%S UTC")
        text = msg.get("text", "").replace("\n", "\n  ")
        thread_id = msg.get("thread_id", "unknown")
        status = msg.get("status", "unknown")
        log.write(f"\n### [{ts}] Feedback\n")
        log.write(f"**Message:** {text}\n")
        log.write(f"**Thread:** {thread_id}\n")
        log.write(f"**Status:** {status}\n")
        processed_ids.add(msg["id"])
        print(f"  - [{ts}] {text[:60]}...")

# Save updated processed IDs (write to tmp then mv for atomicity)
tmp_path = processed_path + ".tmp"
with open(tmp_path, "w") as f:
    json.dump({"processed_ids": sorted(processed_ids)}, f, indent=2)
os.replace(tmp_path, processed_path)

print(f"Done. {len(new_messages)} message(s) written to feedback log.")
PYEOF
