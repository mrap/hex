#!/usr/bin/env bash
# test-install-shape.sh — Verify fresh hex install produces expected structure.
#
# Asserts that a fresh install via install.sh produces:
#   .hex/scripts/, .hex/skills/, .hex/bin/, CLAUDE.md, AGENTS.md
# These must exist regardless of whether Claude Code or Codex is the runtime.

set -uo pipefail

PASS=0
FAIL=0
SKIP=0
TOTAL=0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

TS=$(date +%s)
INSTALL_DIR="/tmp/hex-shape-test-${TS}"

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

skip() {
    SKIP=$((SKIP + 1))
    echo "  SKIP: $1"
}

echo "=== test-install-shape ==="
echo ""

echo "[1] Running install.sh into $INSTALL_DIR"
TOTAL=$((TOTAL + 1))
if bash "$REPO_DIR/install.sh" "$INSTALL_DIR" >/dev/null 2>&1; then
    echo "  PASS: install.sh completed"
    PASS=$((PASS + 1))
else
    echo "  FAIL: install.sh failed — remaining checks may error"
    FAIL=$((FAIL + 1))
fi

echo "[2] Core directory structure"
check ".hex/scripts/ exists"  test -d "$INSTALL_DIR/.hex/scripts"
check ".hex/skills/ exists"   test -d "$INSTALL_DIR/.hex/skills"
check ".hex/bin/ exists"      test -d "$INSTALL_DIR/.hex/bin"

echo "[3] Instruction files"
check "CLAUDE.md exists"  test -f "$INSTALL_DIR/CLAUDE.md"
check "AGENTS.md exists"  test -f "$INSTALL_DIR/AGENTS.md"

echo "[4] AGENTS.md is non-empty"
check "AGENTS.md non-empty"  test -s "$INSTALL_DIR/AGENTS.md"

echo "[5] Cleanup"
TOTAL=$((TOTAL + 1))
if rm -rf "$INSTALL_DIR" 2>/dev/null; then
    echo "  PASS: cleanup"
    PASS=$((PASS + 1))
else
    echo "  FAIL: cleanup failed — remove manually: $INSTALL_DIR"
    FAIL=$((FAIL + 1))
fi

echo ""
echo "  Results: $PASS passed, $FAIL failed, $SKIP skipped ($TOTAL total)"
echo ""

if [ "$FAIL" -gt 0 ]; then
    echo "=== test-install-shape: FAIL ==="
    exit 1
fi
echo "=== test-install-shape: PASS ==="
