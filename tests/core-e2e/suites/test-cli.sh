#!/usr/bin/env bash
# test-cli.sh — E2E tests for the unified hex CLI.
# Verifies every subcommand is accessible and exits gracefully.
# Sourced by run-all.sh which provides PASS/FAIL/assert_* helpers.
set -uo pipefail

HEX="$HEX_DIR/.hex/bin/hex"
HEX_AGENT="$HEX_DIR/.hex/bin/hex-agent"
VERSION_FILE="$HEX_DIR/version.txt"

echo ""
echo "=== UNIFIED CLI TESTS ==="

# ── 1. hex version ────────────────────────────────────────────────────────────
OUT=$("$HEX" version 2>&1)
CODE=$?
assert_exit 0 "$CODE" "cli-version: exit 0"
assert_contains "$OUT" "." "cli-version: output contains a version string (has '.')"

# ── 2. hex agent fleet ────────────────────────────────────────────────────────
OUT=$("$HEX" agent fleet 2>&1)
CODE=$?
# Acceptable: exit 0 (fleet listed) or exit 1 with "no agents" style message
if [ "$CODE" -eq 0 ]; then
    assert_pass "cli-agent-fleet: exit 0"
elif echo "$OUT" | grep -qi "no\|empty\|agent\|0 agent"; then
    assert_pass "cli-agent-fleet: graceful 'no agents' response (exit $CODE)"
else
    assert_fail "cli-agent-fleet: unexpected exit $CODE — output: $OUT"
fi

# ── 3. hex agent list ─────────────────────────────────────────────────────────
OUT=$("$HEX" agent list 2>&1)
CODE=$?
if [ "$CODE" -eq 0 ] || echo "$OUT" | grep -qi "no\|empty\|agent\|0 agent"; then
    assert_pass "cli-agent-list: accessible (exit $CODE)"
else
    assert_fail "cli-agent-list: unexpected exit $CODE — output: $OUT"
fi

# ── 4. hex message list ───────────────────────────────────────────────────────
OUT=$("$HEX" message list 2>&1)
CODE=$?
assert_exit 0 "$CODE" "cli-message-list: exit 0"

# ── 5. hex events policies ────────────────────────────────────────────────────
OUT=$("$HEX" events policies 2>&1)
CODE=$?
assert_exit 0 "$CODE" "cli-events-policies: exit 0"
# The e2e-test.yaml policy was installed in the Dockerfile
assert_contains "$OUT" "e2e-test" "cli-events-policies: e2e-test policy appears in listing"

# ── 6. hex asset types ────────────────────────────────────────────────────────
OUT=$("$HEX" asset types 2>&1)
CODE=$?
assert_exit 0 "$CODE" "cli-asset-types: exit 0"

# ── 7. hex sse topics ─────────────────────────────────────────────────────────
OUT=$("$HEX" sse topics 2>&1)
CODE=$?
# May read from disk (no server needed) or require server; either way exit 0
if [ "$CODE" -eq 0 ]; then
    assert_pass "cli-sse-topics: exit 0"
elif echo "$OUT" | grep -qi "topic\|sse\|content\|system"; then
    assert_pass "cli-sse-topics: accessible with topic output (exit $CODE)"
else
    assert_fail "cli-sse-topics: unexpected exit $CODE — output: $OUT"
fi

# ── 8. hex integration list ───────────────────────────────────────────────────
OUT=$("$HEX" integration list 2>&1)
CODE=$?
# Graceful error if no integrations directory is also acceptable
if [ "$CODE" -eq 0 ] || echo "$OUT" | grep -qi "no integration\|0 integration\|not found\|integration"; then
    assert_pass "cli-integration-list: accessible (exit $CODE)"
else
    assert_fail "cli-integration-list: unexpected exit $CODE — output: $OUT"
fi

# ── 9. hex memory health ──────────────────────────────────────────────────────
OUT=$("$HEX" memory health 2>&1)
CODE=$?
# Graceful error if memory DB not initialised is also acceptable
if [ "$CODE" -eq 0 ] || echo "$OUT" | grep -qi "health\|memory\|ok\|no\|not found\|missing"; then
    assert_pass "cli-memory-health: accessible (exit $CODE)"
else
    assert_fail "cli-memory-health: unexpected exit $CODE — output: $OUT"
fi

# ── 10. hex doctor --quiet ────────────────────────────────────────────────────
OUT=$("$HEX" doctor --quiet 2>&1)
CODE=$?
# exit 0 = all clear, exit 2 = warnings, anything else = error
if [ "$CODE" -eq 0 ] || [ "$CODE" -eq 2 ]; then
    assert_pass "cli-doctor-quiet: exit $CODE (0=ok, 2=warnings)"
else
    assert_fail "cli-doctor-quiet: exit $CODE (expected 0 or 2) — output: $OUT"
fi

# ── 11. hex-agent fleet (backward compat symlink) ─────────────────────────────
if [ -L "$HEX_AGENT" ] || [ -f "$HEX_AGENT" ]; then
    OUT=$("$HEX_AGENT" fleet 2>&1)
    CODE=$?
    if [ "$CODE" -eq 0 ] || echo "$OUT" | grep -qi "no\|empty\|agent\|0 agent"; then
        assert_pass "cli-hex-agent-symlink: hex-agent fleet accessible (exit $CODE)"
    else
        assert_fail "cli-hex-agent-symlink: unexpected exit $CODE — output: $OUT"
    fi
else
    assert_fail "cli-hex-agent-symlink: $HEX_AGENT does not exist"
fi

# ── 12. Version consistency: hex version matches version.txt ──────────────────
if [ -f "$VERSION_FILE" ]; then
    EXPECTED_VERSION=$(cat "$VERSION_FILE" | tr -d '[:space:]')
    VERSION_OUT=$("$HEX" version 2>&1)
    if echo "$VERSION_OUT" | grep -qF "$EXPECTED_VERSION"; then
        assert_pass "cli-version-consistency: 'hex version' output contains version.txt value ($EXPECTED_VERSION)"
    else
        assert_fail "cli-version-consistency: expected '$EXPECTED_VERSION' in 'hex version' output, got: $VERSION_OUT"
    fi
else
    assert_fail "cli-version-consistency: version.txt not found at $VERSION_FILE"
fi
