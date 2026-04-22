#!/usr/bin/env bash
# hex-integration-check.sh — Run one integration sub-check, update state, emit events.
#
# Usage: hex-integration-check.sh <integration-name>
# Exit codes: 0=ok, 1=check failed, 2=missing sub-check script

set -uo pipefail

# ─── Paths ────────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HEX_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

NAME="${1:-}"
if [[ -z "$NAME" ]]; then
  echo "Usage: hex-integration-check.sh <name>" >&2
  exit 2
fi

SUBCHECK="$SCRIPT_DIR/integrations/${NAME}.sh"
if [[ ! -f "$SUBCHECK" ]]; then
  echo "[ERROR] No sub-check found: $SUBCHECK" >&2
  exit 2
fi

STATE_DIR="$HEX_ROOT/projects/integrations/_state"
STATE_FILE="$STATE_DIR/${NAME}.json"
LOCK_DIR="$STATE_DIR/.${NAME}.lock.d"
RUNBOOK="projects/integrations/runbooks/${NAME}.md"

HEX_EMIT="python3 $HOME/.hex-events/hex_emit.py"

# ─── Ensure state dir exists ──────────────────────────────────────────────────
mkdir -p "$STATE_DIR"

# ─── mkdir-based atomic lock with 30s timeout (works on macOS without flock) ──
LOCK_ACQUIRED=false
for _i in $(seq 1 30); do
  if mkdir "$LOCK_DIR" 2>/dev/null; then
    LOCK_ACQUIRED=true
    break
  fi
  sleep 1
done
if ! $LOCK_ACQUIRED; then
  echo "[ERROR] Could not acquire lock for $NAME after 30s" >&2
  exit 1
fi
trap 'rmdir "$LOCK_DIR" 2>/dev/null || true' EXIT

# ─── Execute sub-check with timeout, measure latency ─────────────────────────
CHECKED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
START_MS="$(python3 -c 'import time; print(int(time.time()*1000))')"

ERROR_FILE="$(mktemp /tmp/hex-integration-err.XXXXXX)"
trap 'rm -f "$ERROR_FILE"; rmdir "$LOCK_DIR" 2>/dev/null || true' EXIT

set +e
python3 - <<PYEOF
import subprocess, sys
try:
    with open("$ERROR_FILE", "w") as ef:
        r = subprocess.run(["bash", "$SUBCHECK"], timeout=30, stderr=ef)
    sys.exit(r.returncode)
except subprocess.TimeoutExpired:
    with open("$ERROR_FILE", "w") as ef:
        ef.write("check timed out after 30s")
    sys.exit(124)
PYEOF
CHECK_EXIT=$?
set -e

END_MS="$(python3 -c 'import time; print(int(time.time()*1000))')"
LATENCY_MS=$(( END_MS - START_MS ))

ERROR_MSG=""
if [[ $CHECK_EXIT -ne 0 ]]; then
  ERROR_MSG="$(cat "$ERROR_FILE" 2>/dev/null | tr '\n' ' ' | sed 's/[[:space:]]*$//')"
  if [[ -z "$ERROR_MSG" ]]; then
    ERROR_MSG="sub-check exited $CHECK_EXIT"
  fi
fi

# ─── Read prior state ─────────────────────────────────────────────────────────
PRIOR_STATUS=""
PRIOR_CONSECUTIVE_FAILS=0
PRIOR_STREAK=0
PRIOR_LAST_OK="null"
PRIOR_LAST_FAIL="null"

if [[ -f "$STATE_FILE" ]]; then
  PRIOR_STATUS="$(jq -r '.status // ""' "$STATE_FILE" 2>/dev/null || true)"
  PRIOR_CONSECUTIVE_FAILS="$(jq -r '.consecutive_fails // 0' "$STATE_FILE" 2>/dev/null || echo 0)"
  PRIOR_STREAK="$(jq -r '.streak // 0' "$STATE_FILE" 2>/dev/null || echo 0)"
  raw_ok="$(jq -r '.last_ok // empty' "$STATE_FILE" 2>/dev/null || true)"
  raw_fail="$(jq -r '.last_fail // empty' "$STATE_FILE" 2>/dev/null || true)"
  PRIOR_LAST_OK="${raw_ok:-null}"
  PRIOR_LAST_FAIL="${raw_fail:-null}"
fi

