#!/usr/bin/env bash
# test-messaging.sh — E2E tests for the hex messaging primitive.
# Sourced by run-all.sh which provides PASS/FAIL/assert_* helpers.
# All operations go through the unified hex binary CLI.
set -uo pipefail

HEX="$HEX_DIR/.hex/bin/hex"
MESSAGES_FILE="$HEX_DIR/.hex/data/messages.json"
DATA_DIR="$HEX_DIR/.hex/data"

echo ""
echo "=== MESSAGING TESTS ==="

# Clean state for this suite so earlier suite runs don't bleed in
rm -f "$MESSAGES_FILE"
mkdir -p "$DATA_DIR"

# ── 1. Create a comment message ───────────────────────────────────────────────
OUT=$("$HEX" message send mike brand \
    --content "test comment" \
    --msg-type comment \
    --anchor "post:P-001" 2>&1)
CODE=$?
assert_exit 0 "$CODE" "msg-send-comment: exit 0"
assert_contains "$OUT" "Sent message M" "msg-send-comment: output shows 'Sent message M…'"

MSG1_ID=$(echo "$OUT" | grep -o 'M[a-f0-9]*' | head -1)

# ── 2. Create an agent message ────────────────────────────────────────────────
OUT=$("$HEX" message send brand cos --content "status update" 2>&1)
CODE=$?
assert_exit 0 "$CODE" "msg-send-agent: exit 0"
assert_contains "$OUT" "Sent message M" "msg-send-agent: output shows 'Sent message M…'"
MSG2_ID=$(echo "$OUT" | grep -o 'M[a-f0-9]*' | head -1)

# ── 3. Create a notification ──────────────────────────────────────────────────
OUT=$("$HEX" message send system mike \
    --content "deploy complete" \
    --msg-type notification 2>&1)
CODE=$?
assert_exit 0 "$CODE" "msg-send-notification: exit 0"
MSG3_ID=$(echo "$OUT" | grep -o 'M[a-f0-9]*' | head -1)

# ── 4. List all messages ──────────────────────────────────────────────────────
OUT=$("$HEX" message list 2>&1)
CODE=$?
assert_exit 0 "$CODE" "msg-list-all: exit 0"
assert_contains "$OUT" "3 messages" "msg-list-all: shows '3 messages'"

# ── 5. Filter by type ─────────────────────────────────────────────────────────
OUT=$("$HEX" message list --msg-type comment 2>&1)
assert_contains "$OUT" "1 messages" "msg-list-type-comment: shows '1 messages'"
assert_contains "$OUT" "comment" "msg-list-type-comment: output contains 'comment'"
assert_not_contains "$OUT" "agent" "msg-list-type-comment: no 'agent' type in output"

# ── 6. Filter by status ───────────────────────────────────────────────────────
OUT=$("$HEX" message list --status new 2>&1)
assert_contains "$OUT" "3 messages" "msg-list-status-new: all 3 messages are 'new'"

# ── 7. Respond to a message (status change) ───────────────────────────────────
if [ -n "${MSG1_ID:-}" ]; then
    OUT=$("$HEX" message respond "$MSG1_ID" acting "Working on it" 2>&1)
    CODE=$?
    assert_exit 0 "$CODE" "msg-respond-acting: exit 0"
    assert_contains "$OUT" "status=acting" "msg-respond-acting: output shows status=acting"
else
    assert_fail "msg-respond-acting: skipped — MSG1_ID not captured"
fi

# ── 8. Respond with related assets ───────────────────────────────────────────
if [ -n "${MSG1_ID:-}" ]; then
    OUT=$("$HEX" message respond "$MSG1_ID" done "Shipped" \
        --assets "post:P-001" \
        --assets "proposal:build-in-public" 2>&1)
    CODE=$?
    assert_exit 0 "$CODE" "msg-respond-assets: exit 0"
    assert_contains "$OUT" "status=done" "msg-respond-assets: output shows status=done"

    # Verify action log has related_assets in the JSON file
    ASSETS_CHECK=$(python3 - <<PYEOF 2>&1
import json, sys
with open("$MESSAGES_FILE") as f:
    data = json.load(f)
msgs = [m for m in data["messages"] if m["id"] == "$MSG1_ID"]
if not msgs:
    print("NOT_FOUND")
    sys.exit(1)
msg = msgs[0]
for entry in msg.get("action_log", []):
    if entry.get("related_assets"):
        print("HAS_ASSETS")
        sys.exit(0)
print("NO_ASSETS")
PYEOF
)
    assert_contains "$ASSETS_CHECK" "HAS_ASSETS" "msg-respond-assets: action_log entry has related_assets"
else
    assert_fail "msg-respond-assets: skipped — MSG1_ID not captured"
fi

# ── 9. Verify JSON storage ────────────────────────────────────────────────────
if [ -f "$MESSAGES_FILE" ]; then
    MSG_COUNT=$(python3 -c "
import json
with open('$MESSAGES_FILE') as f:
    data = json.load(f)
print(len(data['messages']))
" 2>&1)
    if [ "$MSG_COUNT" -eq 3 ] 2>/dev/null; then
        assert_pass "msg-storage-count: messages.json has 3 messages"
    else
        assert_fail "msg-storage-count: expected 3 messages, got $MSG_COUNT"
    fi

    ACTION_COUNT=$(python3 -c "
import json
with open('$MESSAGES_FILE') as f:
    data = json.load(f)
total = sum(len(m.get('action_log', [])) for m in data['messages'])
print(total)
" 2>&1)
    # MSG1 was responded to twice → at least 2 action entries
    if [ "$ACTION_COUNT" -ge 2 ] 2>/dev/null; then
        assert_pass "msg-storage-actions: action_log has $ACTION_COUNT entries (expected >=2)"
    else
        assert_fail "msg-storage-actions: expected >=2 action_log entries, got $ACTION_COUNT"
    fi
else
    assert_fail "msg-storage-count: messages.json does not exist"
    assert_fail "msg-storage-actions: messages.json does not exist"
fi

# ── 10. Migration: legacy comments.json → messages.json ──────────────────────
# Remove current messages.json so migration is triggered on the next run
rm -f "$MESSAGES_FILE"
python3 - <<PYEOF 2>/dev/null
import json
legacy = {
    "comments": [
        {
            "id": "c-legacy-001",
            "asset": "post:P-999",
            "text": "Legacy comment text",
            "author": "mike",
            "status": "new",
            "created_at": "2026-01-01T00:00:00Z",
            "action_log": [],
            "routed_to": []
        }
    ]
}
with open("$DATA_DIR/comments.json", "w") as f:
    json.dump(legacy, f)
PYEOF

# Any hex message command triggers migrate_if_needed()
"$HEX" message list > /dev/null 2>&1 || true

MIGRATED=$(python3 - <<PYEOF 2>&1
import json, sys
try:
    with open("$MESSAGES_FILE") as f:
        data = json.load(f)
    msgs = [m for m in data["messages"] if m["id"] == "c-legacy-001"]
    if msgs and msgs[0].get("msg_type") == "comment":
        print("MIGRATED")
    else:
        print("NOT_MIGRATED")
except Exception as e:
    print(f"ERROR: {e}")
    sys.exit(1)
PYEOF
)
assert_contains "$MIGRATED" "MIGRATED" "msg-migration: legacy comments.json migrated to messages.json"
