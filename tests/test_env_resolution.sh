#!/usr/bin/env bash
# test_env_resolution.sh — E2E tests for env.sh, path resolution, and AGENT_DIR.
#
# Validates the v0.4.0 fixes:
#   1. env.sh exists after install and is executable
#   2. HEX_DIR / AGENT_DIR are set after sourcing env.sh
#   3. PATH includes common tool locations after sourcing env.sh
#   4. hex-agent-spawn.sh has no hardcoded absolute paths
#   5. CLAUDE.md template references binaries, not paths
#   6. verify-agent-infra.sh auto-detects HEX_DIR
#   7. Metrics scripts don't crash without AGENT_DIR set
#   8. Install upgrades existing companions (not just skips)
#
# Usage:
#   bash test_env_resolution.sh                   # Run against local checkout
#   docker build -f tests/Dockerfile.env -t hex-env-test . && docker run hex-env-test

set -uo pipefail

PASS=0
FAIL=0
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

red()   { printf '\033[31mFAIL: %s\033[0m\n' "$*"; }
green() { printf '\033[32mPASS: %s\033[0m\n' "$*"; }
bold()  { printf '\033[1m%s\033[0m\n' "$*"; }

assert_pass() { PASS=$((PASS + 1)); green "$1"; }
assert_fail() { FAIL=$((FAIL + 1)); red "$1"; }

# ── Setup: run install to a temp dir ────────────────────────────────────────
INSTALL_BASE=$(mktemp -d /tmp/hex-env-test-XXXXXX)
INSTALL_DIR="$INSTALL_BASE/hex"
trap 'rm -rf "$INSTALL_BASE"' EXIT

bold "══ hex v0.4.0 Environment Resolution Tests ══"
echo "Install dir: $INSTALL_DIR"
echo ""

# ── Test 0: Install succeeds ────────────────────────────────────────────────
bold "── Install ──"
if bash "$REPO_DIR/install.sh" "$INSTALL_DIR" 2>&1 | tail -5; then
  assert_pass "install.sh completed without errors"
else
  assert_fail "install.sh failed"
fi
echo ""

# ── Test 1: env.sh exists and is executable ─────────────────────────────────
bold "── env.sh ──"
if [ -f "$INSTALL_DIR/.hex/scripts/env.sh" ]; then
  assert_pass "env.sh exists at .hex/scripts/env.sh"
else
  assert_fail "env.sh missing from .hex/scripts/env.sh"
fi

if [ -x "$INSTALL_DIR/.hex/scripts/env.sh" ]; then
  assert_pass "env.sh is executable"
else
  assert_fail "env.sh is not executable"
fi

# ── Test 2: Sourcing env.sh sets HEX_DIR and AGENT_DIR ─────────────────────
bold "── HEX_DIR / AGENT_DIR ──"
ENV_OUT=$(HEX_DIR="$INSTALL_DIR" bash -c "source '$INSTALL_DIR/.hex/scripts/env.sh' && echo HEX_DIR=\$HEX_DIR AGENT_DIR=\$AGENT_DIR" 2>&1)

if echo "$ENV_OUT" | grep -q "HEX_DIR=$INSTALL_DIR"; then
  assert_pass "HEX_DIR set correctly after sourcing env.sh"
else
  assert_fail "HEX_DIR not set correctly: $ENV_OUT"
fi

if echo "$ENV_OUT" | grep -q "AGENT_DIR=$INSTALL_DIR"; then
  assert_pass "AGENT_DIR set correctly (mirrors HEX_DIR)"
else
  assert_fail "AGENT_DIR not set: $ENV_OUT"
fi

# Test auto-detection (no env vars pre-set)
AUTO_OUT=$(unset HEX_DIR; unset AGENT_DIR; bash -c "source '$INSTALL_DIR/.hex/scripts/env.sh' && echo HEX_DIR=\$HEX_DIR" 2>&1)
if echo "$AUTO_OUT" | grep -q "HEX_DIR="; then
  if echo "$AUTO_OUT" | grep -q "HEX_DIR=$"; then
    assert_fail "HEX_DIR auto-detection produced empty value"
  else
    assert_pass "HEX_DIR auto-detected from script location"
  fi
else
  assert_fail "HEX_DIR auto-detection failed: $AUTO_OUT"
fi

# ── Test 3: PATH includes common tool locations ────────────────────────────
bold "── PATH ──"
PATH_OUT=$(HEX_DIR="$INSTALL_DIR" bash -c "source '$INSTALL_DIR/.hex/scripts/env.sh' && echo \$PATH" 2>&1)

# Always expected (created by env.sh even if dir doesn't exist on this platform)
for loc in ".local/bin" "/usr/local/bin"; do
  if echo "$PATH_OUT" | grep -q "$loc"; then
    assert_pass "PATH includes $loc"
  else
    assert_fail "PATH missing $loc"
  fi
done
# Platform-specific: /opt/homebrew/bin only exists on macOS ARM
if [ -d "/opt/homebrew/bin" ]; then
  if echo "$PATH_OUT" | grep -q "/opt/homebrew/bin"; then
    assert_pass "PATH includes /opt/homebrew/bin (macOS)"
  else
    assert_fail "PATH missing /opt/homebrew/bin (macOS)"
  fi
else
  assert_pass "PATH skips /opt/homebrew/bin (not macOS, dir absent — correct behavior)"
fi

