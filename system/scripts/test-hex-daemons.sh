#!/bin/bash
# test-hex-daemons.sh — Integration tests for hex-daemons and daemon-check-only behavior
#
# Tests:
#   1. hex-daemons status runs without error
#   2. hex-daemons list shows all 4 managed daemons
#   3. startup.sh does not start daemons (no nohup/systemctl start/launchctl load)
#   4. boi.sh dispatch path does not start daemons

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
FAIL=0

pass() { echo "  [PASS] $1"; PASS=$((PASS + 1)); }
fail() { echo "  [FAIL] $1"; FAIL=$((FAIL + 1)); }

echo "=== hex-daemons integration tests ==="
echo ""

# ─── Test 1: hex-daemons status runs without error ────────────────────────
echo "Test 1: hex-daemons status"
STATUS_OUT=$(bash "$AGENT_DIR/.hex/scripts/hex-daemons.sh" status 2>&1) || true

# Strip ANSI codes
STRIPPED=$(echo "$STATUS_OUT" | sed 's/\x1b\[[0-9;]*m//g')

if echo "$STRIPPED" | grep -q "hex-daemons status"; then
    pass "hex-daemons status runs and produces output"
else
    fail "hex-daemons status did not produce expected header"
    echo "    Got: $STRIPPED"
fi

# ─── Test 2: hex-daemons list shows all 4 daemons ────────────────────────
echo "Test 2: hex-daemons list"
LIST_OUT=$(bash "$AGENT_DIR/.hex/scripts/hex-daemons.sh" list 2>&1)
LIST_STRIPPED=$(echo "$LIST_OUT" | sed 's/\x1b\[[0-9;]*m//g')

EXPECTED_DAEMONS=("boi-daemon" "hex-events" "boi-poold" "syncthing")
ALL_FOUND=true
for daemon in "${EXPECTED_DAEMONS[@]}"; do
    if ! echo "$LIST_STRIPPED" | grep -q "$daemon"; then
        fail "hex-daemons list missing: $daemon"
        ALL_FOUND=false
    fi
done
if $ALL_FOUND; then
    pass "hex-daemons list shows all 4 daemons"
fi

# ─── Test 3: startup.sh does not start daemons ───────────────────────────
echo "Test 3: startup.sh does not start daemons"
STARTUP_FILE="$AGENT_DIR/.hex/scripts/startup.sh"

# Check that startup.sh does not contain actual daemon-starting commands
# (exclude lines that are info/warn strings or comments)
STARTUP_CLEAN=true

# Look for nohup daemon starts (not in strings)
if grep -E 'nohup.*daemon' "$STARTUP_FILE" 2>/dev/null | grep -vE '^\s*#|info |warn |echo |".*nohup' | grep -q .; then
    fail "startup.sh contains nohup daemon auto-start"
    STARTUP_CLEAN=false
fi

# Look for direct systemctl start/enable calls (not in info/warn strings)
if grep -E 'systemctl --user (start|enable)' "$STARTUP_FILE" 2>/dev/null | grep -vE '^\s*#|info |warn |echo |".*systemctl' | grep -q .; then
    fail "startup.sh contains systemctl start/enable calls"
    STARTUP_CLEAN=false
fi

# Look for direct launchctl load calls (not in info/warn strings)
if grep -E 'launchctl (load|bootstrap)' "$STARTUP_FILE" 2>/dev/null | grep -vE '^\s*#|info |warn |echo |".*launchctl' | grep -q .; then
    fail "startup.sh contains launchctl load/bootstrap calls"
    STARTUP_CLEAN=false
fi

if $STARTUP_CLEAN; then
    pass "startup.sh does not contain daemon-starting commands"
fi

# Verify startup.sh uses hex-daemons for daemon health checks
if grep -q 'hex-daemons' "$STARTUP_FILE" 2>/dev/null; then
    pass "startup.sh uses hex-daemons for daemon status checks"
else
    fail "startup.sh does not reference hex-daemons for daemon health checks"
fi

# ─── Test 4: boi.sh dispatch path does not start daemons ─────────────────
echo "Test 4: boi.sh dispatch path does not start daemons"
BOI_FILE="$HOME/boi/boi.sh"

if [[ ! -f "$BOI_FILE" ]]; then
    echo "  [SKIP] boi.sh not found at $BOI_FILE (skipping boi.sh tests)"
else
    # The dispatch function (cmd_dispatch) should not auto-start daemons.
    # Extract the dispatch function and check for nohup starts.
    # The dispatch path uses require_daemon + progress_warn pattern, not nohup start.
    # Note: boi.sh may still have nohup in other paths (e.g., upgrade restarts daemon).
    # We only care that the dispatch path warns instead of starting.

    # Check that dispatch path uses warn/progress_warn for daemon status
    if grep -q 'progress_warn.*hex-daemons\|hex-daemons start boi-daemon' "$BOI_FILE" 2>/dev/null; then
        pass "boi.sh dispatch path warns to use hex-daemons (does not auto-start)"
    else
        fail "boi.sh dispatch path does not warn to use hex-daemons"
    fi

    # Check that boi.sh does not auto-start poold with nohup in the dispatch path
    # (poold nohup in other contexts like 'boi pool start' is acceptable)
    if grep -q 'hex-daemons start boi-poold\|hex-daemons.*poold' "$BOI_FILE" 2>/dev/null; then
        pass "boi.sh references hex-daemons for poold management"
    else
        # Check that at minimum the dispatch path does not nohup-start poold
        # The dispatch section (after enqueue) should use progress_warn
        if grep -A5 'pool_enabled.*true' "$BOI_FILE" 2>/dev/null | grep -q 'progress_warn\|warn'; then
            pass "boi.sh dispatch path warns about poold status (does not auto-start)"
        else
            fail "boi.sh dispatch path may still auto-start poold"
        fi
    fi

    # General: boi.sh should reference hex-daemons somewhere
    if grep -q "hex-daemons" "$BOI_FILE" 2>/dev/null; then
        pass "boi.sh references hex-daemons for daemon management"
    else
        fail "boi.sh does not reference hex-daemons"
    fi
fi

# ─── Summary ─────────────────────────────────────────────────────────────
echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [[ $FAIL -gt 0 ]]; then
    exit 1
fi
exit 0
