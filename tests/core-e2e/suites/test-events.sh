#!/usr/bin/env bash
# test-events.sh — E2E tests for the hex event engine CLI.
# Sourced by run-all.sh which provides PASS/FAIL/assert_* helpers.
# All operations go through the unified hex binary CLI.
set -uo pipefail

HEX="$HEX_DIR/.hex/bin/hex"

# The events engine always uses ~/.hex-events (not $HEX_DIR) via shellexpand::tilde
HEX_EVENTS_DIR="$HOME/.hex-events"
EVENTS_DB="$HEX_EVENTS_DIR/events.db"
POLICIES_DIR="$HEX_EVENTS_DIR/policies"

echo ""
echo "=== EVENT ENGINE TESTS ==="

# Set up a clean events environment for this suite
rm -f "$EVENTS_DB"
mkdir -p "$POLICIES_DIR"
# Remove any stale test policies from prior runs
rm -f "$POLICIES_DIR/e2e-test.yaml" "$POLICIES_DIR/e2e-condition-test.yaml"

# ── 1. Emit a basic event ─────────────────────────────────────────────────────
OUT=$("$HEX" events emit test.ping '{"source":"e2e"}' 2>&1)
CODE=$?
assert_exit 0 "$CODE" "events-emit: exit 0"
assert_contains "$OUT" "Emitted test.ping" "events-emit: output shows 'Emitted test.ping'"

# Capture the event ID for later trace test
EVENT_ID=$(echo "$OUT" | grep -o 'id=[0-9]*' | cut -d= -f2 | head -1)

# ── 2. Verify recent event in SQLite ─────────────────────────────────────────
if [ -f "$EVENTS_DB" ]; then
    COUNT=$(sqlite3 "$EVENTS_DB" \
        "SELECT count(*) FROM events WHERE event_type = 'test.ping';" 2>&1)
    if [ "${COUNT:-0}" -ge 1 ] 2>/dev/null; then
        assert_pass "events-db-query: events table has >= 1 test.ping row"
    else
        assert_fail "events-db-query: expected test.ping in events table, got count=$COUNT"
    fi
else
    assert_fail "events-db-query: events.db does not exist at $EVENTS_DB"
fi

# ── 3. Policy matching: policy fires on test.ping ────────────────────────────
rm -f /tmp/e2e-policy-fired

# Write the policy to the correct location (~/.hex-events/policies/)
cat > "$POLICIES_DIR/e2e-test.yaml" <<'POLICY'
name: e2e-test
description: "E2E test policy — fires on test.ping"
enabled: true
rules:
  - name: ping-handler
    trigger:
      event: test.ping
    actions:
      - type: shell
        command: touch /tmp/e2e-policy-fired
        timeout: 5
POLICY

# Reload policies and emit
"$HEX" events reload > /dev/null 2>&1 || true
"$HEX" events emit test.ping '{"source":"policy-test"}' > /dev/null 2>&1

# Give the action executor a moment to run the shell action
sleep 1

if [ -f /tmp/e2e-policy-fired ]; then
    assert_pass "events-policy-match: test.ping policy fired and created marker file"
else
    assert_fail "events-policy-match: marker file /tmp/e2e-policy-fired not created after test.ping"
fi

# ── 4. Condition evaluation: conditional policy ──────────────────────────────
rm -f /tmp/e2e-critical-fired

cat > "$POLICIES_DIR/e2e-condition-test.yaml" <<'POLICY'
name: e2e-condition-test
description: "E2E conditional policy — fires only when level=critical"
enabled: true
rules:
  - name: critical-handler
    trigger:
      event: test.critical
    conditions:
      - field: payload.level
        op: eq
        value: critical
    actions:
      - type: shell
        command: touch /tmp/e2e-critical-fired
        timeout: 5
POLICY

"$HEX" events reload > /dev/null 2>&1 || true

