#!/usr/bin/env bash
# Test: hex-doctor invokes external health scripts for hex-events and vector search.
#
# The v0.13.2 design moved policy-load checking out of inline check_66 into
# external scripts (check-hex-events-policy-load.sh, check-vector-search.sh).
# hex-doctor guards each with `if [ -f "$SCRIPT" ]` so absent scripts are skipped.
#
# Asserts:
#   - No external scripts → modules skipped, doctor exits 0
#   - policy-load script present, daemon log clean → doctor shows PASS
#   - policy-load script present, daemon log has errors → doctor shows ERROR
#   - policy-load script present, no daemon log → doctor skips gracefully
#
# No hex binary stub needed — the new scripts don't call `hex events status`.
set -uo pipefail

PASS=0
FAIL=0
TOTAL=0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HEX_DOCTOR="$SCRIPT_DIR/../system/scripts/hex-doctor"
POLICY_LOAD_SCRIPT="$SCRIPT_DIR/../system/scripts/health/check-hex-events-policy-load.sh"

# ── Helpers ───────────────────────────────────────────────────────────────────

assert_exit() {
  local name="$1" expected="$2" actual="$3"
  TOTAL=$((TOTAL + 1))
  if [ "$actual" -eq "$expected" ]; then
    echo "  PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $name (exit $actual, expected $expected)"
    FAIL=$((FAIL + 1))
  fi
}

assert_contains() {
  local name="$1" pattern="$2" output="$3"
  TOTAL=$((TOTAL + 1))
  if printf '%s' "$output" | grep -q "$pattern"; then
    echo "  PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $name (pattern '$pattern' not found in output)"
    FAIL=$((FAIL + 1))
    printf '%s' "$output" | tail -10 | sed 's/^/    /' >&2
  fi
}

assert_not_contains() {
  local name="$1" pattern="$2" output="$3"
  TOTAL=$((TOTAL + 1))
  if ! printf '%s' "$output" | grep -q "$pattern"; then
    echo "  PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $name (unexpected pattern '$pattern' found in output)"
    FAIL=$((FAIL + 1))
  fi
}

run_doctor() {
  DOCTOR_EXIT=0
  DOCTOR_OUT=$(HEX_DIR="$1" bash "$HEX_DOCTOR" 2>&1) || DOCTOR_EXIT=$?
}

# ── Setup ─────────────────────────────────────────────────────────────────────

FAKE_HEX=$(mktemp -d)
trap 'rm -rf "$FAKE_HEX"' EXIT

mkdir -p "$FAKE_HEX/.hex/scripts/health"
mkdir -p "$FAKE_HEX/.hex/scripts/doctor-checks"
mkdir -p "$FAKE_HEX/.hex/bin"

cat > "$FAKE_HEX/.hex/scripts/doctor.sh" << 'DOCEOF'
#!/bin/bash
echo ""
echo "hex-doctor-stub"
echo "  ─────────────────────────────────────────"
echo "  [PASS] stub: ok"
echo "  Summary: 1 passed, 0 warnings, 0 errors, 0 fixed"
DOCEOF
chmod +x "$FAKE_HEX/.hex/scripts/doctor.sh"

echo "=== test-doctor-events-coverage ==="
echo ""

# ── Test 1: No external scripts → modules skipped, doctor exits 0 ─────────────
echo "[1] No external check scripts: modules skipped gracefully"

run_doctor "$FAKE_HEX"
TOTAL=$((TOTAL + 1))
if [ "$DOCTOR_EXIT" -ne 1 ]; then
  echo "  PASS: doctor exits 0 or 2 (no errors when scripts absent)"
  PASS=$((PASS + 1))
else
  echo "  FAIL: doctor exits 1 (unexpected error when scripts absent)"
  FAIL=$((FAIL + 1))
fi
assert_not_contains "no ERROR when scripts absent"  "ERROR"  "$DOCTOR_OUT"

# ── Test 2: policy-load script present, no daemon log → SKIP (not ERROR) ──────
echo ""
echo "[2] Policy-load script present, daemon log absent: SKIP (not ERROR)"

