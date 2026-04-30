#!/usr/bin/env bash
# run-all.sh — Master test runner for the core hex E2E suite.
# Discovers all suites under suites/ and runs them in sorted order.
# Can also be run on the host (e.g. for BOI suites that need Docker access).
#
# Usage: run-all.sh [--include <pattern>] [--exclude <pattern>]
#   --include <pattern>   Only run suites whose name matches grep -E pattern
#   --exclude <pattern>   Skip suites whose name matches grep -E pattern
#
# Examples:
#   run-all.sh                        # all suites
#   run-all.sh --exclude boi          # skip BOI suites (e.g. in container w/o Docker)
#   run-all.sh --include boi          # only BOI suites (e.g. on host with Docker)
#   run-all.sh --include 'install|upgrade'  # specific suites by pattern
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── Parse flags ────────────────────────────────────────────────────────────────
INCLUDE_PATTERN=""
EXCLUDE_PATTERN=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --include) INCLUDE_PATTERN="$2"; shift 2 ;;
        --exclude) EXCLUDE_PATTERN="$2"; shift 2 ;;
        *) echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
done

# ── Load shared helpers (defines PASS, FAIL, assert_* functions) ──────────────
# shellcheck source=helpers.sh
source "$SCRIPT_DIR/helpers.sh"

bold "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
bold "  hex core E2E suite"
bold "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
[[ -n "$INCLUDE_PATTERN" ]] && yellow "  --include: $INCLUDE_PATTERN"
[[ -n "$EXCLUDE_PATTERN" ]] && yellow "  --exclude: $EXCLUDE_PATTERN"

# ── Discover suites ────────────────────────────────────────────────────────────
# Auto-discover *.sh files in suites/, sorted alphabetically.
# Filter by --include / --exclude patterns (grep -E regex on suite name).
SUITES=()
while IFS= read -r suite_file; do
    suite=$(basename "$suite_file" .sh)
    if [[ -n "$INCLUDE_PATTERN" ]] && ! echo "$suite" | grep -qE "$INCLUDE_PATTERN"; then
        continue
    fi
    if [[ -n "$EXCLUDE_PATTERN" ]] && echo "$suite" | grep -qE "$EXCLUDE_PATTERN"; then
        continue
    fi
    SUITES+=("$suite")
done < <(ls "$SCRIPT_DIR/suites/"*.sh 2>/dev/null | sort || true)

if [[ ${#SUITES[@]} -eq 0 ]]; then
    yellow "  No suites matched (include='${INCLUDE_PATTERN}' exclude='${EXCLUDE_PATTERN}')"
    exit 0
fi

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
