#!/bin/bash
# run-hex-events-checks.sh — Standalone runner for hex-events module health checks
#
# Usage:
#   run-hex-events-checks.sh              # Check only
#   run-hex-events-checks.sh --fix        # Auto-fix issues
#   run-hex-events-checks.sh --smoke      # Include smoke tests
#   run-hex-events-checks.sh --quiet      # Only errors/warnings

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECKS_DIR="$SCRIPT_DIR/doctor-checks"

# ─── Flags ────────────────────────────────────────────────────────────────────
FIX=false
QUIET=false
SMOKE=false

for arg in "$@"; do
  case "$arg" in
    --fix)   FIX=true ;;
    --quiet) QUIET=true ;;
    --smoke) SMOKE=true ;;
    --help|-h)
      echo "Usage: run-hex-events-checks.sh [--fix] [--quiet] [--smoke]"
      exit 0
      ;;
    *) echo "Unknown option: $arg" >&2; exit 1 ;;
  esac
done

# ─── State ────────────────────────────────────────────────────────────────────
PASS_COUNT=0
WARN_COUNT=0
ERROR_COUNT=0
FIXED_COUNT=0
HAS_ERRORS=false
HAS_WARNINGS=false

CHECKS_FILE=$(mktemp /tmp/hex-events-checks.XXXXXX)
trap 'rm -f "$CHECKS_FILE"' EXIT

# ─── Colors ───────────────────────────────────────────────────────────────────
GREEN='\033[32m'
YELLOW='\033[33m'
RED='\033[31m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

# ─── Output helpers (same interface as doctor.sh) ─────────────────────────────
_pass() {
  PASS_COUNT=$((PASS_COUNT + 1))
  if ! $QUIET; then
    echo -e "  [${GREEN}PASS${RESET}] $1"
  fi
}

_warn() {
  WARN_COUNT=$((WARN_COUNT + 1))
  HAS_WARNINGS=true
  echo -e "  [${YELLOW}WARN${RESET}] $1"
}

_error() {
  ERROR_COUNT=$((ERROR_COUNT + 1))
  HAS_ERRORS=true
  echo -e "  [${RED}ERROR${RESET}] $1"
}

_fixed() {
  FIXED_COUNT=$((FIXED_COUNT + 1))
  echo -e "  [${GREEN}FIXED${RESET}] $1"
}

_info() {
  if ! $QUIET; then
    echo -e "  ${DIM}→${RESET} $1"
  fi
}

_rec() {
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" >> "$CHECKS_FILE"
}

# ─── Load and run hex-events checks ──────────────────────────────────────────
source "$CHECKS_DIR/hex-events.sh"

echo ""
echo -e "${BOLD}hex-events Module — Health Check${RESET}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "${DIM}HEX_EVENTS_DIR=${HEX_EVENTS_DIR:-$HOME/.hex-events}${RESET}"
if $SMOKE; then
  echo -e "${DIM}Smoke tests: enabled${RESET}"
fi
echo ""

run_hex_events_checks

echo ""
echo -e "  ${BOLD}Summary:${RESET} ${PASS_COUNT} passed, ${WARN_COUNT} warnings, ${ERROR_COUNT} errors, ${FIXED_COUNT} fixed"
echo ""

if $HAS_ERRORS; then
  exit 1
elif $HAS_WARNINGS; then
  exit 2
fi
exit 0
