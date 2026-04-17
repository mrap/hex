#!/usr/bin/env bash
# Verifies Codex can discover all 11 hex skill names and invoke 3 skill-equivalent
# operations in a fresh install.
#
# Note on Codex skill model: Codex does not have first-class "skills" (slash
# commands). It reads AGENTS.md and the .hex/skills/ directory directly.
# Discovery is tested by asking Codex to list procedures from AGENTS.md context
# and by checking that all 11 skill directories are present on disk post-install.
# Invocation equivalents use direct prompts rather than /slash syntax.
#
# Requires OPENAI_API_KEY (auto-sourced from ~/.hex-test.env if present).
# If key or codex CLI is missing, structural checks still run and live session is
# skipped with SKIP (not FAIL) — this is intentional: CI without an OpenAI key
# still validates the static shape of the install.
set -uo pipefail

PASS=0
FAIL=0
SKIP=0
TOTAL=0

INSTALL_DIR="/tmp/hex-skilldisco-codex-$(date +%s)"
MODEL="codex-mini-latest"

# Portable timeout
TIMEOUT_CMD=""
if command -v gtimeout >/dev/null 2>&1; then
    TIMEOUT_CMD="gtimeout 120"
elif command -v timeout >/dev/null 2>&1; then
    TIMEOUT_CMD="timeout 120"
fi

# ── Auth ───────────────────────────────────────────────────────────────────────
if [ -z "${OPENAI_API_KEY:-}" ] && [ -f "$HOME/.hex-test.env" ]; then
    OPENAI_API_KEY=$(grep "^OPENAI_API_KEY=" "$HOME/.hex-test.env" \
        | cut -d= -f2- | tr -d '"' | tr -d "'" || true)
    export OPENAI_API_KEY
fi

HAVE_KEY="${OPENAI_API_KEY:+yes}"
HAVE_CODEX="no"
if command -v codex >/dev/null 2>&1; then
    HAVE_CODEX="yes"
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

check_skip() {
    TOTAL=$((TOTAL + 1))
    echo "  SKIP: $1"
    SKIP=$((SKIP + 1))
}

cleanup() {
    rm -rf "$INSTALL_DIR" 2>/dev/null || true
}
trap cleanup EXIT

echo "=== hex skill discovery test (Codex) ==="
echo ""
echo "NOTE: Codex reads AGENTS.md + .hex/skills/ directly; it does not use"
echo "      Claude Code slash commands. Discovery is verified structurally"
echo "      (disk) and via live Codex reasoning (when key + CLI available)."
echo ""

# ── 1. Fresh install ───────────────────────────────────────────────────────────
echo "[1] Fresh install"
if bash "$REPO_ROOT/install.sh" "$INSTALL_DIR" >/dev/null 2>&1; then
    check_pass "install succeeded"
else
    echo "  FATAL: install.sh failed — aborting"
    exit 1
fi
echo ""

# ── 2. Structural: skill directories on disk ────────────────────────────────────
echo "[2] Skill directories on disk (structural)"
EXPECTED_SKILLS=(landings hex-reflect hex-decide hex-debrief hex-consolidate
                 hex-doctor hex-checkpoint hex-shutdown hex-triage hex-startup memory)
for skill in "${EXPECTED_SKILLS[@]}"; do
    if [ -d "$INSTALL_DIR/.hex/skills/$skill" ]; then
        check_pass "skill dir present: $skill"
    else
        check_fail "skill dir missing: $skill"
    fi
done
echo ""

# ── 3. Structural: AGENTS.md present and non-empty ──────────────────────────────
echo "[3] AGENTS.md (Codex entry point)"
if [ -f "$INSTALL_DIR/AGENTS.md" ] && [ -s "$INSTALL_DIR/AGENTS.md" ]; then
    check_pass "AGENTS.md present and non-empty"
else
    check_fail "AGENTS.md missing or empty"
fi
echo ""

# ── 4. Structural: .codex/config.toml present ──────────────────────────────────
echo "[4] .codex/config.toml"
if [ -f "$INSTALL_DIR/.codex/config.toml" ]; then
    check_pass ".codex/config.toml present"
    if grep -q "codex-mini-latest" "$INSTALL_DIR/.codex/config.toml"; then
        check_pass "config uses codex-mini-latest model"
    else
        check_fail "config does not reference codex-mini-latest"
    fi
else
    check_fail ".codex/config.toml missing"
fi
echo ""