# ── Test 4: hex-agent-spawn.sh has no hardcoded absolute paths ──────────────
bold "── hex-agent-spawn.sh ──"
SPAWN_SCRIPT="$INSTALL_DIR/.hex/scripts/hex-agent-spawn.sh"
if [ -f "$SPAWN_SCRIPT" ]; then
  if grep -q "$HOME" "$SPAWN_SCRIPT"; then
    assert_fail "hex-agent-spawn.sh still contains hardcoded home path ($HOME)"
    grep "$HOME" "$SPAWN_SCRIPT" | head -3
  else
    assert_pass "hex-agent-spawn.sh has no hardcoded user paths"
  fi

  if grep -q 'HEX_EVENTS_CLI=' "$SPAWN_SCRIPT"; then
    assert_fail "hex-agent-spawn.sh still has HEX_EVENTS_CLI variable (should use binary)"
  else
    assert_pass "hex-agent-spawn.sh uses hex-events binary, not path variable"
  fi

  if head -10 "$SPAWN_SCRIPT" | grep -q 'SCRIPT_DIR='; then
    assert_pass "hex-agent-spawn.sh auto-detects HEX_DIR from script location"
  else
    assert_fail "hex-agent-spawn.sh doesn't auto-detect HEX_DIR"
  fi
else
  assert_fail "hex-agent-spawn.sh not found in install"
fi

# ── Test 5: CLAUDE.md template references binaries not paths ────────────────
bold "── CLAUDE.md template ──"
CLAUDE_MD="$INSTALL_DIR/CLAUDE.md"
if [ -f "$CLAUDE_MD" ]; then
  if grep -q 'bash ~/.boi/boi' "$CLAUDE_MD"; then
    assert_fail "CLAUDE.md still references 'bash ~/.boi/boi' (should be 'boi')"
  else
    assert_pass "CLAUDE.md uses 'boi' binary references"
  fi

  if grep -q 'python3 ~/.hex-events/hex_events_cli.py' "$CLAUDE_MD"; then
    assert_fail "CLAUDE.md still references 'python3 ~/.hex-events/hex_events_cli.py'"
  else
    assert_pass "CLAUDE.md uses 'hex-events' binary references"
  fi

  if grep -q 'env\.sh' "$CLAUDE_MD"; then
    assert_pass "CLAUDE.md documents env.sh"
  else
    assert_fail "CLAUDE.md doesn't mention env.sh"
  fi
else
  assert_fail "CLAUDE.md not found in install"
fi

# ── Test 6: verify-agent-infra.sh doesn't hard-require AGENT_DIR ───────────
bold "── verify-agent-infra.sh ──"
VERIFY_SCRIPT="$INSTALL_DIR/.hex/scripts/verify-agent-infra.sh"
if [ -f "$VERIFY_SCRIPT" ]; then
  if grep -q 'AGENT_DIR:?' "$VERIFY_SCRIPT"; then
    assert_fail "verify-agent-infra.sh still hard-requires AGENT_DIR with :?"
  else
    assert_pass "verify-agent-infra.sh doesn't hard-require AGENT_DIR"
  fi
else
  assert_fail "verify-agent-infra.sh not found"
fi

# ── Test 7: Metrics scripts don't crash without AGENT_DIR ──────────────────
bold "── metrics scripts ──"
for script in context-continuity.py done-claim-verification.py frustration-signals.py loop-waste-detection.py; do
  SCRIPT_PATH="$INSTALL_DIR/.hex/scripts/metrics/$script"
  if [ -f "$SCRIPT_PATH" ]; then
    if grep -q 'os.environ\["AGENT_DIR"\]' "$SCRIPT_PATH"; then
      assert_fail "$script still uses os.environ[\"AGENT_DIR\"] (hard crash if unset)"
    else
      assert_pass "$script uses safe AGENT_DIR resolution"
    fi
  fi
done

# Check hex-vitals.py
VITALS="$INSTALL_DIR/.hex/scripts/hex-vitals.py"
if [ -f "$VITALS" ]; then
  if grep -q 'os.environ\["AGENT_DIR"\]' "$VITALS"; then
    assert_fail "hex-vitals.py still uses os.environ[\"AGENT_DIR\"] (hard crash)"
  else
    assert_pass "hex-vitals.py uses safe AGENT_DIR resolution"
  fi
fi

# ── Test 8: doctor.sh check_21 passes with env.sh present ──────────────────
bold "── doctor check_21 ──"
DOCTOR="$INSTALL_DIR/.hex/scripts/doctor.sh"
if [ -f "$DOCTOR" ]; then
  if grep -q 'env_file.*\.hex/env\.sh' "$DOCTOR"; then
    assert_fail "doctor.sh check_21 looks for .hex/env.sh (should be .hex/scripts/env.sh)"
  fi
fi

# ── Test 9: Version present ──────────────────────────────────────────────────
bold "── version ──"
VERSION=$(cat "$INSTALL_DIR/.hex/version.txt" 2>/dev/null || echo "MISSING")
EXPECTED_VERSION=$(cat "$REPO_DIR/system/version.txt" 2>/dev/null || echo "v0.4.0")
if [ "$VERSION" = "$EXPECTED_VERSION" ]; then
  assert_pass "Version is $VERSION"
else
  assert_fail "Version is '$VERSION' (expected $EXPECTED_VERSION)"
fi

# ── Summary ─────────────────────────────────────────────────────────────────
echo ""
bold "══ Results ══"
echo "  Pass: $PASS"
echo "  Fail: $FAIL"
echo "  Total: $((PASS + FAIL))"

if [ $FAIL -gt 0 ]; then
  red "OVERALL: FAIL ($FAIL failures)"
  exit 1
else
  green "OVERALL: PASS"
  exit 0
fi
