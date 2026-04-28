#!/usr/bin/env bash
# run-all.sh — Master test runner for the core hex E2E suite.
# Runs inside Docker. Sources helpers and each suite in order.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── Load shared helpers (defines PASS, FAIL, assert_* functions) ──────────────
# shellcheck source=helpers.sh
source "$SCRIPT_DIR/helpers.sh"

bold "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
bold "  hex core E2E suite"
bold "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

SUITES=(
    "test-cli"
    "test-messaging"
    "test-events"
    "test-assets"
    "test-sse"
    "test-telemetry"
)

# ── Run each suite ─────────────────────────────────────────────────────────────
for suite in "${SUITES[@]}"; do
    suite_file="$SCRIPT_DIR/suites/${suite}.sh"
    if [ ! -f "$suite_file" ]; then
        red "  MISSING suite file: $suite_file"
        FAIL=$((FAIL + 1))
        continue
    fi

    pass_before=$PASS
    fail_before=$FAIL

    # Source rather than subshell so PASS/FAIL counters accumulate
    # shellcheck source=/dev/null
    source "$suite_file"

    print_suite_summary "$suite" "$pass_before" "$fail_before"
done

# ── Overall summary ────────────────────────────────────────────────────────────
TOTAL=$((PASS + FAIL))
echo ""
bold "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
if [ "$FAIL" -eq 0 ]; then
    green "  ALL $TOTAL TESTS PASSED"
else
    red   "  $FAIL/$TOTAL TESTS FAILED"
fi
bold "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

exit "$FAIL"
