#!/usr/bin/env bash
# test-doctor-codex.sh — Verify doctor.sh includes Codex CLI health checks.
#
# Asserts:
#   1. doctor.sh exists and is executable
#   2. doctor-checks/codex.sh exists (the Codex-specific check module)
#   3. doctor.sh references or sources the codex check
#   4. Running bash -n on codex.sh passes (syntax check)
#   5. When codex CLI is present, the check reports PASS (not ERROR)

set -uo pipefail

PASS=0
FAIL=0
SKIP=0
TOTAL=0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

DOCTOR_SH="$REPO_DIR/system/scripts/doctor.sh"
CODEX_CHECK="$REPO_DIR/system/scripts/doctor-checks/codex.sh"

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
        echo "  FAIL: $desc — '$pattern' not found in $(basename "$file")"
        FAIL=$((FAIL + 1))
    fi
}

skip() {
    SKIP=$((SKIP + 1))
    echo "  SKIP: $1"
}

echo "=== test-doctor-codex ==="
echo ""

echo "[1] doctor.sh exists and is executable"
check "doctor.sh exists"      test -f "$DOCTOR_SH"
check "doctor.sh executable"  test -x "$DOCTOR_SH"

echo "[2] codex.sh doctor check exists"
check "doctor-checks/codex.sh exists"      test -f "$CODEX_CHECK"
check "doctor-checks/codex.sh syntax ok"   bash -n "$CODEX_CHECK"

echo "[3] codex.sh covers required checks"
check_contains "check for codex on PATH"      "command -v codex\|codex.*PATH"        "$CODEX_CHECK"
check_contains "check codex --version"        "codex.*--version\|version"            "$CODEX_CHECK"
check_contains "check OPENAI_API_KEY"         "OPENAI_API_KEY"                       "$CODEX_CHECK"
check_contains "check AGENTS.md exists"       "AGENTS.md"                            "$CODEX_CHECK"

echo "[4] doctor.sh integrates Codex checks"
check_contains "doctor.sh sources/calls codex" "codex" "$DOCTOR_SH"

echo "[5] Live check: codex CLI presence triggers PASS"
HAVE_CODEX="no"
command -v codex &>/dev/null && HAVE_CODEX="yes"

if [ "$HAVE_CODEX" = "yes" ]; then
    # Source the check module in a subshell with minimal stub environment
    TOTAL=$((TOTAL + 1))
    STUB_SCRIPT="/tmp/codex-check-stub-$$.sh"
    cat > "$STUB_SCRIPT" << STUBEOF
#!/usr/bin/env bash
_pass()  { echo "PASS: \$*"; }
_warn()  { echo "WARN: \$*"; }
_error() { echo "ERROR: \$*"; }
_info()  { :; }
_fixed() { :; }
_rec()   { :; }
HEX_DIR='/tmp'
AGENT_DIR='/tmp'
FIX=false
SMOKE=false
source '$CODEX_CHECK'
check_codex_1
STUBEOF
    CHECK_OUT=$(bash "$STUB_SCRIPT" 2>&1 || true)
    rm -f "$STUB_SCRIPT"
    if echo "$CHECK_OUT" | grep -qi "PASS.*codex.*path\|found at"; then
        echo "  PASS: codex CLI presence detected correctly"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: codex CLI presence check did not return PASS (output: $CHECK_OUT)"
        FAIL=$((FAIL + 1))
    fi
else
    skip "codex CLI not on PATH — skipping live CLI presence check"
fi

echo ""
echo "  Results: $PASS passed, $FAIL failed, $SKIP skipped ($TOTAL total)"
echo ""

if [ "$FAIL" -gt 0 ]; then
    echo "=== test-doctor-codex: FAIL ==="
    exit 1
fi
echo "=== test-doctor-codex: PASS ==="
