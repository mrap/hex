#!/usr/bin/env bash
# hex-ui build pipeline: typecheck → test → build → restart services → verify
# Usage: hex-ui-build.sh [--skip-tests]
set -uo pipefail

FRONTEND_DIR="$HOME/github.com/mrap/hex-ui/frontend"
BACKEND_DIR="$HOME/github.com/mrap/hex-ui/backend"
BACKEND_PORT=8889
ROUTER_PORT=8880
BACKEND_LOG="/tmp/hex-ui-backend.log"
SKIP_TESTS=false

for arg in "$@"; do
  case "$arg" in
    --skip-tests) SKIP_TESTS=true ;;
  esac
done

echo "=== hex-ui build pipeline ==="

# ── Typecheck ──────────────────────────────────────────────────────────────
echo ""
echo "▶ TypeScript check..."
cd "$FRONTEND_DIR"
if ! npx tsc --noEmit 2>&1; then
  echo "✗ TypeScript check failed" >&2
  exit 1
fi
echo "✓ TypeScript OK"

# ── Tests ──────────────────────────────────────────────────────────────────
TEST_PASSED="skipped"
if [ "$SKIP_TESTS" = "false" ]; then
  echo ""
  echo "▶ Running tests..."
  TEST_OUTPUT=$(npx vitest run 2>&1)
  TEST_EXIT=$?
  echo "$TEST_OUTPUT"
  if [ $TEST_EXIT -ne 0 ]; then
    echo "✗ Tests failed" >&2
    exit 1
  fi
  TEST_PASSED=$(echo "$TEST_OUTPUT" | grep -E "Tests\s+[0-9]+ passed" | grep -oE "[0-9]+ passed" | head -1 || echo "passed")
  echo "✓ Tests: $TEST_PASSED"
else
  echo ""
  echo "⚠ Tests skipped (--skip-tests)"
fi

# ── Build ──────────────────────────────────────────────────────────────────
echo ""
echo "▶ Building frontend..."
BUILD_OUTPUT=$(npx vite build 2>&1)
BUILD_EXIT=$?
echo "$BUILD_OUTPUT"
if [ $BUILD_EXIT -ne 0 ]; then
  echo "✗ Vite build failed" >&2
  exit 1
fi
BUNDLE_SIZE=$(du -sh "$FRONTEND_DIR/dist" 2>/dev/null | cut -f1 || echo "unknown")
echo "✓ Build OK (dist: $BUNDLE_SIZE)"

# ── Restart backend ────────────────────────────────────────────────────────
echo ""
echo "▶ Restarting backend..."
OLD_PIDS=$(lsof -ti :"$BACKEND_PORT" 2>/dev/null || true)
if [ -n "$OLD_PIDS" ]; then
  echo "$OLD_PIDS" | xargs kill -TERM 2>/dev/null || true
  sleep 1
fi
cd "$BACKEND_DIR"
nohup uvicorn main:app --host 0.0.0.0 --port "$BACKEND_PORT" > "$BACKEND_LOG" 2>&1 &
sleep 3

# ── Verify backend health ──────────────────────────────────────────────────
echo "▶ Verifying backend health..."
BACKEND_OK=false
for i in 1 2 3; do
  if curl -sf "http://localhost:${BACKEND_PORT}/health" > /dev/null 2>&1; then
    BACKEND_OK=true
    break
  fi
  sleep 2
done

if [ "$BACKEND_OK" = "true" ]; then
  echo "✓ Backend healthy on :${BACKEND_PORT}"
else
  echo "✗ Backend not responding on :${BACKEND_PORT}" >&2
  echo "  Check: tail $BACKEND_LOG"
  exit 1
fi

# ── Verify/restart router ──────────────────────────────────────────────────
echo ""
echo "▶ Checking router on :${ROUTER_PORT}..."
ROUTER_OK=false
if curl -sf "http://localhost:${ROUTER_PORT}/health" > /dev/null 2>&1; then
  ROUTER_OK=true
else
  echo "  Router not responding — attempting restart..."
  OLD_ROUTER=$(lsof -ti :"$ROUTER_PORT" 2>/dev/null || true)
  if [ -n "$OLD_ROUTER" ]; then
    echo "$OLD_ROUTER" | xargs kill -TERM 2>/dev/null || true
    sleep 1
  fi
  ROUTER_DIR="${AGENT_DIR:-$HOME/hex}/.hex/scripts/hex-router"
  if [ -d "$ROUTER_DIR" ]; then
    cd "$ROUTER_DIR"
    nohup python3 router.py > /tmp/hex-router.log 2>&1 &
    sleep 3
    if curl -sf "http://localhost:${ROUTER_PORT}/health" > /dev/null 2>&1; then
      ROUTER_OK=true
    fi
  fi
fi

if [ "$ROUTER_OK" = "true" ]; then
  echo "✓ Router healthy on :${ROUTER_PORT}"
else
  echo "⚠ Router not responding on :${ROUTER_PORT} (non-fatal)"
fi

# ── Summary ────────────────────────────────────────────────────────────────
echo ""
echo "=== Summary ==="
echo "  Tests:    $TEST_PASSED"
echo "  Bundle:   $BUNDLE_SIZE"
echo "  Backend:  $([ "$BACKEND_OK" = "true" ] && echo "UP :${BACKEND_PORT}" || echo "DOWN")"
echo "  Router:   $([ "$ROUTER_OK" = "true" ] && echo "UP :${ROUTER_PORT}" || echo "DOWN (non-fatal)")"
echo ""
echo "✓ Build pipeline complete"