# ─── Compute new state ────────────────────────────────────────────────────────
if [[ $CHECK_EXIT -eq 0 ]]; then
  NEW_STATUS="ok"
  NEW_CONSECUTIVE_FAILS=0
  NEW_LAST_OK="\"$CHECKED_AT\""
  if [[ "$PRIOR_LAST_FAIL" == "null" ]]; then
    NEW_LAST_FAIL="null"
  else
    NEW_LAST_FAIL="\"$PRIOR_LAST_FAIL\""
  fi
else
  NEW_STATUS="fail"
  NEW_CONSECUTIVE_FAILS=$(( PRIOR_CONSECUTIVE_FAILS + 1 ))
  NEW_LAST_FAIL="\"$CHECKED_AT\""
  if [[ "$PRIOR_LAST_OK" == "null" ]]; then
    NEW_LAST_OK="null"
  else
    NEW_LAST_OK="\"$PRIOR_LAST_OK\""
  fi
fi

# Streak = consecutive checks with same status
if [[ -n "$PRIOR_STATUS" && "$NEW_STATUS" == "$PRIOR_STATUS" ]]; then
  NEW_STREAK=$(( PRIOR_STREAK + 1 ))
else
  NEW_STREAK=1
fi

# Transition detection
TRANSITION=false
if [[ -n "$PRIOR_STATUS" && "$NEW_STATUS" != "$PRIOR_STATUS" ]]; then
  TRANSITION=true
fi

# ─── Escape error for JSON ────────────────────────────────────────────────────
if [[ -n "$ERROR_MSG" ]]; then
  ERROR_JSON="$(printf '%s' "$ERROR_MSG" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')"
else
  ERROR_JSON="null"
fi

# ─── Write state atomically ───────────────────────────────────────────────────
TMP_STATE="$(mktemp "$STATE_DIR/.${NAME}.json.XXXXXX")"
cat >"$TMP_STATE" <<EOF
{
  "name": "$NAME",
  "status": "$NEW_STATUS",
  "last_ok": $NEW_LAST_OK,
  "last_fail": $NEW_LAST_FAIL,
  "last_checked": "$CHECKED_AT",
  "consecutive_fails": $NEW_CONSECUTIVE_FAILS,
  "streak": $NEW_STREAK,
  "latency_ms": $LATENCY_MS,
  "error": $ERROR_JSON,
  "runbook": "$RUNBOOK"
}
EOF
mv "$TMP_STATE" "$STATE_FILE"

# ─── Build payloads via Python to ensure valid JSON ──────────────────────────
# Pass error as env var to avoid bash-null interpolation issues in heredoc
_error_raw="$ERROR_MSG" \
OK_PAYLOAD="$(python3 -c "
import json, os
print(json.dumps({'name': '$NAME', 'latency_ms': $LATENCY_MS, 'checked_at': '$CHECKED_AT'}))
")"

_error_raw="$ERROR_MSG" \
FAIL_PAYLOAD="$(python3 -c "
import json, os
err = os.environ.get('_error_raw') or None
print(json.dumps({'name': '$NAME', 'error': err, 'latency_ms': $LATENCY_MS, 'checked_at': '$CHECKED_AT'}))
")"

_error_raw="$ERROR_MSG" \
TRANS_PAYLOAD="$(python3 -c "
import json, os
err = os.environ.get('_error_raw') or None
print(json.dumps({'name': '$NAME', 'from': '$PRIOR_STATUS', 'to': '$NEW_STATUS', 'streak': $NEW_STREAK, 'error': err, 'checked_at': '$CHECKED_AT'}))
")"

# ─── Emit events ──────────────────────────────────────────────────────────────
if [[ "$NEW_STATUS" == "ok" ]]; then
  $HEX_EMIT "hex.integration.check.ok" "$OK_PAYLOAD" "hex:integration-check" 2>/dev/null || true
else
  $HEX_EMIT "hex.integration.check.fail" "$FAIL_PAYLOAD" "hex:integration-check" 2>/dev/null || true
fi

if $TRANSITION; then
  $HEX_EMIT "hex.integration.check.transition" "$TRANS_PAYLOAD" "hex:integration-check" 2>/dev/null || true
fi

# ─── Summary line ─────────────────────────────────────────────────────────────
if [[ "$NEW_STATUS" == "ok" ]]; then
  echo "[ok] $NAME ${LATENCY_MS}ms streak=$NEW_STREAK"
  exit 0
else
  echo "[FAIL] $NAME ${LATENCY_MS}ms streak=$NEW_STREAK error: $ERROR_MSG"
  exit 1
fi