cp "$POLICY_LOAD_SCRIPT" "$FAKE_HEX/.hex/scripts/health/check-hex-events-policy-load.sh"
chmod +x "$FAKE_HEX/.hex/scripts/health/check-hex-events-policy-load.sh"

DAEMON_LOG="$FAKE_HEX/daemon-absent.log"  # doesn't exist
DOCTOR_EXIT=0
DOCTOR_OUT=$(HEX_DIR="$FAKE_HEX" HEX_EVENTS_DAEMON_LOG="$DAEMON_LOG" bash "$HEX_DOCTOR" 2>&1) || DOCTOR_EXIT=$?

TOTAL=$((TOTAL + 1))
if [ "$DOCTOR_EXIT" -ne 1 ]; then
  echo "  PASS: exit code is not 1 (absent daemon log is a SKIP, not ERROR)"
  PASS=$((PASS + 1))
else
  echo "  FAIL: exit code is 1 (absent daemon log should be SKIP, not ERROR)"
  FAIL=$((FAIL + 1))
fi
assert_not_contains "no ERROR for absent daemon log"   "ERROR"  "$DOCTOR_OUT"
assert_contains     "SKIP shown for absent daemon log" "SKIP"   "$DOCTOR_OUT"

# ── Test 3: policy-load script present, daemon log clean → PASS ───────────────
echo ""
echo "[3] Policy-load script present, daemon log clean: PASS"

DAEMON_LOG="$FAKE_HEX/daemon-clean.log"
# Recent normal daemon log line (no POLICY errors)
echo "$(date -u '+%Y-%m-%d %H:%M:%S'),000 hex-events INFO Loaded 5 policies" > "$DAEMON_LOG"

DOCTOR_EXIT=0
DOCTOR_OUT=$(HEX_DIR="$FAKE_HEX" HEX_EVENTS_DAEMON_LOG="$DAEMON_LOG" bash "$HEX_DOCTOR" 2>&1) || DOCTOR_EXIT=$?

TOTAL=$((TOTAL + 1))
if [ "$DOCTOR_EXIT" -ne 1 ]; then
  echo "  PASS: exit code is not 1 (clean log has no errors)"
  PASS=$((PASS + 1))
else
  echo "  FAIL: exit code is 1 (clean log should not produce errors)"
  FAIL=$((FAIL + 1))
fi
assert_not_contains "no ERROR for clean daemon log"  "ERROR"  "$DOCTOR_OUT"

# ── Test 4: policy-load script present, daemon log has POLICY LOAD ERROR ──────
echo ""
echo "[4] Policy-load script present, daemon log has errors: ERROR"

DAEMON_LOG="$FAKE_HEX/daemon-errors.log"
# Simulate a POLICY LOAD ERROR entry within the last 2h
NOW_UTC="$(date -u '+%Y-%m-%d %H:%M:%S')"
echo "${NOW_UTC},000 hex-events ERROR [POLICY LOAD ERROR] /hex/policies/broken-policy.yaml: duplicate field 'timeout'" > "$DAEMON_LOG"

DOCTOR_EXIT=0
DOCTOR_OUT=$(HEX_DIR="$FAKE_HEX" HEX_EVENTS_DAEMON_LOG="$DAEMON_LOG" bash "$HEX_DOCTOR" 2>&1) || DOCTOR_EXIT=$?

assert_exit "exit code is 1 (errors from policy-load check)" 1 "$DOCTOR_EXIT"
assert_contains "ERROR shown in doctor output"               "ERROR"              "$DOCTOR_OUT"
assert_contains "failing policy path named"                  "broken-policy.yaml" "$DOCTOR_OUT"

# ── Summary ───────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Results: $PASS passed, $FAIL failed, $TOTAL total"
echo ""

if [ "$FAIL" -eq 0 ]; then
  echo "  All tests PASS"
  exit 0
else
  echo "  $FAIL test(s) FAILED"
  exit 1
fi
