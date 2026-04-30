#!/usr/bin/env bash
# test-boi-upgrade.sh — Containerized upgrade E2E for BOI.
#
# Catches the stale-symlink bug where upgrading hex doesn't rebuild the
# binary, leaving new subcommands (e.g. `bench`) absent from the installed
# binary.  This is the test that would have caught the 2026-04-29 session's
# missing-`bench` incident.
#
# Flow in container:
#   1. Clone hex-foundation; checkout v0.7.0 (BOI v0.2.0)
#   2. Run install.sh — baseline install
#   3. Capture version + help-line count + binary mtime
#   4. Checkout HEAD (BOI v1.0.0); re-run install.sh — upgrade
#   5. Assert: version bumped, binary mtime newer, `bench` + others present
#   6a. Smoke dispatch (optional, requires ANTHROPIC_API_KEY)
#   6b. BAD case: corrupt symlink → run doctor → assert caught
#
# Usage:
#   Standalone:    bash tests/core-e2e/suites/test-boi-upgrade.sh
#   With dispatch: ANTHROPIC_API_KEY=<key> bash tests/core-e2e/suites/test-boi-upgrade.sh
#   Via run-all.sh: sourced automatically (shares global PASS/FAIL counters)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

if ! declare -f assert_pass >/dev/null 2>&1; then
    # shellcheck source=../helpers.sh
    source "$SCRIPT_DIR/../helpers.sh"
fi

echo ""
echo "=== BOI UPGRADE E2E (containerized) ==="

# ── Docker availability ───────────────────────────────────────────────────────
if ! command -v docker >/dev/null 2>&1; then
    assert_fail "boi-upgrade-prereq: docker not installed — cannot run containerized test"
    return 0 2>/dev/null || exit 1
fi
if ! docker info >/dev/null 2>&1; then
    assert_fail "boi-upgrade-prereq: docker daemon not running"
    return 0 2>/dev/null || exit 1
fi
assert_pass "boi-upgrade-prereq: docker available"

# ── Build or reuse Docker image ───────────────────────────────────────────────
# Reuse the same image as test-boi-install.sh — no redundant build.
IMAGE_TAG="hex-boi-install-test:latest"
BUILD_LOG="/tmp/boi-upgrade-build-$$.log"

if ! docker image inspect "$IMAGE_TAG" >/dev/null 2>&1; then
    yellow "  Building Docker image $IMAGE_TAG (first run — ~2-3 min)..."
    docker build -t "$IMAGE_TAG" - > "$BUILD_LOG" 2>&1 << 'DOCKERFILE_EOF'
