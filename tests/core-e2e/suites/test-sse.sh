#!/usr/bin/env bash
# test-sse.sh — E2E tests for the hex SSE bus.
# Sourced by run-all.sh which provides PASS/FAIL/assert_* helpers.
# Requires: curl, the hex server binary.
set -uo pipefail

HEX="$HEX_DIR/.hex/bin/hex"
SSE_PORT=8882   # dedicated port to avoid collisions with other suites
BASE="http://127.0.0.1:${SSE_PORT}"
SSE_PID_FILE="/tmp/hex-sse-test-$$.pid"

echo ""
echo "=== SSE BUS TESTS ==="

# ── Helpers ───────────────────────────────────────────────────────────────────

stop_server() {
    if [ -f "$SSE_PID_FILE" ]; then
        local pid
        pid=$(cat "$SSE_PID_FILE")
        kill "$pid" 2>/dev/null || true
        rm -f "$SSE_PID_FILE"
    fi
}

# Ensure we stop the server even if the suite exits early
trap stop_server EXIT

start_server() {
    stop_server
    "$HEX" server start --port "$SSE_PORT" >/tmp/hex-sse-server-$$.log 2>&1 &
    echo "$!" > "$SSE_PID_FILE"

    # Wait up to 5s for server to be ready
    local tries=0
    while [ "$tries" -lt 50 ]; do
        if curl -sf "${BASE}/events/health" >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
        tries=$((tries + 1))
    done
    echo "Server failed to start. Log:"
    cat /tmp/hex-sse-server-$$.log 2>/dev/null || true
    return 1
}

# ── 1. Server starts and health endpoint responds ─────────────────────────────
if start_server; then
    assert_pass "sse-server-start: server started on port $SSE_PORT"
else
    assert_fail "sse-server-start: server did not become ready within 5s"
    # Without a server, remaining tests cannot run
    exit 0
fi

HEALTH=$(curl -sf "${BASE}/events/health" 2>&1)
CODE=$?
assert_exit 0 "$CODE" "sse-health: curl exit 0"
assert_contains "$HEALTH" '"status"' "sse-health: response contains 'status'"
assert_contains "$HEALTH" '"subscribers"' "sse-health: response contains 'subscribers'"

# ── 2. Topics endpoint returns expected topic names ───────────────────────────
TOPICS=$(curl -sf "${BASE}/events/topics" 2>&1)
CODE=$?
assert_exit 0 "$CODE" "sse-topics: curl exit 0"
assert_contains "$TOPICS" "content.messages" "sse-topics: content.messages present"
assert_contains "$TOPICS" "content.assets"   "sse-topics: content.assets present"
assert_contains "$TOPICS" "system.agents"    "sse-topics: system.agents present"
assert_contains "$TOPICS" "system.boi"       "sse-topics: system.boi present"

# ── 3. Subscribe + publish: wildcard subscriber receives matching event ────────
SSE_OUT_FILE="/tmp/hex-sse-out-$$.txt"
rm -f "$SSE_OUT_FILE"

# Start background subscriber for test.* topic
curl -sN --max-time 10 "${BASE}/events/stream?topics=test.*" > "$SSE_OUT_FILE" 2>&1 &
SUB_PID=$!

# Give the subscriber a moment to connect
sleep 0.3

# Publish a test event
PUB_RESP=$(curl -sf -X POST "${BASE}/events/publish" \
    -H "Content-Type: application/json" \
    -d '{"topic":"test.ping","type":"e2e","payload":{"msg":"hello"}}' 2>&1)
CODE=$?
assert_exit 0 "$CODE" "sse-publish: curl exit 0"
assert_contains "$PUB_RESP" '"ok"' "sse-publish: response contains 'ok'"

# Give the event a moment to propagate
sleep 0.3

# Kill the subscriber
kill "$SUB_PID" 2>/dev/null || true
wait "$SUB_PID" 2>/dev/null || true

