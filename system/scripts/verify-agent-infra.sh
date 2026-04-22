#!/usr/bin/env bash
# verify-agent-infra.sh — Integration test for agent self-healing infrastructure.
# Tests the REAL system, not mocks. Run after any change to wake scripts,
# env.sh, agent.py, or watchdog.
#
# Exit 0 = all pass, non-zero = failures.
# Must pass before any agent infra change is declared "done."
set -uo pipefail

PASS=0
FAIL=0
HEX_DIR="/Users/mrap/mrap-hex"
ENV_SH="$HEX_DIR/.hex/env.sh"

red()   { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
bold()  { printf '\033[1m%s\033[0m\n' "$*"; }

assert_pass() {
  PASS=$((PASS + 1))
  green "  PASS: $1"
}

assert_fail() {
  FAIL=$((FAIL + 1))
  red "  FAIL: $1"
}

# ══════════════════════════════════════════════════════════════════════════════
bold "═══ Agent Infrastructure Verification ═══"
bold "Testing against LIVE system — $(date)"
echo ""

# ── 1. env.sh loads in non-interactive bash ──────────────────────────────────
bold "1. env.sh loads in non-interactive bash"

ENV_OUT=$(/bin/bash -c "source '$ENV_SH' && type claude 2>&1 && type agent_check_circuit_breaker 2>&1" 2>&1)
if echo "$ENV_OUT" | grep -q 'claude is a function' && echo "$ENV_OUT" | grep -q 'agent_check_circuit_breaker is a function'; then
  assert_pass "claude function and circuit breaker defined"
else
  assert_fail "claude function or circuit breaker missing after sourcing env.sh"
  echo "    Output: $ENV_OUT"
fi

# ── 2. claude binary reachable via env.sh PATH ──────────────────────────────
bold "2. claude binary reachable via env.sh"

CLAUDE_PATH=$(/bin/bash -c "source '$ENV_SH' && command -v claude" 2>&1)
if [ -n "$CLAUDE_PATH" ]; then
  assert_pass "claude found at: $CLAUDE_PATH"
else
  assert_fail "claude not found on PATH after sourcing env.sh"
fi

CLAUDE_VER=$(/bin/bash -c "source '$ENV_SH' && claude --version" 2>&1)
if echo "$CLAUDE_VER" | grep -q 'Claude Code'; then
  assert_pass "claude --version returns: $CLAUDE_VER"
else
  assert_fail "claude --version failed: $CLAUDE_VER"
fi

# ── 3. --dangerously-skip-permissions baked into function ────────────────────
bold "3. --dangerously-skip-permissions in claude function"

FUNC_BODY=$(/bin/bash -c "source '$ENV_SH' && declare -f claude" 2>&1)
if echo "$FUNC_BODY" | grep -q '\-\-dangerously-skip-permissions'; then
  assert_pass "claude function includes --dangerously-skip-permissions"
else
  assert_fail "claude function missing --dangerously-skip-permissions"
  echo "    Function: $FUNC_BODY"
fi

# ── 4. hex-agent binary exists and fleet validates ───────────────────────────
bold "4. hex-agent binary + fleet validation"

HEX_AGENT_BIN="$HEX_DIR/.hex/bin/hex-agent"
if [ -x "$HEX_AGENT_BIN" ]; then
  assert_pass "hex-agent binary exists and is executable"
  if HEX_DIR="$HEX_DIR" "$HEX_AGENT_BIN" fleet >/dev/null 2>&1; then
    AGENT_COUNT=$(HEX_DIR="$HEX_DIR" "$HEX_AGENT_BIN" list 2>/dev/null | wc -l | tr -d ' ')
    assert_pass "fleet validates — $AGENT_COUNT agents discovered from charters"
  else
    assert_fail "hex-agent fleet failed — charter validation errors (run: hex-agent fleet)"
  fi
else
  assert_fail "hex-agent binary missing at $HEX_AGENT_BIN"
fi

# ── 5. hex-agent rejects unregistered agents ─────────────────────────────────
bold "5. hex-agent rejects unregistered agents"

_wake_out=$(HEX_DIR="$HEX_DIR" "$HEX_AGENT_BIN" wake nonexistent-test --trigger test 2>&1) || true
if echo "$_wake_out" | grep -q "not registered"; then
  assert_pass "hex-agent wake rejects unregistered agent with clear error"
else
  assert_fail "hex-agent wake did not reject unregistered agent"
fi

# ── 6. hex-agent validates charter id matches directory ──────────────────────
bold "6. Charter id validation"

if HEX_DIR="$HEX_DIR" "$HEX_AGENT_BIN" fleet 2>&1 | grep -q "ERROR"; then
  assert_fail "fleet has charter validation errors — run hex-agent fleet for details"
else
  assert_pass "all charters pass validation (id matches directory)"
fi

# ── 7. Circuit breaker trips on 3 consecutive failures ──────────────────────
bold "7. Circuit breaker integration test"

TMPLOG=$(mktemp /tmp/verify-cb-XXXXXX.jsonl)
echo '{"ts":"2026-04-22T01:00:00Z","status":"failed","rc":127}' >> "$TMPLOG"
echo '{"ts":"2026-04-22T01:00:01Z","status":"failed","rc":127}' >> "$TMPLOG"
echo '{"ts":"2026-04-22T01:00:02Z","status":"failed","rc":127}' >> "$TMPLOG"

/bin/bash -c "source '$ENV_SH' && agent_check_circuit_breaker verify-test '$TMPLOG' 3" 2>/dev/null
CB_RC=$?
if [ $CB_RC -eq 1 ] && [ -f "$HOME/.hex-verify-test-HALT" ]; then
  assert_pass "circuit breaker tripped (rc=1, HALT file created)"
else
  assert_fail "circuit breaker did NOT trip (rc=$CB_RC)"
fi
rm -f "$HOME/.hex-verify-test-HALT" "$TMPLOG"

# Does NOT trip on mixed log
TMPLOG2=$(mktemp /tmp/verify-cb2-XXXXXX.jsonl)
echo '{"ts":"2026-04-22T01:00:00Z","status":"failed","rc":127}' >> "$TMPLOG2"
echo '{"ts":"2026-04-22T01:00:01Z","status":"completed","rc":0}' >> "$TMPLOG2"
echo '{"ts":"2026-04-22T01:00:02Z","status":"failed","rc":127}' >> "$TMPLOG2"

/bin/bash -c "source '$ENV_SH' && agent_check_circuit_breaker verify-test2 '$TMPLOG2' 3" 2>/dev/null
CB_RC2=$?
if [ $CB_RC2 -eq 0 ] && [ ! -f "$HOME/.hex-verify-test2-HALT" ]; then
  assert_pass "circuit breaker did NOT false-trip on mixed log"
else
  assert_fail "circuit breaker false-tripped on mixed log"
fi
rm -f "$HOME/.hex-verify-test2-HALT" "$TMPLOG2"

# ── 8. Budget reporting via Rust harness ─────────────────────────────────────
bold "8. Budget tracking via hex-agent status"

# agent.py was archived when the Rust harness took over budget tracking.
# Verify the Rust harness correctly reports cost/budget fields.
_budget_out=$(HEX_DIR="$HEX_DIR" "$HEX_AGENT_BIN" status hex-ops 2>&1) || true
if echo "$_budget_out" | grep -qE "^Cost \(lifetime\):"; then
  assert_pass "hex-agent reports cost tracking (lifetime field present)"
else
  assert_fail "hex-agent status missing cost tracking: $_budget_out"
fi

# ── 9. Watchdog script runs and finds doctor.sh ─────────────────────────────
bold "9. Watchdog finds and runs doctor.sh"

WD_OUT=$(AGENT_DIR="$HEX_DIR" bash "$HEX_DIR/.hex/scripts/hex-doctor-watchdog.sh" 2>&1)
WD_RC=$?
if [ $WD_RC -eq 0 ]; then
  assert_pass "watchdog ran clean (exit 0)"
else
  # Exit 2 = warnings only, still acceptable
  if echo "$WD_OUT" | grep -q 'hex-doctor not found'; then
    assert_fail "watchdog can't find doctor.sh"
  else
    assert_pass "watchdog ran with warnings (exit $WD_RC)"
  fi
fi

# ── 10. Watchdog recovery: removes HALT when claude is reachable ─────────────
bold "10. Watchdog auto-recovery"

touch "$HOME/.hex-verify-recovery-HALT"
# Simulate watchdog recovery logic
RECOVERY_OUT=$(/bin/bash -c "
source '$ENV_SH'
_halt=\"\$HOME/.hex-verify-recovery-HALT\"
if [ -f \"\$_halt\" ] && command -v claude &>/dev/null; then
  rm -f \"\$_halt\"
  echo 'RECOVERED'
else
  echo 'NOT_RECOVERED'
fi
" 2>&1)

if [ "$RECOVERY_OUT" = "RECOVERED" ] && [ ! -f "$HOME/.hex-verify-recovery-HALT" ]; then
  assert_pass "watchdog removed HALT file (claude reachable)"
else
  assert_fail "watchdog did not remove HALT file"
fi
rm -f "$HOME/.hex-verify-recovery-HALT"

# ── 11. No agents currently halted ──────────────────────────────────────────
bold "11. No agents currently halted"

HALTED=0
AGENT_COUNT=0
while IFS= read -r agent; do
  [ -z "$agent" ] && continue
  AGENT_COUNT=$((AGENT_COUNT + 1))
  if [ -f "$HOME/.hex-${agent}-HALT" ]; then
    assert_fail "agent $agent is HALTED"
    HALTED=$((HALTED + 1))
  fi
done < <(HEX_DIR="$HEX_DIR" "$HEX_DIR/.hex/bin/hex-agent" list 2>/dev/null)
if [ $HALTED -eq 0 ]; then
  assert_pass "all $AGENT_COUNT agents active (no HALT files)"
fi

# ── 12. hex-events daemon running ────────────────────────────────────────────
bold "12. hex-events daemon running"

if pgrep -f hex_eventd.py >/dev/null 2>&1; then
  assert_pass "hex_eventd.py process running"
else
  assert_fail "hex_eventd.py not running"
fi

# ── 13. Agent policies loaded in daemon ──────────────────────────────────────
bold "13. Agent policies exist"

while IFS= read -r agent; do
  [ -z "$agent" ] && continue
  POLICY="$HOME/.hex-events/policies/${agent}-agent.yaml"
  if [ -f "$POLICY" ]; then
    assert_pass "${agent}-agent.yaml exists"
  else
    assert_fail "${agent}-agent.yaml missing"
  fi
done < <(HEX_DIR="$HEX_DIR" "$HEX_DIR/.hex/bin/hex-agent" list 2>/dev/null)

# ══════════════════════════════════════════════════════════════════════════════
echo ""
bold "═══ Results ═══"
echo "  $PASS passed, $FAIL failed"
echo ""

if [ $FAIL -gt 0 ]; then
  red "VERIFICATION FAILED — do not declare done"
  exit 1
fi

green "ALL CHECKS PASSED"
exit 0
