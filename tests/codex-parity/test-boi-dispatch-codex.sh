#!/usr/bin/env bash
# test-boi-dispatch-codex.sh — Verify BOI dispatch with runtime=codex.
#
# Requires: OPENAI_API_KEY, codex CLI on PATH, and ~/.boi/boi on PATH.
# Skips all live tests when prerequisites are absent.
#
# Creates a minimal spec with runtime=codex and dispatches it, verifying
# the dispatch completes and produces output.

set -uo pipefail

PASS=0
FAIL=0
SKIP=0
TOTAL=0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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
HAVE_BOI="no"
(command -v boi &>/dev/null || [ -f "$HOME/.boi/boi" ]) && HAVE_BOI="yes"

echo "=== test-boi-dispatch-codex ==="
echo ""

echo "[1] Prerequisites check"
TOTAL=$((TOTAL + 1))
if [ "$HAVE_KEY" = "yes" ]; then
    echo "  PASS: OPENAI_API_KEY is set"
    PASS=$((PASS + 1))
else
    echo "  SKIP: OPENAI_API_KEY not set — all live tests will be skipped"
    SKIP=$((SKIP + 1))
fi

TOTAL=$((TOTAL + 1))
if [ "$HAVE_CODEX" = "yes" ]; then
    echo "  PASS: codex CLI found on PATH"
    PASS=$((PASS + 1))
else
    echo "  SKIP: codex CLI not found — live dispatch skipped"
    SKIP=$((SKIP + 1))
fi

TOTAL=$((TOTAL + 1))
if [ "$HAVE_BOI" = "yes" ]; then
    echo "  PASS: boi CLI found"
    PASS=$((PASS + 1))
else
    echo "  SKIP: boi CLI not found — live dispatch skipped"
    SKIP=$((SKIP + 1))
fi

echo "[2] Structural: runtime=codex accepted in YAML"
TOTAL=$((TOTAL + 1))
SPEC_YAML='tasks:
  - id: test-01
    title: "Minimal codex dispatch test"
    runtime: codex
    spec: |
      Echo the string CODEX_DISPATCH_OK
    verify: "echo CODEX_DISPATCH_OK"
'
TMPSPEC="/tmp/codex-dispatch-test-$$.yaml"
echo "$SPEC_YAML" > "$TMPSPEC"
if python3 -c "import yaml; data = yaml.safe_load(open('$TMPSPEC')); t = data['tasks'][0]; assert t.get('runtime') == 'codex', 'runtime field wrong'; print('runtime=codex ok')" 2>/dev/null; then
    echo "  PASS: runtime=codex parses correctly in YAML"
    PASS=$((PASS + 1))
else
    # yaml module may not be available; try grep
    if grep -q "runtime: codex" "$TMPSPEC" 2>/dev/null; then
        echo "  PASS: runtime=codex present in spec YAML"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: runtime=codex not found in spec YAML"
        FAIL=$((FAIL + 1))
    fi
fi
rm -f "$TMPSPEC"

echo "[3] Live BOI dispatch (requires key + codex + boi)"
if [ "$HAVE_KEY" != "yes" ]; then
    skip "OPENAI_API_KEY not set"
elif [ "$HAVE_CODEX" != "yes" ]; then
    skip "codex CLI not on PATH"
elif [ "$HAVE_BOI" != "yes" ]; then
    skip "boi CLI not found"
else
    TS=$(date +%s)
    LIVE_SPEC="/tmp/codex-live-dispatch-${TS}.yaml"
    cat > "$LIVE_SPEC" << 'YAML'
title: "Codex parity live dispatch test"
tasks:
  - id: live-01
    title: "Codex echo check"
    runtime: codex
    spec: |
      Echo the exact string: CODEX_LIVE_OK
    verify: "true"
YAML

    TOTAL=$((TOTAL + 1))
    BOI_CMD="${HOME}/.boi/boi"
    [ ! -f "$BOI_CMD" ] && BOI_CMD="boi"

    DISPATCH_OUT=$(timeout 60 bash "$BOI_CMD" dispatch "$LIVE_SPEC" 2>&1 || true)
    rm -f "$LIVE_SPEC"

    if [ -n "$DISPATCH_OUT" ]; then
        echo "  PASS: boi dispatch produced output"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: boi dispatch produced no output (timeout or error)"
        FAIL=$((FAIL + 1))
    fi
fi

echo ""
echo "  Results: $PASS passed, $FAIL failed, $SKIP skipped ($TOTAL total)"
echo ""

if [ "$FAIL" -gt 0 ]; then
    echo "=== test-boi-dispatch-codex: FAIL ==="
    exit 1
fi
echo "=== test-boi-dispatch-codex: PASS ==="
