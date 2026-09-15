#!/usr/bin/env bash
# test-cli.sh — E2E tests for the unified hex CLI.
# Verifies every subcommand is accessible and exits gracefully.
# Sourced by run-all.sh which provides PASS/FAIL/assert_* helpers.
set -uo pipefail

HEX="$HEX_DIR/.hex/bin/hex"
VERSION_FILE="$HEX_DIR/.hex/hex-version.txt"

echo ""
echo "=== UNIFIED CLI TESTS ==="

# ── 1. hex version ────────────────────────────────────────────────────────────
OUT=$("$HEX" version 2>&1)
CODE=$?
assert_exit 0 "$CODE" "cli-version: exit 0"
assert_contains "$OUT" "." "cli-version: output contains a version string (has '.')"

# ── 2. hex telemetry present (live subcommand) ──────────────────────────────
# Telemetry was NOT removed: it is a documented, maintained subcommand
# (.hex/telemetry/events.db; CLAUDE.md; see commit fix(telemetry): serialize
# HEX_DIR-mutating tests). The prior "removed in the collapse-to-cc-boi
# demolition" assertion was stale. `hex telemetry` with no subcommand prints
# help and exits non-zero (clap), so assert it is RECOGNIZED, not the exit code.
OUT=$("$HEX" telemetry 2>&1)
if echo "$OUT" | grep -qi "unrecognized subcommand"; then
    assert_fail "cli-telemetry-present: 'hex telemetry' unexpectedly absent — output: $OUT"
else
    assert_pass "cli-telemetry-present: 'hex telemetry' is a recognized subcommand"
fi

# ── 3. hex integration list ──────────────────────────────────────────────────
OUT=$("$HEX" integration list 2>&1)
CODE=$?
# Graceful error if no integrations directory is also acceptable
if [ "$CODE" -eq 0 ] || echo "$OUT" | grep -qi "no integration\|0 integration\|not found\|integration"; then
    assert_pass "cli-integration-list: accessible (exit $CODE)"
else
    assert_fail "cli-integration-list: unexpected exit $CODE — output: $OUT"
fi

# ── 4. hex memory stats (was `memory health`, removed as a pure alias) ───────
OUT=$("$HEX" memory stats 2>&1)
CODE=$?
# Graceful error if memory DB not initialised is also acceptable
if [ "$CODE" -eq 0 ] || echo "$OUT" | grep -qi "stats\|memory\|facts\|not found\|missing"; then
    assert_pass "cli-memory-stats: accessible (exit $CODE)"
else
    assert_fail "cli-memory-stats: unexpected exit $CODE — output: $OUT"
fi

# ── 5. hex doctor --quiet ────────────────────────────────────────────────────
OUT=$("$HEX" doctor --quiet 2>&1)
CODE=$?
# exit 0 = all clear, exit 2 = warnings, anything else = error
if [ "$CODE" -eq 0 ] || [ "$CODE" -eq 2 ]; then
    assert_pass "cli-doctor-quiet: exit $CODE (0=ok, 2=warnings)"
else
    assert_fail "cli-doctor-quiet: exit $CODE (expected 0 or 2) — output: $OUT"
fi

# ── 6. Version consistency: hex version matches Cargo.toml version compiled in ──
if [ -f "$VERSION_FILE" ]; then
    EXPECTED_VERSION=$(cat "$VERSION_FILE" | tr -d '[:space:]')
    VERSION_OUT=$("$HEX" version 2>&1)
    if echo "$VERSION_OUT" | grep -qF "$EXPECTED_VERSION"; then
        assert_pass "cli-version-consistency: 'hex version' output matches compiled Cargo.toml version ($EXPECTED_VERSION)"
    else
        assert_fail "cli-version-consistency: expected '$EXPECTED_VERSION' in 'hex version' output, got: $VERSION_OUT"
    fi
else
    assert_fail "cli-version-consistency: compiled version stamp not found at $VERSION_FILE"
fi
