#!/usr/bin/env bash
# test-boi-install.sh — Containerized fresh-install E2E for BOI.
#
# Spins up a Docker container (rust:latest base + dev tools) and:
#   1. Clones hex-foundation from a volume mount of the local repo
#   2. Runs install.sh non-interactively
#   3. Asserts: binary presence, --help subcommands, version, wrapper chain
#   4. Dispatches a smoke spec if ANTHROPIC_API_KEY is set (optional)
#   5. Clears all BOI state on exit so reruns are clean
#
# Failure dumps container logs + last 50 lines of any boi daemon log.
#
# Usage:
#   Standalone:    bash tests/core-e2e/suites/test-boi-install.sh
#   With dispatch: ANTHROPIC_API_KEY=<key> bash tests/core-e2e/suites/test-boi-install.sh
#   Via run-all.sh: sourced automatically (shares global PASS/FAIL counters)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

# Source helpers.sh when run standalone.
# When sourced by run-all.sh, assert_pass/assert_fail are already in scope.
if ! declare -f assert_pass >/dev/null 2>&1; then
    # shellcheck source=../helpers.sh
    source "$SCRIPT_DIR/../helpers.sh"
fi

echo ""
echo "=== BOI INSTALL E2E (containerized) ==="

# ── Docker availability ───────────────────────────────────────────────────────
if ! command -v docker >/dev/null 2>&1; then
    assert_fail "boi-install-prereq: docker not installed — cannot run containerized test"
    return 0 2>/dev/null || exit 1
fi
if ! docker info >/dev/null 2>&1; then
    assert_fail "boi-install-prereq: docker daemon not running"
    return 0 2>/dev/null || exit 1
fi
assert_pass "boi-install-prereq: docker available"

# ── Build (or reuse) Docker image ────────────────────────────────────────────
# rust:latest provides cargo; we layer on git, python3, sqlite3, etc.
# Fixed tag: rebuild only when image is absent. Force rebuild: docker rmi <tag>.
IMAGE_TAG="hex-boi-install-test:latest"
BUILD_LOG="/tmp/boi-install-build-$$.log"

if ! docker image inspect "$IMAGE_TAG" >/dev/null 2>&1; then
    yellow "  Building Docker image $IMAGE_TAG (first run — ~2-3 min)..."
    docker build -t "$IMAGE_TAG" - > "$BUILD_LOG" 2>&1 << 'DOCKERFILE_EOF'