# Emit with level=info — should NOT fire
"$HEX" events emit test.critical '{"level":"info"}' > /dev/null 2>&1
sleep 1
if [ ! -f /tmp/e2e-critical-fired ]; then
    assert_pass "events-condition-miss: condition level=info does NOT fire critical-handler"
else
    assert_fail "events-condition-miss: critical-handler incorrectly fired for level=info"
fi

# Emit with level=critical — MUST fire
"$HEX" events emit test.critical '{"level":"critical"}' > /dev/null 2>&1
sleep 1
if [ -f /tmp/e2e-critical-fired ]; then
    assert_pass "events-condition-hit: condition level=critical fires critical-handler"
else
    assert_fail "events-condition-hit: critical-handler did not fire for level=critical"
fi

# ── 5. Scheduler-style tick event ─────────────────────────────────────────────
# The scheduler runs on 60s intervals — can't wait that long in a test.
# Emit a timer.tick event manually to verify the engine stores scheduler-style events.
"$HEX" events emit timer.tick.minutely '{}' > /dev/null 2>&1
TICK_COUNT=$(sqlite3 "$EVENTS_DB" \
    "SELECT count(*) FROM events WHERE event_type LIKE 'timer.tick.%';" 2>&1)
if [ "${TICK_COUNT:-0}" -ge 1 ] 2>/dev/null; then
    assert_pass "events-tick: timer.tick.* event stored in events table"
else
    assert_fail "events-tick: expected timer.tick.* in events table, got count=$TICK_COUNT"
fi

# ── 6. Policy listing ─────────────────────────────────────────────────────────
OUT=$("$HEX" events policies 2>&1)
CODE=$?
assert_exit 0 "$CODE" "events-policies: exit 0"
assert_contains "$OUT" "policies" "events-policies: output contains 'policies'"
assert_contains "$OUT" "rules" "events-policies: output contains 'rules' count"

# ── 7. Event trace ────────────────────────────────────────────────────────────
if [ -n "${EVENT_ID:-}" ]; then
    OUT=$("$HEX" events trace "$EVENT_ID" 2>&1)
    CODE=$?
    assert_exit 0 "$CODE" "events-trace: exit 0"
    assert_contains "$OUT" "test.ping" "events-trace: output shows event type test.ping"
    assert_contains "$OUT" "Action chain" "events-trace: output shows 'Action chain'"
else
    assert_fail "events-trace: skipped — EVENT_ID not captured from emit output"
fi

# ── 8. SQLite verification ────────────────────────────────────────────────────
if [ -f "$EVENTS_DB" ]; then
    TOTAL=$(sqlite3 "$EVENTS_DB" "SELECT count(*) FROM events;" 2>&1)
    if [ "${TOTAL:-0}" -ge 4 ] 2>/dev/null; then
        assert_pass "events-sqlite-count: events table has $TOTAL rows (expected >=4)"
    else
        assert_fail "events-sqlite-count: expected >=4 events in DB, got $TOTAL"
    fi

    ACTION_ROWS=$(sqlite3 "$EVENTS_DB" "SELECT count(*) FROM action_log;" 2>&1)
    if [ "${ACTION_ROWS:-0}" -ge 1 ] 2>/dev/null; then
        assert_pass "events-sqlite-actions: action_log has $ACTION_ROWS entries"
    else
        assert_fail "events-sqlite-actions: expected >=1 action_log entries, got $ACTION_ROWS"
    fi

    # Verify schema columns exist
    SCHEMA=$(sqlite3 "$EVENTS_DB" ".schema events" 2>&1)
    assert_contains "$SCHEMA" "event_type" "events-sqlite-schema: events table has event_type column"
    assert_contains "$SCHEMA" "payload" "events-sqlite-schema: events table has payload column"
    assert_contains "$SCHEMA" "created_at" "events-sqlite-schema: events table has created_at column"
else
    assert_fail "events-sqlite-count: events.db not found at $EVENTS_DB"
    assert_fail "events-sqlite-actions: events.db not found at $EVENTS_DB"
    assert_fail "events-sqlite-schema: events.db not found at $EVENTS_DB"
fi