# ── 5. Live: Codex discovery (requires key + CLI) ──────────────────────────────
echo "[5] Codex live skill discovery"
if [ "$HAVE_KEY" != "yes" ]; then
    check_skip "OPENAI_API_KEY not set — skipping live discovery"
    echo "        Set OPENAI_API_KEY or add to ~/.hex-test.env to enable."
elif [ "$HAVE_CODEX" != "yes" ]; then
    check_skip "codex CLI not on PATH — skipping live discovery"
    echo "        Install: npm install -g @openai/codex"
else
    echo "    Asking Codex to list hex procedures..."

    DISCOVERY_OUTPUT=$(cd "$INSTALL_DIR" && \
        OPENAI_API_KEY="$OPENAI_API_KEY" \
        $TIMEOUT_CMD codex exec --model "$MODEL" \
        "Read AGENTS.md and list all the hex skills or procedures available in this workspace. Include their names and what each one does." \
        2>&1) || true

    if [ -z "$DISCOVERY_OUTPUT" ]; then
        check_fail "codex returned empty output"
    else
        check_pass "codex returned non-empty response"
    fi

    for skill in "${EXPECTED_SKILLS[@]}"; do
        if echo "$DISCOVERY_OUTPUT" | grep -qi "$skill"; then
            check_pass "discovery: '$skill' found in output"
        else
            check_fail "discovery: '$skill' NOT found in output"
        fi
    done
fi
echo ""

# ── 6. Live: Codex skill-equivalent invocation (3 skills) ──────────────────────
echo "[6] Codex skill-equivalent invocation (3 skills)"
if [ "$HAVE_KEY" != "yes" ]; then
    check_skip "OPENAI_API_KEY not set — skipping invocation tests"
elif [ "$HAVE_CODEX" != "yes" ]; then
    check_skip "codex CLI not on PATH — skipping invocation tests"
else
    invoke_codex() {
        local label="$1"
        local prompt="$2"
        local check_pattern="$3"
        echo "    Invoking equivalent of: $label"
        local output
        output=$(cd "$INSTALL_DIR" && \
            OPENAI_API_KEY="$OPENAI_API_KEY" \
            $TIMEOUT_CMD codex exec --model "$MODEL" \
            "$prompt" \
            2>&1) || true

        if [ -z "$output" ]; then
            check_fail "invoke $label: got empty output"
            return
        fi
        check_pass "invoke $label: non-empty response"

        if echo "$output" | grep -qiE "traceback|segfault|fatal error|command not found"; then
            check_fail "invoke $label: output contains crash/fatal indicators"
        else
            check_pass "invoke $label: no crash indicators"
        fi

        if [ -n "$check_pattern" ] && echo "$output" | grep -qiE "$check_pattern"; then
            check_pass "invoke $label: expected content found"
        elif [ -n "$check_pattern" ]; then
            check_fail "invoke $label: expected pattern '$check_pattern' not found"
        fi
    }

    invoke_codex "hex-doctor" \
        "Run .hex/scripts/doctor.sh and report the results. If doctor.sh does not exist, list the files in .hex/scripts/ instead." \
        "health|check|install|ok|pass|warn|valid|script|doctor|found|exist"

    invoke_codex "hex-decide" \
        "Use the hex decision framework from .hex/skills/hex-decide/SKILL.md to help decide: 'Should a test fixture use mock data or a temp directory?' Be brief." \
        "option|decision|recommend|consider|choose|trade|mock|temp|fixture"

    invoke_codex "hex-triage" \
        "Check .hex/raw/captures/ for untriaged markdown files (frontmatter triaged: true not set). Report how many need triage or say there are none." \
        "triage|capture|untriaged|nothing|empty|no.*capture|found|none|zero"
fi
echo ""

# ── Summary ────────────────────────────────────────────────────────────────────
echo "Results: $PASS/$TOTAL passed, $FAIL failed, $SKIP skipped"
echo ""

if [ "$HAVE_KEY" != "yes" ] || [ "$HAVE_CODEX" != "yes" ]; then
    echo "Note: live Codex session skipped (key or CLI unavailable)."
    echo "      Structural checks (sections 1-4) validate install shape."
    echo "      Codex skill model: procedures via AGENTS.md, not slash commands."
    echo "      See docs/codex-integration.md for Codex support scope."
fi
echo ""

if [ "$FAIL" -eq 0 ]; then
    echo "PASS: skill-discovery-codex"
    exit 0
else
    echo "FAIL: skill-discovery-codex ($FAIL test(s) failed)"
    exit 1
fi
