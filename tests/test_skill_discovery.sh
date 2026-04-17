#!/usr/bin/env bash
# Verifies Claude Code can discover all 11 hex skills and invoke 3 of them.
# Requires ANTHROPIC_API_KEY (auto-sourced from ~/.hex-test.env if present).
set -uo pipefail

PASS=0
FAIL=0
TOTAL=0

INSTALL_DIR="/tmp/hex-skilldisco-$(date +%s)"
MODEL="claude-haiku-4-5"

# Portable timeout: gtimeout (macOS coreutils) or timeout (Linux/Docker)
TIMEOUT_CMD=""
if command -v gtimeout >/dev/null 2>&1; then
    TIMEOUT_CMD="gtimeout 120"
elif command -v timeout >/dev/null 2>&1; then
    TIMEOUT_CMD="timeout 120"
fi

# ── Auth ───────────────────────────────────────────────────────────
if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ -f "$HOME/.hex-test.env" ]; then
    ANTHROPIC_API_KEY=$(grep "^ANTHROPIC_API_KEY=" "$HOME/.hex-test.env" \
        | cut -d= -f2- | tr -d '"' | tr -d "'")
    export ANTHROPIC_API_KEY
fi

if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "SKIP: ANTHROPIC_API_KEY not set and ~/.hex-test.env missing"
    echo "  Create ~/.hex-test.env with: ANTHROPIC_API_KEY=sk-ant-..."
    exit 0
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

check_pass() {
    TOTAL=$((TOTAL + 1))
    echo "  PASS: $1"
    PASS=$((PASS + 1))
}

check_fail() {
    TOTAL=$((TOTAL + 1))
    echo "  FAIL: $1"
    FAIL=$((FAIL + 1))
}

cleanup() {
    rm -rf "$INSTALL_DIR" 2>/dev/null || true
}
trap cleanup EXIT

echo "=== hex skill discovery test (Claude Code) ==="
echo ""

# ── 1. Fresh install ──────────────────────────────────────────────
echo "[1] Fresh install"
if bash "$REPO_ROOT/install.sh" "$INSTALL_DIR" >/dev/null 2>&1; then
    check_pass "install succeeded"
else
    echo "  FATAL: install.sh failed — aborting"
    exit 1
fi
echo ""

# ── 2. Skill files on disk ────────────────────────────────────────
echo "[2] Skill files on disk"
# memory has no SKILL.md (it's a script-only skill) — check scripts dir instead
SKILLS_WITH_SKILL_MD=(landings hex-reflect hex-decide hex-debrief hex-consolidate hex-doctor hex-checkpoint hex-shutdown hex-triage hex-startup)
for skill in "${SKILLS_WITH_SKILL_MD[@]}"; do
    if [ -f "$INSTALL_DIR/.hex/skills/$skill/SKILL.md" ]; then
        check_pass "SKILL.md present: $skill"
    else
        check_fail "SKILL.md missing: $skill"
    fi
done
# memory: verify scripts directory exists
if [ -d "$INSTALL_DIR/.hex/skills/memory/scripts" ]; then
    check_pass "memory skill: scripts/ directory present"
else
    check_fail "memory skill: scripts/ directory missing"
fi
echo ""

# ── 3. Claude Code skill discovery ───────────────────────────────
echo "[3] Claude Code discovery (model: $MODEL)"
echo "    Asking Claude to list all hex skills..."

# landings and memory are not slash commands — landings is a dir-based framework,
# memory is script-only. Claude discovers via .claude/commands/; hex-upgrade IS one.
EXPECTED_SKILLS=(hex-reflect hex-decide hex-debrief hex-consolidate hex-doctor hex-checkpoint hex-shutdown hex-triage hex-startup hex-upgrade)

DISCOVERY_OUTPUT=$(cd "$INSTALL_DIR" && $TIMEOUT_CMD claude \
    -p "What hex skills and commands are available in this installation? List them all." \
    --model "$MODEL" \
    --dangerously-skip-permissions \
    2>&1) || true

if [ -z "$DISCOVERY_OUTPUT" ]; then
    check_fail "claude returned empty output"
else
    check_pass "claude returned non-empty response"
fi

for skill in "${EXPECTED_SKILLS[@]}"; do
    if echo "$DISCOVERY_OUTPUT" | grep -qi "$skill"; then
        check_pass "discovery: '$skill' found in output"
    else
        check_fail "discovery: '$skill' NOT found in output"
    fi
done
echo ""

# ── 4. Skill invocation smoke tests ──────────────────────────────
echo "[4] Skill invocation smoke tests (3 skills)"

invoke_skill() {
    local skill_name="$1"
    local prompt="$2"
    local check_pattern="$3"
    echo "    Invoking: $skill_name"
    local output
    output=$(cd "$INSTALL_DIR" && $TIMEOUT_CMD claude \
        -p "$prompt" \
        --model "$MODEL" \
        --dangerously-skip-permissions \
        2>&1) || true

    if [ -z "$output" ]; then
        check_fail "invoke $skill_name: got empty output"
        return
    fi
    check_pass "invoke $skill_name: non-empty response"

    # Fail only if output contains crash/fatal indicators (not ordinary "error" mentions)
    if echo "$output" | grep -qiE "traceback|segfault|fatal error|command not found"; then
        check_fail "invoke $skill_name: output contains crash/fatal indicators"
    else
        check_pass "invoke $skill_name: output clean (no crashes)"
    fi

    if [ -n "$check_pattern" ]; then
        if echo "$output" | grep -qiE "$check_pattern"; then
            check_pass "invoke $skill_name: expected content found"
        else
            check_fail "invoke $skill_name: expected pattern '$check_pattern' not found in output"
        fi
    fi
}

invoke_skill "hex-doctor" \
    "/hex-doctor" \
    "health|check|install|ok|pass|warn|valid|script|doctor"

invoke_skill "hex-decide" \
    '/hex-decide "test decision about test fixture"' \
    "option|decision|recommend|consider|choose|trade|test fixture"

invoke_skill "hex-triage" \
    "/hex-triage" \
    "triage|capture|untriaged|nothing|empty|no.*capture|found"

echo ""
# ── Summary ───────────────────────────────────────────────────────
echo "Results: $PASS/$TOTAL passed, $FAIL failed"
echo ""
if [ "$FAIL" -eq 0 ]; then
    echo "PASS: skill-discovery"
    exit 0
else
    echo "FAIL: skill-discovery ($FAIL test(s) failed)"
    exit 1
fi