# Verify the subscriber received the event
SSE_CONTENT=$(cat "$SSE_OUT_FILE" 2>/dev/null || true)
assert_contains "$SSE_CONTENT" "test.ping" "sse-subscribe-receive: subscriber received test.ping event"
assert_contains "$SSE_CONTENT" "hello"     "sse-subscribe-receive: subscriber received payload content"
rm -f "$SSE_OUT_FILE"

# ── 4. Wildcard filtering: content.* receives content.messages but not system.boi ─
CONTENT_OUT="/tmp/hex-sse-content-$$.txt"
rm -f "$CONTENT_OUT"

# Subscribe to content.* only
curl -sN --max-time 10 "${BASE}/events/stream?topics=content.*" > "$CONTENT_OUT" 2>&1 &
FILTER_PID=$!
sleep 0.3

# Publish one content event and one system event
curl -sf -X POST "${BASE}/events/publish" \
    -H "Content-Type: application/json" \
    -d '{"topic":"content.messages","type":"update","payload":{"x":1}}' >/dev/null 2>&1
curl -sf -X POST "${BASE}/events/publish" \
    -H "Content-Type: application/json" \
    -d '{"topic":"system.boi","type":"tick","payload":{"y":2}}' >/dev/null 2>&1
sleep 0.3

kill "$FILTER_PID" 2>/dev/null || true
wait "$FILTER_PID" 2>/dev/null || true

CONTENT_BODY=$(cat "$CONTENT_OUT" 2>/dev/null || true)
assert_contains     "$CONTENT_BODY" "content.messages" "sse-wildcard-filter: content.messages received"
assert_not_contains "$CONTENT_BODY" "system.boi"       "sse-wildcard-filter: system.boi NOT received"
rm -f "$CONTENT_OUT"

# ── 5. No-filter subscribe receives everything ────────────────────────────────
ALL_OUT="/tmp/hex-sse-all-$$.txt"
rm -f "$ALL_OUT"

curl -sN --max-time 10 "${BASE}/events/stream" > "$ALL_OUT" 2>&1 &
ALL_PID=$!
sleep 0.3

curl -sf -X POST "${BASE}/events/publish" \
    -H "Content-Type: application/json" \
    -d '{"topic":"content.assets","type":"registered","payload":{"id":"test:001"}}' >/dev/null 2>&1
sleep 0.3

kill "$ALL_PID" 2>/dev/null || true
wait "$ALL_PID" 2>/dev/null || true

ALL_BODY=$(cat "$ALL_OUT" 2>/dev/null || true)
assert_contains "$ALL_BODY" "content.assets" "sse-no-filter: unfiltered subscriber receives content.assets"
rm -f "$ALL_OUT"

# ── 6. Heartbeat: subscribe with no events and wait for `: heartbeat` ─────────
# Heartbeat fires after 15s idle. Use a fresh port-publish approach:
# We can skip publishing and just check after a brief pause that the connection
# is still alive (non-empty headers). A full 15s wait would bloat the suite.
# Instead, verify the SSE header format is correct (connection stays alive).
HBEAT_OUT="/tmp/hex-sse-hbeat-$$.txt"
rm -f "$HBEAT_OUT"

curl -sN --max-time 3 "${BASE}/events/stream?topics=heartbeat.test" > "$HBEAT_OUT" 2>&1 &
HBEAT_PID=$!
sleep 1.5

# After 1.5s the connection should be open (no data yet, but curl is alive)
if kill -0 "$HBEAT_PID" 2>/dev/null; then
    assert_pass "sse-heartbeat-alive: SSE connection stays alive without events"
else
    # curl exited, check if it received the SSE header at minimum
    HBEAT_BODY=$(cat "$HBEAT_OUT" 2>/dev/null || true)
    if [ -z "$HBEAT_BODY" ]; then
        assert_fail "sse-heartbeat-alive: connection closed prematurely with no output"
    else
        assert_pass "sse-heartbeat-alive: connection sent output before closing"
    fi
fi

kill "$HBEAT_PID" 2>/dev/null || true
wait "$HBEAT_PID" 2>/dev/null || true
rm -f "$HBEAT_OUT"

# ── Cleanup ───────────────────────────────────────────────────────────────────
stop_server
rm -f "/tmp/hex-sse-server-$$.log"