FROM rust:latest
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl ca-certificates pkg-config libssl-dev python3 python3-pip sqlite3 \
    && rm -rf /var/lib/apt/lists/*
RUN useradd -m -s /bin/bash testuser
USER testuser
WORKDIR /home/testuser
DOCKERFILE_EOF
    BUILD_EXIT=$?
    rm -f "$BUILD_LOG"
    if [ "$BUILD_EXIT" -ne 0 ]; then
        assert_fail "boi-upgrade-docker-build: image build failed (exit $BUILD_EXIT)"
        return 0 2>/dev/null || exit 1
    fi
fi
assert_pass "boi-upgrade-docker-build: image ready ($IMAGE_TAG)"

# ── Inner script (runs inside the container as testuser) ─────────────────────
# Single-quoted heredoc: no host-side expansion. All $vars expand in-container.
read -r -d '' INNER_SCRIPT << 'INNER_EOF' || true
set -uo pipefail

PASS=0; FAIL=0
pass() { PASS=$((PASS+1)); echo "  PASS: $*"; }
fail() { FAIL=$((FAIL+1)); echo "  FAIL: $*"; }

on_exit() {
    echo ""
    echo "--- inner summary: ${PASS} passed, ${FAIL} failed ---"
    if [ "${FAIL:-0}" -gt 0 ]; then
        echo "--- boi daemon log (last 50 lines) ---"
        find "$HOME/.boi/logs" -name "*.log" 2>/dev/null \
            | xargs -r tail -n 50 2>/dev/null || true
    fi
    # Clean up for reruns
    rm -rf "$HOME/.boi" /tmp/hex "$HOME/github.com" 2>/dev/null || true
    exit "${FAIL}"
}
trap on_exit EXIT

export PATH="$HOME/.boi/bin:$PATH"

# ── 1. Clone hex-foundation + checkout old release ────────────────────────────
echo "--- 1. clone + checkout v0.7.0 ---"
if git clone /repo /tmp/hex > /tmp/clone.log 2>&1; then
    pass "clone: hex-foundation cloned to /tmp/hex"
else
    fail "clone: git clone failed"
    cat /tmp/clone.log
    exit 1
fi

cd /tmp/hex
UPGRADE_SHA=$(git rev-parse HEAD)

if git checkout v0.7.0 > /tmp/checkout-old.log 2>&1; then
    pass "checkout-old: checked out v0.7.0"
else
    fail "checkout-old: could not checkout v0.7.0"
    cat /tmp/checkout-old.log
    exit 1
fi

# ── 2. Install at v0.7.0 (BOI v0.2.0) ────────────────────────────────────────
echo "--- 2. baseline install (v0.7.0 / BOI v0.2.0) ---"
export HEX_NONINTERACTIVE=1 CI=1
if [ -d /boi/.git ]; then
    export HEX_BOI_REPO="file:///boi"
fi

if bash install.sh > /tmp/install-old.log 2>&1; then
    pass "install-old: install.sh (v0.7.0) exited 0"
else
    INSTALL_EXIT=$?
    fail "install-old: install.sh exited $INSTALL_EXIT"
    tail -50 /tmp/install-old.log
    exit 1
fi

BOI="$HOME/.boi/bin/boi"
if [ ! -x "$BOI" ]; then
    fail "baseline-binary: $BOI not executable after old install"
    exit 1
fi
pass "baseline-binary: $BOI is executable after old install"

BASELINE_VER=$("$BOI" version 2>&1 || echo "unknown")
BASELINE_HELP_LINES=$("$BOI" --help 2>&1 | wc -l | tr -d ' ')
BINARY_MTIME_BEFORE=$(stat -c %Y "$BOI" 2>/dev/null || echo "0")
pass "baseline-captured: version='$BASELINE_VER', help-lines=$BASELINE_HELP_LINES, mtime=$BINARY_MTIME_BEFORE"

# ── 3. Update checkout to HEAD ────────────────────────────────────────────────
echo "--- 3. update checkout to HEAD ---"
if git checkout "$UPGRADE_SHA" > /tmp/checkout-head.log 2>&1; then
    pass "checkout-head: updated to HEAD ($UPGRADE_SHA)"
else
    fail "checkout-head: could not checkout HEAD"
    cat /tmp/checkout-head.log
    exit 1
fi

NEW_BOI_VERSION=$(grep "^BOI_VERSION=" /tmp/hex/VERSIONS | cut -d= -f2)
echo "  HEAD VERSIONS BOI_VERSION=$NEW_BOI_VERSION (baseline was from v0.7.0)"

# Sleep 1 second to ensure binary mtime differs from baseline
sleep 1

# ── 4. Upgrade: re-run install.sh from HEAD ───────────────────────────────────
echo "--- 4. upgrade install (HEAD / $NEW_BOI_VERSION) ---"
if bash install.sh > /tmp/install-new.log 2>&1; then
    pass "install-new: install.sh (HEAD) exited 0"
else
    INSTALL_EXIT=$?
    fail "install-new: install.sh (HEAD) exited $INSTALL_EXIT"
    tail -50 /tmp/install-new.log
    exit 1
fi

# ── 5. Post-upgrade assertions ────────────────────────────────────────────────
echo "--- 5. post-upgrade assertions ---"

# 5a. Binary still executable
if [ -x "$BOI" ]; then
    pass "post-binary-exec: $BOI still executable after upgrade"
else
    fail "post-binary-exec: $BOI not executable after upgrade"
fi

# 5b. Symlink resolves to a real file (not dangling)
if [ -L "$BOI" ]; then
    RESOLVED=$(readlink -f "$BOI" 2>/dev/null || true)
    if [ -x "$RESOLVED" ]; then
        pass "symlink-resolve: symlink -> $RESOLVED (real executable)"
    else
        fail "symlink-resolve: symlink is dangling or non-executable: $RESOLVED"
    fi
else
    pass "symlink-resolve: $BOI is a regular (non-symlink) executable"
fi

# 5c. boi version reflects new BOI_VERSION from VERSIONS
EXPECTED_VER="$NEW_BOI_VERSION"
EXPECTED_BARE="${EXPECTED_VER#v}"
NEW_VER=$("$BOI" version 2>&1 || echo "unknown")
if echo "$NEW_VER" | grep -qF "$EXPECTED_BARE"; then
    pass "version-match: '$NEW_VER' contains '$EXPECTED_BARE' (VERSIONS $EXPECTED_VER)"
else
    fail "version-match: '$NEW_VER' does not contain '$EXPECTED_BARE' — stale binary?"
fi

# 5d. Version changed from baseline (guards against no-op upgrade)
if [ "$NEW_VER" != "$BASELINE_VER" ]; then
    pass "version-changed: bumped from '$BASELINE_VER' to '$NEW_VER'"
else
    fail "version-changed: version unchanged after upgrade ('$NEW_VER') — stale binary not rebuilt"
fi

# 5e. boi --help lists required subcommands including `bench` (the bug-to-catch)
HELP_OUTPUT=$("$BOI" --help 2>&1)
for sub in dispatch status bench cancel; do
    if echo "$HELP_OUTPUT" | grep -q "$sub"; then
        pass "subcmd-present: '$sub' in --help after upgrade"
    else
        fail "subcmd-present: '$sub' NOT in --help after upgrade — stale binary?"
    fi
done

# 5f. Help line count grew or stayed same (detects shrinking = regressed binary)
NEW_HELP_LINES=$("$BOI" --help 2>&1 | wc -l | tr -d ' ')
if [ "$NEW_HELP_LINES" -ge "$BASELINE_HELP_LINES" ]; then
    pass "help-lines: help grew/stable ($BASELINE_HELP_LINES → $NEW_HELP_LINES lines)"
else
    fail "help-lines: help shrank ($BASELINE_HELP_LINES → $NEW_HELP_LINES lines) — possible regression"
fi

# 5g. Binary mtime is newer than pre-upgrade (proves binary was rebuilt, not reused)
BINARY_MTIME_AFTER=$(stat -c %Y "$BOI" 2>/dev/null || echo "0")
if [ "$BINARY_MTIME_AFTER" -gt "$BINARY_MTIME_BEFORE" ] 2>/dev/null; then
    pass "binary-rebuilt: mtime updated (before=$BINARY_MTIME_BEFORE after=$BINARY_MTIME_AFTER)"
else
    fail "binary-rebuilt: mtime NOT updated (before=$BINARY_MTIME_BEFORE after=$BINARY_MTIME_AFTER) — binary may be stale symlink"
fi

# ── 6a. Smoke dispatch after upgrade (only when ANTHROPIC_API_KEY is set) ─────
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "  (ANTHROPIC_API_KEY not set — skipping smoke dispatch)"
else
    echo "--- 6a. smoke dispatch after upgrade ---"
    "$BOI" daemon start > /tmp/daemon-start.log 2>&1 || true

    daemon_ready=0
    for _i in $(seq 1 20); do
        if "$BOI" status > /dev/null 2>&1; then
            daemon_ready=1; break
        fi
        sleep 0.5
    done

    if [ "$daemon_ready" -eq 1 ]; then
        pass "smoke-daemon: BOI daemon ready after upgrade"
    else
        fail "smoke-daemon: daemon not ready within 10s after upgrade"
        cat /tmp/daemon-start.log || true
    fi

    if [ "$daemon_ready" -eq 1 ]; then
        SMOKE_MARKER="/tmp/boi-upgrade-smoke-$$"
        SMOKE_SPEC="/tmp/boi-upgrade-spec-$$.yaml"
        cat > "$SMOKE_SPEC" << SMOKESPEC
title: "BOI upgrade smoke test"
mode: execute
tasks:
  - id: U0001
    title: "create upgrade marker"
    spec: |
      Create the file ${SMOKE_MARKER} with content: boi-upgrade-smoke-ok
    verify: "test -f ${SMOKE_MARKER}"
SMOKESPEC
        pass "smoke-spec: written"

        SPEC_ID=$("$BOI" dispatch "$SMOKE_SPEC" 2>&1)
        DISPATCH_EXIT=$?
        if [ "$DISPATCH_EXIT" -eq 0 ] && [ -n "$SPEC_ID" ]; then
            pass "smoke-dispatch: spec enqueued (id: $SPEC_ID)"
        else
            fail "smoke-dispatch: boi dispatch failed (exit $DISPATCH_EXIT)"
        fi

        if [ "$DISPATCH_EXIT" -eq 0 ] && [ -n "$SPEC_ID" ]; then
            POLL_START=$(date +%s)
            TERMINAL=""
            while true; do
                ELAPSED=$(( $(date +%s) - POLL_START ))
                if [ "$ELAPSED" -ge 120 ]; then
                    fail "smoke-poll: timed out after 120s"
                    "$BOI" status "$SPEC_ID" 2>&1 | tail -20 || true
                    break
                fi
                STATUS_JSON=$("$BOI" status "$SPEC_ID" --json 2>/dev/null || echo '{}')
                SPEC_STATUS=$(python3 -c \
                    "import sys,json; print(json.load(sys.stdin).get('status',''))" \
                    <<< "$STATUS_JSON" 2>/dev/null || echo "")
                case "$SPEC_STATUS" in
                    completed)        TERMINAL="completed"; break ;;
                    failed|cancelled) TERMINAL="$SPEC_STATUS"; break ;;
                esac
                sleep 3
            done

            if [ "$TERMINAL" = "completed" ]; then
                pass "smoke-complete: spec reached 'completed'"
            else
                fail "smoke-complete: spec reached '$TERMINAL' (expected 'completed')"
            fi

            if [ -f "$SMOKE_MARKER" ]; then
                pass "smoke-output: marker file exists"
            else
                fail "smoke-output: marker file not found at $SMOKE_MARKER"
            fi
        fi
    fi
fi

# ── 6b. BAD case: corrupt symlink → doctor must catch it ─────────────────────
echo "--- 6b. bad case: corrupt symlink + doctor check ---"

# Save resolved target so we can restore it
GOOD_TARGET=$(readlink -f "$BOI" 2>/dev/null || echo "")

# Corrupt the symlink to a nonexistent path
ln -sf /tmp/nonexistent-boi-binary-corrupt "$BOI"

if [ -L "$BOI" ] && [ ! -f "$BOI" ]; then
    pass "corrupt-symlink: $BOI is now a dangling symlink"
else
    fail "corrupt-symlink: expected dangling symlink; got something else"
fi

# Run doctor.sh against the HEAD checkout; it must detect the dangling symlink.
# HEX_DIR is auto-detected from SCRIPT_DIR in doctor.sh, so running from /tmp/hex works.
DOCTOR_OUT=$(HEX_DIR=/tmp/hex bash /tmp/hex/system/scripts/doctor.sh 2>&1) || DOCTOR_RC=$?
DOCTOR_RC=${DOCTOR_RC:-0}

if echo "$DOCTOR_OUT" | grep -qi "dangling\|corrupt\|\[ERROR\].*boi\|boi.*error\|boi.*symlink\|symlink.*boi"; then
    pass "doctor-catch: doctor detected dangling symlink (exit $DOCTOR_RC)"
else
    fail "doctor-catch: doctor did NOT report dangling symlink — this is the bug to fix"
    echo "  BOI-related doctor output:"
    echo "$DOCTOR_OUT" | grep -i boi | head -10 || echo "  (no BOI lines in doctor output)"
    echo "  Last 15 lines:"
    echo "$DOCTOR_OUT" | tail -15
fi

# Also assert doctor exited non-zero (errors should fail the run)
if [ "$DOCTOR_RC" -ne 0 ]; then
    pass "doctor-exit: doctor exited $DOCTOR_RC (non-zero on error, as expected)"
else
    fail "doctor-exit: doctor exited 0 despite dangling symlink — errors not reflected in exit code"
fi

# Restore clean symlink so on_exit cleanup is tidy
if [ -n "$GOOD_TARGET" ] && [ -f "$GOOD_TARGET" ]; then
    ln -sf "$GOOD_TARGET" "$BOI"
fi
INNER_EOF

# ── Run the container ─────────────────────────────────────────────────────────
BOI_SRC="$HOME/github.com/mrap/boi"
CONTAINER_LOG="/tmp/boi-upgrade-e2e-$$.log"

DOCKER_ARGS=(
    "--rm"
    "-v" "${REPO_ROOT}:/repo:ro"
    "-e" "HOME=/home/testuser"
)
[ -n "${ANTHROPIC_API_KEY:-}" ] && DOCKER_ARGS+=("-e" "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}")
[ -d "${BOI_SRC}/.git" ]       && DOCKER_ARGS+=("-v" "${BOI_SRC}:/boi:ro")

echo "  Launching fresh container for upgrade test..."
docker run "${DOCKER_ARGS[@]}" "$IMAGE_TAG" bash -c "$INNER_SCRIPT" \
    > "$CONTAINER_LOG" 2>&1
CONTAINER_EXIT=$?

# Always display container output (shows PASS:/FAIL: lines)
cat "$CONTAINER_LOG"
echo ""

# ── Merge inner counters into outer helpers.sh PASS/FAIL ─────────────────────
INNER_PASS=$(grep -c "^  PASS:" "$CONTAINER_LOG" 2>/dev/null || true)
INNER_FAIL=$(grep -c "^  FAIL:" "$CONTAINER_LOG" 2>/dev/null || true)
INNER_PASS=${INNER_PASS:-0}
INNER_FAIL=${INNER_FAIL:-0}

PASS=$((PASS + INNER_PASS))
FAIL=$((FAIL + INNER_FAIL))

if [ "$CONTAINER_EXIT" -eq 0 ] && [ "$INNER_FAIL" -eq 0 ]; then
    assert_pass "boi-upgrade-e2e: containerized upgrade passed ($INNER_PASS assertions)"
else
    assert_fail "boi-upgrade-e2e: $INNER_FAIL of $((INNER_PASS + INNER_FAIL)) assertions failed (container exit: $CONTAINER_EXIT)"
    echo "  [hint] Force image rebuild: docker rmi $IMAGE_TAG"
    echo "  [hint] Smoke dispatch:      ANTHROPIC_API_KEY=<key> bash $0"
fi

rm -f "$CONTAINER_LOG"
