#!/usr/bin/env bash
# check-message-roundtrip.sh — end-to-end: send message → wake agent → verify delivered
# Validates the mark_delivered flow wired in T5505 (wake.rs wake-end).
# Exit 0 if all checks pass; exit 1 with specific failure on stderr.
#
# Targets the `health-probe` agent (charter wake.skip_llm: true) so the wake
# bypasses Claude entirely — fast (<1s) and free. The check still exercises
# inbox routing, mark_delivered, state save, and audit emission.

set -uo pipefail

HEALTH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$HEALTH_DIR/.." && pwd)"
DOT_HEX="$(cd "$SCRIPTS_DIR/.." && pwd)"
HEX="$DOT_HEX/bin/hex"
MESSAGES_JSON="$DOT_HEX/data/messages.json"
AUDIT_JSONL="$DOT_HEX/audit/actions.jsonl"
PROBE_STATE_JSON="$(cd "$SCRIPTS_DIR/../.." && pwd)/projects/health-probe/state.json"

FAIL=0

# ── Preflight ─────────────────────────────────────────────────────────────────
# state.json is created by the harness on first wake — only require the files
# the harness can't bootstrap on its own.
for f in "$MESSAGES_JSON" "$AUDIT_JSONL"; do
  if [[ ! -f "$f" ]]; then
    echo "check-message-roundtrip: FAIL — required file missing: $f" >&2
    exit 1
  fi
done
PROBE_DIR="$(dirname "$PROBE_STATE_JSON")"
if [[ ! -d "$PROBE_DIR" ]]; then
  echo "check-message-roundtrip: FAIL — probe agent dir missing: $PROBE_DIR" >&2
  exit 1
fi
if [[ ! -f "$PROBE_DIR/charter.yaml" ]]; then
  echo "check-message-roundtrip: FAIL — probe charter missing: $PROBE_DIR/charter.yaml" >&2
  exit 1
fi
if [[ ! -x "$HEX" ]]; then
  echo "check-message-roundtrip: FAIL — hex binary not executable: $HEX" >&2
  exit 1
fi

# ── 1. Generate unique synthetic tag ─────────────────────────────────────────
TAG="$(uuidgen 2>/dev/null || date +%s$$)"
SUBJECT="ROUNDTRIP-$TAG"

# Record test start time before sending (ISO8601 UTC)
TEST_START_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# ── 2. Send message, capture ID ───────────────────────────────────────────────
MSG_OUT=$("$HEX" agent message cos health-probe \
  --subject "$SUBJECT" \
  --body "health-check synthetic test — safe to ignore, ROUNDTRIP-$TAG" \
  2>&1) || {
  echo "check-message-roundtrip: FAIL — hex agent message failed: $MSG_OUT" >&2
  exit 1
}

# Output: "Sent message M<hex> (agent) from cos to health-probe"
MSG_ID=$(echo "$MSG_OUT" | grep -oE 'M[0-9a-f]+' | head -1)
if [[ -z "$MSG_ID" ]]; then
  echo "check-message-roundtrip: FAIL — could not extract message ID from: $MSG_OUT" >&2
  exit 1
fi

# ── 3. Wake health-probe ─────────────────────────────────────────────────────
# Use gtimeout (macOS coreutils) if available, else run without timeout
if command -v gtimeout &>/dev/null; then
  TIMEOUT_WRAP="gtimeout 180"
elif command -v timeout &>/dev/null; then
  TIMEOUT_WRAP="timeout 180"
else
  TIMEOUT_WRAP=""
fi

WAKE_OUT=$(${TIMEOUT_WRAP} "$HEX" agent wake health-probe --trigger inbox.message 2>&1)
WAKE_EXIT=$?
if [[ $WAKE_EXIT -eq 124 ]]; then
  echo "check-message-roundtrip: FAIL — hex agent wake timed out after 180s" >&2
  FAIL=1
elif [[ $WAKE_EXIT -ne 0 ]]; then
  echo "check-message-roundtrip: FAIL — hex agent wake exited $WAKE_EXIT: $WAKE_OUT" >&2
  FAIL=1
fi

