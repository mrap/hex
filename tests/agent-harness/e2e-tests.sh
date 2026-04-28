#!/usr/bin/env bash
# e2e-tests.sh — Agent harness end-to-end tests
# Runs in Docker to isolate from real fleet state.
set -uo pipefail

PASS=0
FAIL=0
HEX_AGENT="$HEX_DIR/.hex/bin/hex-agent"
HEX_BIN="$HEX_DIR/.hex/bin/hex"

red()   { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
bold()  { printf '\033[1m%s\033[0m\n' "$*"; }

assert_pass() { PASS=$((PASS + 1)); green "  ✓ $*"; }
assert_fail() { FAIL=$((FAIL + 1)); red "  ✗ $*"; }

assert_exit() {
  local expected=$1 actual=$2 desc="$3"
  if [ "$actual" -eq "$expected" ]; then
    assert_pass "$desc (exit $actual)"
  else
    assert_fail "$desc (expected exit $expected, got $actual)"
  fi
}

assert_contains() {
  local output="$1" pattern="$2" desc="$3"
  if echo "$output" | grep -q "$pattern"; then
    assert_pass "$desc"
  else
    assert_fail "$desc — expected '$pattern' in output"
  fi
}

assert_not_contains() {
  local output="$1" pattern="$2" desc="$3"
  if echo "$output" | grep -q "$pattern"; then
    assert_fail "$desc — found '$pattern' in output (should be absent)"
  else
    assert_pass "$desc"
  fi
}

# Helper: create a valid charter
create_charter() {
  local id="$1" core="${2:-false}"
  mkdir -p "$HEX_DIR/projects/$id"
  cat > "$HEX_DIR/projects/$id/charter.yaml" <<YAML
id: $id
name: Test Agent $id
role: test agent
scope: testing
wake:
  triggers: []
  responsibilities: []
authority:
  green: []
  yellow: []
  red: []
budget:
  wakes_per_hour: 4
  usd_per_day: 5.0
  usd_per_shift: 1.0
kill_switch: "~/.hex-${id}-HALT"
core: $core
YAML
}

# Helper: create a reference core charter
create_reference() {
  local id="$1"
  cat > "$HEX_DIR/.hex/reference/core-agents/$id.yaml" <<YAML
id: $id
name: Reference $id
role: core system agent
scope: system operations
wake:
  triggers: []
  responsibilities: []
authority:
  green: []
  yellow: []
  red: []
budget:
  wakes_per_hour: 4
  usd_per_day: 5.0
  usd_per_shift: 1.0
kill_switch: "~/.hex-${id}-HALT"
core: true
YAML
}

cleanup() {
  rm -rf "$HEX_DIR/projects/"*/
  rm -rf "$HEX_DIR/.hex/reference/core-agents/"*.yaml
  rm -f "$HOME"/.hex-*-HALT
}

echo ""
bold "═══════════════════════════════════════════"
bold "  Hex Agent Harness — E2E Tests"
bold "═══════════════════════════════════════════"
echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "1. Charter-driven discovery"
# ══════════════════════════════════════════════════════════════════════════════

cleanup
out=$("$HEX_AGENT" fleet 2>&1); rc=$?
assert_exit 1 $rc "fleet with no agents exits non-zero"
assert_contains "$out" "no agents found" "fleet says no agents found"

create_charter "alpha"
create_charter "beta"
create_charter "gamma"

out=$("$HEX_AGENT" list 2>&1); rc=$?
assert_exit 0 $rc "list with 3 agents exits 0"
count=$(echo "$out" | wc -l | tr -d ' ')
if [ "$count" -eq 3 ]; then
  assert_pass "list shows exactly 3 agents"
else
  assert_fail "list shows $count agents, expected 3"
fi

out=$("$HEX_AGENT" fleet 2>&1); rc=$?
assert_exit 0 $rc "fleet with 3 valid agents exits 0"
assert_contains "$out" "alpha" "fleet shows alpha"
assert_contains "$out" "beta" "fleet shows beta"
assert_contains "$out" "gamma" "fleet shows gamma"

echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "2. Charter ID mismatch rejection"
# ══════════════════════════════════════════════════════════════════════════════

cleanup
create_charter "good-agent"
mkdir -p "$HEX_DIR/projects/bad-agent"
cat > "$HEX_DIR/projects/bad-agent/charter.yaml" <<YAML
id: wrong-name
name: Bad Agent
role: test
wake:
  triggers: []
  responsibilities: []
authority:
  green: []
  yellow: []
  red: []
budget:
  wakes_per_hour: 1
  usd_per_day: 1.0
  usd_per_shift: 0.5
kill_switch: "~/.hex-bad-agent-HALT"
YAML

out=$("$HEX_AGENT" fleet 2>&1); rc=$?
assert_exit 1 $rc "fleet exits non-zero on id mismatch"
assert_contains "$out" "charter.id is 'wrong-name'" "error message names the mismatch"
assert_contains "$out" "must match directory name" "error message explains the rule"

echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "3. Wake rejects unregistered agent"
# ══════════════════════════════════════════════════════════════════════════════

cleanup
out=$("$HEX_AGENT" wake nonexistent --trigger test 2>&1); rc=$?
assert_exit 1 $rc "wake nonexistent agent exits non-zero"
assert_contains "$out" "not registered" "error says agent is not registered"
assert_contains "$out" "charter.yaml IS registration" "error explains registration"

echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "4. Core agent markers"
# ══════════════════════════════════════════════════════════════════════════════

cleanup
create_charter "core-agent" true
create_charter "user-agent" false

out=$("$HEX_AGENT" fleet 2>&1); rc=$?
assert_exit 0 $rc "fleet with mixed core/non-core exits 0"
assert_contains "$out" "●" "fleet shows core marker"

out=$("$HEX_AGENT" list --core 2>&1)
assert_contains "$out" "core-agent" "list --core includes core agent"
assert_not_contains "$out" "user-agent" "list --core excludes non-core agent"

out=$("$HEX_AGENT" list 2>&1)
count=$(echo "$out" | wc -l | tr -d ' ')
if [ "$count" -eq 2 ]; then
  assert_pass "list (no flag) shows all agents"
else
  assert_fail "list shows $count agents, expected 2"
fi

echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "5. Core agent HALT warning"
# ══════════════════════════════════════════════════════════════════════════════

cleanup
create_charter "ops" true
touch "$HOME/.hex-ops-HALT"

out=$("$HEX_AGENT" fleet 2>&1); rc=$?
assert_exit 0 $rc "fleet still exits 0 with halted core (warning, not error)"
assert_contains "$out" "HALTED" "fleet warns about halted core agent"
assert_contains "$out" "self-healing may be degraded" "warning explains impact"

rm -f "$HOME/.hex-ops-HALT"

echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "6. Core drift detection — missing agent"
# ══════════════════════════════════════════════════════════════════════════════

cleanup
create_reference "ops"
create_reference "cos"
create_reference "fleet-lead"

create_charter "ops" true
create_charter "cos" true
# fleet-lead intentionally missing

out=$("$HEX_AGENT" check-core 2>&1); rc=$?
assert_exit 1 $rc "check-core exits non-zero when core agent is missing"
assert_contains "$out" "MISSING: fleet-lead" "check-core names the missing agent"
assert_contains "$out" "2/3 healthy" "check-core shows correct healthy count"
assert_contains "$out" "restore-core" "check-core suggests the fix command"

echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "7. Core drift detection — broken agent (core: false)"
# ══════════════════════════════════════════════════════════════════════════════

cleanup
create_reference "ops"
create_charter "ops" false

out=$("$HEX_AGENT" check-core 2>&1); rc=$?
assert_exit 1 $rc "check-core exits non-zero when core agent has core: false"
assert_contains "$out" "BROKEN" "check-core flags broken agent"
assert_contains "$out" "core: false" "explains the problem"

echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "8. restore-core — restores missing, skips existing, ignores user agents"
# ══════════════════════════════════════════════════════════════════════════════

cleanup
create_reference "ops"
create_reference "cos"
create_reference "fleet-lead"

create_charter "ops" true
create_charter "user-custom" false

out=$("$HEX_AGENT" restore-core 2>&1); rc=$?
assert_exit 0 $rc "restore-core exits 0"
assert_contains "$out" "SKIP: ops" "restore-core skips existing core agent"
assert_contains "$out" "RESTORED: cos" "restore-core restores missing cos"
assert_contains "$out" "RESTORED: fleet-lead" "restore-core restores missing fleet-lead"
assert_not_contains "$out" "user-custom" "restore-core doesn't mention user agents"

# Verify restored charters exist
if [ -f "$HEX_DIR/projects/cos/charter.yaml" ]; then
  assert_pass "cos charter file created by restore"
else
  assert_fail "cos charter file not created"
fi

if [ -f "$HEX_DIR/projects/fleet-lead/charter.yaml" ]; then
  assert_pass "fleet-lead charter file created by restore"
else
  assert_fail "fleet-lead charter file not created"
fi

# Verify user agent was not touched
if [ -f "$HEX_DIR/projects/user-custom/charter.yaml" ]; then
  assert_pass "user-custom agent preserved (not touched)"
else
  assert_fail "user-custom agent was deleted (should be preserved)"
fi

# Verify check-core now passes
out=$("$HEX_AGENT" check-core 2>&1); rc=$?
assert_exit 0 $rc "check-core passes after restore"
assert_contains "$out" "3/3 healthy" "all 3 core agents healthy after restore"

echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "9. restore-core never overwrites existing charters"
# ══════════════════════════════════════════════════════════════════════════════

cleanup
create_reference "ops"

# Create ops with a custom role (user modification)
mkdir -p "$HEX_DIR/projects/ops"
cat > "$HEX_DIR/projects/ops/charter.yaml" <<YAML
id: ops
name: My Custom Ops
role: my customized ops agent
wake:
  triggers: []
  responsibilities: []
authority:
  green: []
  yellow: []
  red: []
budget:
  wakes_per_hour: 10
  usd_per_day: 99.0
  usd_per_shift: 10.0
kill_switch: "~/.hex-ops-HALT"
core: true
YAML

"$HEX_AGENT" restore-core >/dev/null 2>&1

# Verify the custom charter was NOT overwritten
role=$(grep 'role:' "$HEX_DIR/projects/ops/charter.yaml" | head -1)
if echo "$role" | grep -q "my customized"; then
  assert_pass "restore-core preserved user's custom charter"
else
  assert_fail "restore-core overwrote user's custom charter"
fi

echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "10. Invalid charter validation"
# ══════════════════════════════════════════════════════════════════════════════

cleanup

# Missing budget
mkdir -p "$HEX_DIR/projects/bad-budget"
cat > "$HEX_DIR/projects/bad-budget/charter.yaml" <<YAML
id: bad-budget
name: Bad Budget
role: test
wake:
  triggers: []
  responsibilities: []
authority:
  green: []
  yellow: []
  red: []
budget:
  wakes_per_hour: 0
  usd_per_day: 0
  usd_per_shift: 0
kill_switch: "~/.hex-bad-budget-HALT"
YAML

out=$("$HEX_AGENT" fleet 2>&1); rc=$?
assert_exit 1 $rc "fleet rejects charter with zero budget"
assert_contains "$out" "invalid charter" "error mentions invalid charter"

echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "11. Message send/receive (unified store)"
# ══════════════════════════════════════════════════════════════════════════════

cleanup
create_charter "sender"
create_charter "receiver"

out=$("$HEX_AGENT" message sender receiver --subject "test msg" --body "hello" 2>&1); rc=$?
assert_exit 0 $rc "message send exits 0"

# Verify via hex message list (unified SQLite store) instead of legacy JSONL file
out=$("$HEX_BIN" message list 2>&1); rc=$?
assert_exit 0 $rc "message list exits 0 after send"
assert_contains "$out" "test msg\|hello\|sender\|receiver" "message appears in unified store"

echo ""

# ══════════════════════════════════════════════════════════════════════════════
bold "12. Messaging primitive"
# ══════════════════════════════════════════════════════════════════════════════

cleanup

# Create a comment-type message
out=$("$HEX_BIN" message send mike brand --content "test comment" --msg-type comment 2>&1); rc=$?
assert_exit 0 $rc "message send (comment type) exits 0"

# Create an agent-type message
out=$("$HEX_BIN" message send brand cos --content "status update" 2>&1); rc=$?
assert_exit 0 $rc "message send (agent type) exits 0"

# List messages
out=$("$HEX_BIN" message list 2>&1); rc=$?
assert_exit 0 $rc "message list exits 0"
assert_contains "$out" "test comment\|status update" "messages appear in list"

echo ""

# ══════════════════════════════════════════════════════════════════════════════
# Summary
# ══════════════════════════════════════════════════════════════════════════════

echo ""
bold "═══════════════════════════════════════════"
total=$((PASS + FAIL))
if [ $FAIL -eq 0 ]; then
  green "  ALL $total TESTS PASSED"
else
  red "  $FAIL/$total TESTS FAILED"
fi
bold "═══════════════════════════════════════════"
echo ""

exit $FAIL
