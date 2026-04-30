#!/usr/bin/env bash
# run-all.sh — Orchestrator for the codex-parity E2E suite.
#
# Usage (local):
#   bash tests/codex-parity/run-all.sh
#
# Usage (Docker):
#   docker build -f tests/codex-parity/Dockerfile -t hex-codex-parity .
#   docker run --rm -e OPENAI_API_KEY="$OPENAI_API_KEY" hex-codex-parity
#
# Discovers test-*.sh scripts in the same directory, runs each in sequence,
# aggregates PASS/FAIL/SKIP, and exits non-zero on any FAIL.

set -uo pipefail

SUITE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TOTAL=0
PASS=0
FAIL=0
SKIP=0
FAILED_TESTS=()

echo "=============================================="
echo " hex Codex Parity Test Suite"
echo "=============================================="
echo ""

# Auto-discover test scripts
TESTS=()
for t in "$SUITE_DIR"/test-*.sh; do
    [ -f "$t" ] && TESTS+=("$t")
done

if [ ${#TESTS[@]} -eq 0 ]; then
    echo "ERROR: no test-*.sh scripts found in $SUITE_DIR"
    exit 1
fi

echo "Found ${#TESTS[@]} test script(s):"
for t in "${TESTS[@]}"; do
    echo "  - $(basename "$t")"
done
echo ""

# Run each test script and collect results
for TEST_SCRIPT in "${TESTS[@]}"; do
    TEST_NAME="$(basename "$TEST_SCRIPT" .sh)"
    echo "----------------------------------------------"
    echo "Running: $TEST_NAME"
    echo "----------------------------------------------"

    # Each script outputs its own PASS/FAIL lines; capture its exit code.
    # We parse PASS/FAIL/SKIP from the script's stdout header summary line.
    set +u
    SCRIPT_OUT=$(bash "$TEST_SCRIPT" 2>&1)
    SCRIPT_EXIT=$?
    set -u

    echo "$SCRIPT_OUT"
    echo ""

    TOTAL=$((TOTAL + 1))

    # Parse summary from last "Results:" line in output
    SUMMARY_LINE=$(echo "$SCRIPT_OUT" | grep "Results:" | tail -1 || true)
    if [ -n "$SUMMARY_LINE" ]; then
        T_PASS=$(echo "$SUMMARY_LINE" | grep -oE '[0-9]+ passed' | grep -oE '[0-9]+' || echo 0)
        T_FAIL=$(echo "$SUMMARY_LINE" | grep -oE '[0-9]+ failed' | grep -oE '[0-9]+' || echo 0)
        T_SKIP=$(echo "$SUMMARY_LINE" | grep -oE '[0-9]+ skipped' | grep -oE '[0-9]+' || echo 0)
    else
        T_PASS=0
        T_FAIL=0
        T_SKIP=0
    fi

    if [ "$SCRIPT_EXIT" -eq 0 ]; then
        echo "[PASS] $TEST_NAME"
        PASS=$((PASS + 1))
    else
        echo "[FAIL] $TEST_NAME (exit $SCRIPT_EXIT)"
        FAIL=$((FAIL + 1))
        FAILED_TESTS+=("$TEST_NAME")
    fi
    echo ""
done

echo "=============================================="
echo " Suite Results"
echo "=============================================="
echo "  Total scripts : $TOTAL"
echo "  Passed        : $PASS"
echo "  Failed        : $FAIL"
echo ""

if [ "$FAIL" -gt 0 ]; then
    echo "  FAILED TESTS:"
    for f in "${FAILED_TESTS[@]}"; do
        echo "    - $f"
    done
    echo ""
    echo "  RESULT: FAIL"
    exit 1
fi

echo "  RESULT: PASS"
exit 0