# ── 4a. Verify: messages.json status=delivered, routed_to contains health-probe ──
VERIFY_A=$(python3 - "$MESSAGES_JSON" "$MSG_ID" <<'PYEOF'
import json, sys
path, msg_id = sys.argv[1], sys.argv[2]
try:
    with open(path) as f:
        data = json.load(f)
except Exception as e:
    print(f"fail:cannot read messages.json: {e}")
    sys.exit(0)
for m in data.get('messages', []):
    if m.get('id') == msg_id:
        status = m.get('status', '')
        routed = m.get('routed_to', [])
        if status == 'delivered' and 'health-probe' in routed:
            print('ok')
        elif status != 'delivered':
            print(f"fail:status={status}")
        else:
            print(f"fail:routed_to={routed}")
        sys.exit(0)
print(f"fail:message {msg_id} not found in messages.json")
PYEOF
) || VERIFY_A="fail:python3 error"

if [[ "$VERIFY_A" != "ok" ]]; then
  echo "check-message-roundtrip: FAIL (a) messages.json — $VERIFY_A (msg $MSG_ID)" >&2
  FAIL=1
fi

# ── 4b. Verify: health-probe state.json last_wake >= test start ──────────────
VERIFY_B=$(python3 - "$PROBE_STATE_JSON" "$TEST_START_TS" <<'PYEOF'
import json, sys
from datetime import datetime, timezone
path, test_start_str = sys.argv[1], sys.argv[2]
try:
    with open(path) as f:
        data = json.load(f)
except Exception as e:
    print(f"fail:cannot read state.json: {e}")
    sys.exit(0)
last_wake_str = data.get('last_wake', '')
if not last_wake_str:
    print("fail:last_wake field missing in state.json")
    sys.exit(0)
try:
    test_start = datetime.fromisoformat(test_start_str.replace('Z', '+00:00'))
    last_wake = datetime.fromisoformat(last_wake_str.replace('Z', '+00:00'))
except ValueError as e:
    print(f"fail:timestamp parse error: {e}")
    sys.exit(0)
if last_wake >= test_start:
    print('ok')
else:
    print(f"fail:last_wake={last_wake_str} predates test start {test_start_str}")
PYEOF
) || VERIFY_B="fail:python3 error"

if [[ "$VERIFY_B" != "ok" ]]; then
  echo "check-message-roundtrip: FAIL (b) state.json — $VERIFY_B" >&2
  FAIL=1
fi

# ── 4c. Verify: audit/actions.jsonl wake-start for health-probe with inbox_items >= 1 ──
VERIFY_C=$(python3 - "$AUDIT_JSONL" "$TEST_START_TS" <<'PYEOF'
import json, sys
from datetime import datetime, timezone
path, test_start_str = sys.argv[1], sys.argv[2]
test_start = datetime.fromisoformat(test_start_str.replace('Z', '+00:00'))
try:
    with open(path) as f:
        lines = f.readlines()
except Exception as e:
    print(f"fail:cannot read actions.jsonl: {e}")
    sys.exit(0)
# Scan in reverse to find the most recent matching wake-start
for line in reversed(lines):
    line = line.strip()
    if not line:
        continue
    try:
        d = json.loads(line)
    except Exception:
        continue
    if d.get('action') == 'wake-start' and d.get('agent') == 'health-probe':
        ts_str = d.get('ts', '')
        try:
            ts = datetime.fromisoformat(ts_str.replace('Z', '+00:00'))
        except ValueError:
            continue
        if ts >= test_start:
            inbox_items = d.get('detail', {}).get('inbox_items', 0)
            if inbox_items >= 1:
                print('ok')
            else:
                print(f"fail:wake-start found but inbox_items={inbox_items}")
            sys.exit(0)
print(f"fail:no health-probe wake-start found after {test_start_str}")
PYEOF
) || VERIFY_C="fail:python3 error"

if [[ "$VERIFY_C" != "ok" ]]; then
  echo "check-message-roundtrip: FAIL (c) audit wake-start — $VERIFY_C" >&2
  FAIL=1
fi

# ── Result ────────────────────────────────────────────────────────────────────
if [[ $FAIL -eq 0 ]]; then
  echo "check-message-roundtrip: ok (msg $MSG_ID delivered to health-probe — messages.json + state.json + audit all pass)"
  exit 0
fi

exit 1
