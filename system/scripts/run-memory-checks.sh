#!/bin/bash
# run-memory-checks.sh — Standalone runner for memory module health checks
#
# Usage:
#   run-memory-checks.sh              # Check only
#   run-memory-checks.sh --fix        # Auto-fix issues
#   run-memory-checks.sh --quiet      # Only errors/warnings
#
# This script sets up the same environment as doctor.sh and runs the
# memory module checks. Used for standalone verification and by t-7
# when wiring everything into hex-doctor.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECKS_DIR="$SCRIPT_DIR/doctor-checks"

# ─── Resolve AGENT_DIR ────────────────────────────────────────────────────────
if [ -z "${AGENT_DIR:-}" ]; then
  candidate="$SCRIPT_DIR"
  while [ "$candidate" != "/" ]; do
    if [ -f "$candidate/CLAUDE.md" ]; then
      AGENT_DIR="$candidate"
      break
    fi
    candidate="$(dirname "$candidate")"
  done
  AGENT_DIR="${AGENT_DIR:-${AGENT_DIR:-$HOME/hex}}"
fi

CLAUDE_DIR="$AGENT_DIR/.claude"

# ─── Flags ────────────────────────────────────────────────────────────────────
FIX=false
QUIET=false

for arg in "$@"; do
  case "$arg" in
    --fix)   FIX=true ;;
    --quiet) QUIET=true ;;
    --help|-h)
      echo "Usage: run-memory-checks.sh [--fix] [--quiet]"
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

CHECKS_FILE=$(mktemp /tmp/memory-checks.XXXXXX)
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

# ─── Load and run memory checks ───────────────────────────────────────────────
source "$CHECKS_DIR/memory.sh"

echo ""
echo -e "${BOLD}Memory Module — Health Check${RESET}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "${DIM}AGENT_DIR=$AGENT_DIR${RESET}"
echo ""

run_memory_checks

echo ""
echo -e "  ${BOLD}Summary:${RESET} ${PASS_COUNT} passed, ${WARN_COUNT} warnings, ${ERROR_COUNT} errors, ${FIXED_COUNT} fixed"
echo ""

if $HAS_ERRORS; then
  exit 1
elif $HAS_WARNINGS; then
  exit 2
fi
exit 0
