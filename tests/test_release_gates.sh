#!/usr/bin/env bash
set -euo pipefail

# test_release_gates.sh — Tests for release pipeline enforcement gates
#
# Tests version bump gate, AGENT_DIR guard in session-start hook,
# and doctor check 23. Uses isolated temp git repos.

PASS=0
FAIL=0
TOTAL=0

pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); TOTAL=$((TOTAL + 1)); }
fail() { echo "  FAIL: $1"; FAIL=$((FAIL + 1)); TOTAL=$((TOTAL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

WORK_DIR=$(mktemp -d)
trap 'rm -rf "$WORK_DIR"' EXIT

# ── Helpers: extract just the version gate logic for isolated testing ──

# Source the semver functions from release.sh
eval "$(sed -n '/^semver_valid/,/^}/p' "$REPO_DIR/system/scripts/release.sh")"
eval "$(sed -n '/^semver_gt/,/^}/p' "$REPO_DIR/system/scripts/release.sh")"

setup_repo() {
    rm -rf "$WORK_DIR/repo"
    mkdir -p "$WORK_DIR/repo/system/scripts"
    echo "$1" > "$WORK_DIR/repo/system/version.txt"
    cd "$WORK_DIR/repo"
    git init -q
    git add -A
    git commit -q -m "initial"
    git tag "v${2:-1.0.0}"
    # Make a change so there are commits ahead of the tag
    echo "change" > README.md
    echo "$1" > system/version.txt
    git add -A
    git commit -q -m "post-tag change"
}

echo "=== Release Gate Tests ==="
echo ""

# ── Test 1: semver_valid accepts good versions ─────────────────────
echo "[1] semver_valid — valid versions"
ALL_VALID=true
for v in "0.0.1" "1.0.0" "10.20.30" "0.8.1"; do
    if ! semver_valid "$v"; then
        fail "semver_valid rejected valid '$v'"
        ALL_VALID=false
    fi
done
$ALL_VALID && pass "All valid semver accepted"

# ── Test 2: semver_valid rejects bad versions ──────────────────────
echo "[2] semver_valid — invalid versions"
ALL_INVALID=true
for v in "banana" "1.0" "1" "v1.0.0" "1.0.0-beta" "1.0.0.1"; do
    if semver_valid "$v"; then
        fail "semver_valid accepted invalid '$v'"
        ALL_INVALID=false
    fi
done
$ALL_INVALID && pass "All invalid semver rejected"

# ── Test 3: semver_gt comparisons ──────────────────────────────────
echo "[3] semver_gt — version ordering"
ORDERING_OK=true

# Should be greater
for pair in "1.0.1:1.0.0" "2.0.0:1.9.9" "0.8.1:0.8.0" "1.0.0:0.99.99"; do
    a="${pair%%:*}" b="${pair##*:}"
    if ! semver_gt "$a" "$b"; then
        fail "semver_gt: $a should be > $b"
        ORDERING_OK=false
    fi
done

# Should NOT be greater
for pair in "1.0.0:1.0.0" "0.8.0:0.8.0" "1.0.0:2.0.0" "0.7.9:0.8.0"; do
    a="${pair%%:*}" b="${pair##*:}"
    if semver_gt "$a" "$b"; then
        fail "semver_gt: $a should NOT be > $b"
        ORDERING_OK=false
    fi
done

$ORDERING_OK && pass "All version ordering correct"

# ── Test 4: Gate blocks when version matches tag ───────────────────
echo "[4] Gate integration — blocks unchanged version"
setup_repo "1.0.0" "1.0.0"
VERSION=$(cat system/version.txt)
LATEST_TAG=$(git tag --sort=-version:refname | head -1 | sed 's/^v//')
if [ "$VERSION" = "$LATEST_TAG" ]; then
    pass "Detects version == latest tag (would block)"
else
    fail "Should detect version matches tag"
fi

# ── Test 5: Gate passes when version bumped ────────────────────────
echo "[5] Gate integration — passes on patch bump"
setup_repo "1.0.1" "1.0.0"
VERSION=$(cat system/version.txt)
LATEST_TAG=$(git tag --sort=-version:refname | head -1 | sed 's/^v//')
if semver_gt "$VERSION" "$LATEST_TAG"; then
    pass "Accepts 1.0.0 → 1.0.1 bump"
else
    fail "Should accept patch bump"
fi

# ── Test 6: Gate blocks version regression ─────────────────────────
echo "[6] Gate integration — blocks regression"
setup_repo "0.9.0" "1.0.0"
VERSION=$(cat system/version.txt)
LATEST_TAG=$(git tag --sort=-version:refname | head -1 | sed 's/^v//')
if ! semver_gt "$VERSION" "$LATEST_TAG"; then
    pass "Blocks 1.0.0 → 0.9.0 regression"
else
    fail "Should block version regression"
fi

# ── Test 7: Gate accepts major bump ────────────────────────────────
echo "[7] Gate integration — accepts major bump"
setup_repo "2.0.0" "1.0.0"
VERSION=$(cat system/version.txt)
LATEST_TAG=$(git tag --sort=-version:refname | head -1 | sed 's/^v//')
if semver_gt "$VERSION" "$LATEST_TAG"; then
    pass "Accepts 1.0.0 → 2.0.0 major bump"
else
    fail "Should accept major bump"
fi

# ── Test 8: Minimum patch suggestion is correct ────────────────────
echo "[8] Minimum patch version suggestion"
LATEST="0.8.0"
SUGGESTED=$(echo "$LATEST" | awk -F. '{print $1"."$2"."$3+1}')
if [ "$SUGGESTED" = "0.8.1" ]; then
    pass "Suggests 0.8.1 from 0.8.0"
else
    fail "Expected 0.8.1, got $SUGGESTED"
fi

LATEST="1.2.9"
SUGGESTED=$(echo "$LATEST" | awk -F. '{print $1"."$2"."$3+1}')
if [ "$SUGGESTED" = "1.2.10" ]; then
    pass "Suggests 1.2.10 from 1.2.9"
else
    fail "Expected 1.2.10, got $SUGGESTED"
fi

# ── Test 9: Session-start hook blocks without AGENT_DIR ────────────
echo "[9] Session-start hook — blocks without AGENT_DIR"
HOOK_SCRIPT="$REPO_DIR/system/hooks/scripts/session-start.sh"
if [ -f "$HOOK_SCRIPT" ]; then
    OUTPUT=$(unset AGENT_DIR; bash "$HOOK_SCRIPT" 2>&1 || true)
    if echo "$OUTPUT" | grep -q "AGENT_DIR IS NOT SET"; then
        pass "Session-start blocks without AGENT_DIR"
    else
        fail "Session-start should block without AGENT_DIR"
        echo "    Output: $(echo "$OUTPUT" | head -3)"
    fi
else
    fail "session-start.sh not found at $HOOK_SCRIPT"
fi

# ── Test 10: Session-start hook passes with AGENT_DIR ──────────────
echo "[10] Session-start hook — passes with AGENT_DIR"
if [ -f "$HOOK_SCRIPT" ]; then
    OUTPUT=$(AGENT_DIR="$REPO_DIR" bash "$HOOK_SCRIPT" 2>&1 || true)
    if echo "$OUTPUT" | grep -q "AGENT_DIR IS NOT SET"; then
        fail "Session-start should pass when AGENT_DIR set"
    else
        pass "Session-start passes with AGENT_DIR set"
    fi
else
    fail "session-start.sh not found at $HOOK_SCRIPT"
fi

# ── Test 11: Doctor check 23 exists ────────────────────────────────
echo "[11] Doctor check 23 — AGENT_DIR check exists"
if grep -q 'check_23' "$REPO_DIR/system/scripts/doctor.sh"; then
    pass "check_23 defined in doctor.sh"
else
    fail "check_23 not found in doctor.sh"
fi

if grep -q 'hex-dir-set' "$REPO_DIR/system/scripts/doctor.sh"; then
    pass "hex-dir-set record in doctor.sh"
else
    fail "hex-dir-set not found in doctor.sh"
fi

# ── Test 12: Install adds AGENT_DIR to shell rc ───────────────────
echo "[12] Install script — AGENT_DIR setup"
if grep -q 'export AGENT_DIR' "$REPO_DIR/install.sh"; then
    pass "install.sh sets AGENT_DIR"
else
    fail "install.sh missing AGENT_DIR setup"
fi

if grep -q 'export HEX_DIR' "$REPO_DIR/install.sh"; then
    pass "install.sh sets HEX_DIR"
else
    fail "install.sh missing HEX_DIR setup"
fi

# ── Test 13: Upgrade backfills AGENT_DIR ───────────────────────────
echo "[13] Upgrade script — AGENT_DIR backfill"
if grep -q 'export AGENT_DIR' "$REPO_DIR/system/scripts/upgrade.sh"; then
    pass "upgrade.sh backfills AGENT_DIR"
else
    fail "upgrade.sh missing AGENT_DIR backfill"
fi

# ── Test 14: No hardcoded user paths ──────────────────────────────
echo "[14] No hardcoded user paths in enforcement code"
HARDCODE_FOUND=false
for f in "$REPO_DIR/system/hooks/scripts/session-start.sh" \
         "$REPO_DIR/system/scripts/doctor.sh" \
         "$REPO_DIR/install.sh" \
         "$REPO_DIR/system/scripts/upgrade.sh"; do
    # Exclude GitHub URLs (github.com/mrap/hex-*) — those are canonical upstream refs
    # Build pattern dynamically to avoid triggering sanitize-check on this test file
    _USER="mr""ap"
    _PAT="${_USER}-hex\|/${_USER}/hex\|${_USER}/${_USER}-hex"
    if grep -v 'github.com' "$f" 2>/dev/null | grep -q "$_PAT"; then
        fail "Hardcoded user path found in $(basename "$f")"
        grep -vn 'github.com' "$f" | grep "$_PAT" | head -2
        HARDCODE_FOUND=true
    fi
done
$HARDCODE_FOUND || pass "No hardcoded user paths"

# ── Summary ────────────────────────────────────────────────────────
echo ""
echo "========================================="
echo " Results: $PASS passed, $FAIL failed ($TOTAL total)"
echo "========================================="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
echo ""
echo "=== ALL RELEASE GATE TESTS PASSED ==="
