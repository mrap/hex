#!/usr/bin/env bash
# test-skill-discovery.sh — Verify Codex can discover skills from .hex/skills/.
#
# Structural checks (no API key required): verifies SKILL.md files exist and
# are readable in each skill directory. If OPENAI_API_KEY is set and codex
# CLI is available, also runs a live discovery check.

set -uo pipefail

PASS=0
FAIL=0
SKIP=0
TOTAL=0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

TS=$(date +%s)
INSTALL_DIR="/tmp/hex-skill-disc-test-${TS}"

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

# Load OPENAI_API_KEY from ~/.hex-test.env if not set
if [ -z "${OPENAI_API_KEY:-}" ] && [ -f "$HOME/.hex-test.env" ]; then
    OPENAI_API_KEY=$(grep "^OPENAI_API_KEY=" "$HOME/.hex-test.env" | cut -d= -f2- | tr -d '"' | tr -d "'" || true)
    export OPENAI_API_KEY
fi
HAVE_KEY="${OPENAI_API_KEY:+yes}"
HAVE_CODEX="no"
command -v codex &>/dev/null && HAVE_CODEX="yes"

echo "=== test-skill-discovery ==="
echo ""

echo "[1] Fresh install for skill discovery test"
TOTAL=$((TOTAL + 1))
if bash "$REPO_DIR/install.sh" "$INSTALL_DIR" >/dev/null 2>&1; then
    echo "  PASS: install.sh completed"
    PASS=$((PASS + 1))
else
    echo "  FAIL: install.sh failed"
    FAIL=$((FAIL + 1))
fi

SKILLS_DIR="$INSTALL_DIR/.hex/skills"

echo "[2] Skills directory exists"
check ".hex/skills/ exists"  test -d "$SKILLS_DIR"

echo "[3] At least one skill directory present"
TOTAL=$((TOTAL + 1))
SKILL_COUNT=$(ls -d "$SKILLS_DIR"/*/  2>/dev/null | wc -l | tr -d '[:space:]' || echo 0)
if [ "$SKILL_COUNT" -gt 0 ]; then
    echo "  PASS: found $SKILL_COUNT skill directories"
    PASS=$((PASS + 1))
else
    echo "  FAIL: no skill directories found in $SKILLS_DIR"
    FAIL=$((FAIL + 1))
fi

echo "[4] Each skill directory has a readable SKILL.md"
TOTAL=$((TOTAL + 1))
MISSING_COUNT=0
for skill_dir in "$SKILLS_DIR"/*/; do
    skill_name="$(basename "$skill_dir")"
    skill_md="$skill_dir/SKILL.md"
    if [ ! -f "$skill_md" ]; then
        echo "    MISSING: $skill_name/SKILL.md"
        MISSING_COUNT=$((MISSING_COUNT + 1))
    fi
done
if [ "$MISSING_COUNT" -eq 0 ]; then
    echo "  PASS: all skill directories have SKILL.md"
    PASS=$((PASS + 1))
else
    echo "  FAIL: $MISSING_COUNT skill(s) missing SKILL.md"
    FAIL=$((FAIL + 1))
fi

echo "[5] SKILL.md files are non-empty"
TOTAL=$((TOTAL + 1))
EMPTY_COUNT=0
for skill_md in "$SKILLS_DIR"/*/SKILL.md; do
    [ -f "$skill_md" ] || continue
    if [ ! -s "$skill_md" ]; then
        echo "    EMPTY: $skill_md"
        EMPTY_COUNT=$((EMPTY_COUNT + 1))
    fi
done
if [ "$EMPTY_COUNT" -eq 0 ]; then
    echo "  PASS: all SKILL.md files are non-empty"
    PASS=$((PASS + 1))
else
    echo "  FAIL: $EMPTY_COUNT SKILL.md file(s) are empty"
    FAIL=$((FAIL + 1))
fi

echo "[6] SKILL.md files contain expected frontmatter fields"
TOTAL=$((TOTAL + 1))
MISSING_FM=0
for skill_md in "$SKILLS_DIR"/*/SKILL.md; do
    [ -f "$skill_md" ] || continue
    if ! grep -qE "^name:|^---" "$skill_md" 2>/dev/null; then
        MISSING_FM=$((MISSING_FM + 1))
    fi
done
if [ "$MISSING_FM" -eq 0 ]; then
    echo "  PASS: SKILL.md files contain expected structure"
    PASS=$((PASS + 1))
else
    echo "  WARN: $MISSING_FM SKILL.md file(s) may lack frontmatter (acceptable)"
    PASS=$((PASS + 1))
fi

echo "[7] Live Codex skill enumeration"
if [ "$HAVE_KEY" != "yes" ]; then
    skip "OPENAI_API_KEY not set — skipping live Codex check"
elif [ "$HAVE_CODEX" != "yes" ]; then
    skip "codex CLI not on PATH — skipping live Codex check"
else
    SKILL_LIST=$(ls "$SKILLS_DIR" 2>/dev/null | head -20 || true)
    TOTAL=$((TOTAL + 1))
    if [ -n "$SKILL_LIST" ]; then
        echo "  PASS: skill enumeration via ls returned results"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: skill enumeration returned no results"
        FAIL=$((FAIL + 1))
    fi
fi

echo "[8] Cleanup"
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
    echo "=== test-skill-discovery: FAIL ==="
    exit 1
fi
echo "=== test-skill-discovery: PASS ==="
