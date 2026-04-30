#!/usr/bin/env bash
# test-agents-md-complete.sh — Verify AGENTS.md covers all required sections.
#
# Asserts that AGENTS.md contains all the sections present in CLAUDE.md
# to ensure behavioral parity: standing orders, session lifecycle, BOI,
# hex-events, memory, improvement engine.

set -uo pipefail

PASS=0
FAIL=0
SKIP=0
TOTAL=0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

AGENTS_MD="$REPO_DIR/AGENTS.md"

check() {
    TOTAL=$((TOTAL + 1))
    local name="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        echo "  PASS: $name"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $name"
        FAIL=$((FAIL + 1))
    fi
}

check_contains() {
    TOTAL=$((TOTAL + 1))
    local desc="$1"
    local pattern="$2"
    local file="$3"
    if grep -qi "$pattern" "$file" 2>/dev/null; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc — pattern not found: '$pattern'"
        FAIL=$((FAIL + 1))
    fi
}

echo "=== test-agents-md-complete ==="
echo ""

echo "[1] AGENTS.md file exists and is readable"
check "AGENTS.md exists"     test -f "$AGENTS_MD"
check "AGENTS.md non-empty"  test -s "$AGENTS_MD"

if [ ! -f "$AGENTS_MD" ]; then
    echo ""
    echo "  FATAL: AGENTS.md not found — cannot run remaining checks"
    echo "  Results: $PASS passed, $FAIL failed, $SKIP skipped ($TOTAL total)"
    echo "=== test-agents-md-complete: FAIL ==="
    exit 1
fi

echo "[2] Core philosophy / operating principles"
check_contains "core philosophy section"   "core philosophy\|philosophy"      "$AGENTS_MD"
check_contains "compound principle"        "compound"                         "$AGENTS_MD"
check_contains "anticipate principle"      "anticipate"                       "$AGENTS_MD"
check_contains "evolve principle"          "evolve"                           "$AGENTS_MD"

echo "[3] Standing orders"
check_contains "standing orders section"   "standing orders"                  "$AGENTS_MD"

echo "[4] Session lifecycle"
check_contains "session lifecycle section" "session lifecycle\|session"       "$AGENTS_MD"
check_contains "checkpoint protocol"       "checkpoint\|shutdown"             "$AGENTS_MD"

echo "[5] BOI delegation"
check_contains "BOI section"               "BOI\|boi"                         "$AGENTS_MD"
check_contains "dispatch reference"        "dispatch"                         "$AGENTS_MD"

echo "[6] hex-events automation"
check_contains "hex-events section"        "hex-events\|hex_events"           "$AGENTS_MD"

echo "[7] Memory system"
check_contains "memory section"            "memory\|Memory"                   "$AGENTS_MD"
check_contains "memory search reference"   "search\|index"                    "$AGENTS_MD"

echo "[8] Improvement / learning engine"
check_contains "improvement engine"        "improvement\|learning engine"     "$AGENTS_MD"

echo "[9] Tool equivalents (Codex-specific)"
check_contains "tool equivalents"          "tool\|cat\|curl\|sed\|patch"      "$AGENTS_MD"

echo "[10] Skill discovery"
check_contains "skill discovery section"   "skill\|skills"                    "$AGENTS_MD"

echo ""
echo "  Results: $PASS passed, $FAIL failed, $SKIP skipped ($TOTAL total)"
echo ""

if [ "$FAIL" -gt 0 ]; then
    echo "=== test-agents-md-complete: FAIL ==="
    exit 1
fi
echo "=== test-agents-md-complete: PASS ==="