FROM rust:latest
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl ca-certificates pkg-config libssl-dev python3 python3-pip sqlite3 \
    && rm -rf /var/lib/apt/lists/*
# Non-root user matches the expected $HOME layout
RUN useradd -m -s /bin/bash testuser
USER testuser
WORKDIR /home/testuser
DOCKERFILE_EOF
    BUILD_EXIT=$?
    rm -f "$BUILD_LOG"
    if [ "$BUILD_EXIT" -ne 0 ]; then
        assert_fail "boi-install-docker-build: image build failed (exit $BUILD_EXIT)"
        return 0 2>/dev/null || exit 1
    fi
fi
assert_pass "boi-install-docker-build: image ready ($IMAGE_TAG)"

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
    rm -rf "$HOME/.boi" /tmp/hex 2>/dev/null || true
    exit "${FAIL}"
}
trap on_exit EXIT

# boi binary lives here after install
export PATH="$HOME/.boi/bin:$PATH"

# ── 1. Clone hex-foundation ────────────────────────────────────────────────────
echo "--- 1. clone ---"
if git clone /repo /tmp/hex > /tmp/clone-out.log 2>&1; then
    pass "clone: hex-foundation cloned to /tmp/hex"
else
    fail "clone: git clone failed"
    cat /tmp/clone-out.log
    exit 1
fi

# ── 2. Run install.sh non-interactively ──────────────────────────────────────
echo "--- 2. install.sh ---"
cd /tmp/hex
export HEX_NONINTERACTIVE=1 CI=1
# Override BOI repo to use volume-mounted local source — avoids network during test
if [ -d /boi/.git ]; then
    export HEX_BOI_REPO="file:///boi"
fi

if bash install.sh > /tmp/install.log 2>&1; then
    pass "install: install.sh exited 0"
else
    INSTALL_EXIT=$?
    fail "install: install.sh exited $INSTALL_EXIT"
    echo "--- install.sh output (last 50 lines) ---"
    tail -50 /tmp/install.log
    exit 1
fi

BOI="$HOME/.boi/bin/boi"

# ── 3. Binary assertions ──────────────────────────────────────────────────────
echo "--- 3. assertions ---"

# 3a. Executable file (or symlink to one)
if [ -x "$BOI" ]; then
    pass "binary-exists: $BOI is executable"
else
    fail "binary-exists: $BOI is not executable (or missing)"
    exit 1
fi

# 3b. If symlink, target must be a real executable (not dangling)
if [ -L "$BOI" ]; then
    RESOLVED=$(readlink -f "$BOI" 2>/dev/null || true)
    if [ -x "$RESOLVED" ]; then
        pass "binary-real: symlink resolves to real executable ($RESOLVED)"
    else
        fail "binary-real: symlink resolves to missing/non-executable: $RESOLVED"
    fi
else
    pass "binary-real: $BOI is a regular (non-symlink) executable"
fi

# 3c. boi --help exits 0
HELP_OUTPUT=$("$BOI" --help 2>&1) || HELP_EXIT=$?
HELP_EXIT=${HELP_EXIT:-0}
if [ "$HELP_EXIT" -eq 0 ]; then
    pass "help-exit: boi --help exits 0"
else
    fail "help-exit: boi --help exited $HELP_EXIT"
fi

# 3d. --help lists the required subcommands
for sub in dispatch status bench cancel; do
    if echo "$HELP_OUTPUT" | grep -q "$sub"; then
        pass "help-subcmd: '$sub' listed in --help"
    else
        fail "help-subcmd: '$sub' NOT found in --help"
    fi
done

# 3e. `boi version` output matches VERSIONS BOI_VERSION
# VERSIONS format: BOI_VERSION=v1.0.0   boi version output: "boi 1.0.0"
EXPECTED_TAGGED=$(grep "^BOI_VERSION=" /tmp/hex/VERSIONS | cut -d= -f2)
EXPECTED_BARE="${EXPECTED_TAGGED#v}"
VER_OUTPUT=$("$BOI" version 2>&1) || true
if echo "$VER_OUTPUT" | grep -qF "$EXPECTED_BARE"; then
    pass "version-match: '$VER_OUTPUT' contains '$EXPECTED_BARE' (from VERSIONS $EXPECTED_TAGGED)"
else
    fail "version-match: '$VER_OUTPUT' does not contain '$EXPECTED_BARE'"
fi

# 3f. Wrapper chain: boi.sh delegates to ~/.boi/bin/boi
BOI_WRAPPER="$HOME/github.com/mrap/boi/boi.sh"
if [ -x "$BOI_WRAPPER" ]; then
    if "$BOI_WRAPPER" --help > /dev/null 2>&1; then
        pass "wrapper-chain: $BOI_WRAPPER --help exits 0"
    else
        fail "wrapper-chain: $BOI_WRAPPER --help failed"
    fi
else
    fail "wrapper-chain: boi.sh not found/executable at $BOI_WRAPPER"
fi

# ── 4–7. Smoke dispatch (only when ANTHROPIC_API_KEY is set) ─────────────────
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "  (ANTHROPIC_API_KEY not set — skipping smoke dispatch)"
    echo "  (re-run with ANTHROPIC_API_KEY=<key> to exercise dispatch/status)"
    exit "${FAIL}"
fi

echo "--- 4. daemon start ---"
"$BOI" daemon start > /tmp/daemon-start.log 2>&1 || true

daemon_ready=0
for _i in $(seq 1 20); do
    if "$BOI" status > /dev/null 2>&1; then
        daemon_ready=1; break
    fi
    sleep 0.5
done

if [ "$daemon_ready" -eq 1 ]; then
    pass "daemon-start: BOI daemon ready within 10s"
else
    fail "daemon-start: daemon did not become ready within 10s"
    cat /tmp/daemon-start.log || true
    exit 1
fi

echo "--- 5. smoke spec ---"
SMOKE_MARKER="/tmp/boi-smoke-marker-$$"
SMOKE_SPEC="/tmp/boi-smoke-spec-$$.yaml"
cat > "$SMOKE_SPEC" << SMOKESPEC
title: "BOI install smoke test"
mode: execute
tasks:
  - id: T0001
    title: "create marker file"
    spec: |
      Create the file ${SMOKE_MARKER} with content: boi-install-smoke-ok
    verify: "test -f ${SMOKE_MARKER}"
SMOKESPEC
pass "smoke-spec: written"

echo "--- 6. dispatch + poll ---"
SPEC_ID=$("$BOI" dispatch "$SMOKE_SPEC" 2>&1)
DISPATCH_EXIT=$?
if [ "$DISPATCH_EXIT" -eq 0 ] && [ -n "$SPEC_ID" ]; then
    pass "dispatch: spec enqueued (id: $SPEC_ID)"
else
    fail "dispatch: boi dispatch failed (exit $DISPATCH_EXIT, output: $SPEC_ID)"
    exit 1
fi

echo "  polling status (cap 120s)..."
POLL_START=$(date +%s)
TERMINAL=""
while true; do
    ELAPSED=$(( $(date +%s) - POLL_START ))
    if [ "$ELAPSED" -ge 120 ]; then
        fail "dispatch-poll: timed out after 120s waiting for completion"
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
    pass "dispatch-complete: spec reached 'completed'"
else
    fail "dispatch-complete: spec reached '$TERMINAL' (expected 'completed')"
fi

echo "--- 7. smoke output ---"
if [ -f "$SMOKE_MARKER" ]; then
    pass "smoke-output: marker file exists ($SMOKE_MARKER)"
else
    fail "smoke-output: marker file not found at $SMOKE_MARKER"
fi
INNER_EOF

# ── Run the container ─────────────────────────────────────────────────────────
BOI_SRC="$HOME/github.com/mrap/boi"
CONTAINER_LOG="/tmp/boi-install-e2e-$$.log"

DOCKER_ARGS=(
    "--rm"
    "-v" "${REPO_ROOT}:/repo:ro"
    "-e" "HOME=/home/testuser"
)
# Pass API key through if available (needed for smoke dispatch)
[ -n "${ANTHROPIC_API_KEY:-}" ] && DOCKER_ARGS+=("-e" "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}")
# Mount local BOI source to avoid network dependency during test
[ -d "${BOI_SRC}/.git" ]       && DOCKER_ARGS+=("-v" "${BOI_SRC}:/boi:ro")

echo "  Launching fresh container for install test..."
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
    assert_pass "boi-install-e2e: containerized install passed ($INNER_PASS assertions)"
else
    assert_fail "boi-install-e2e: $INNER_FAIL of $((INNER_PASS + INNER_FAIL)) assertions failed (container exit: $CONTAINER_EXIT)"
    echo "  [hint] Force image rebuild: docker rmi $IMAGE_TAG"
    echo "  [hint] Smoke dispatch:      ANTHROPIC_API_KEY=<key> bash $0"
fi

rm -f "$CONTAINER_LOG"
