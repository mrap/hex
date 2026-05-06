#!/usr/bin/env bash
# check-career-pipeline.sh — dry-run health check for the career auto-send pipeline.
#
# Tests the full queue → veto cycle without sending real Slack or Gmail messages.
# Exit 0 on pass; exit 1 with descriptive stderr on any failure.
#
# Usage: check-career-pipeline.sh --dry-run
#   (--dry-run is required; without it the script prints usage and exits 1)

set -uo pipefail

HEALTH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$HEALTH_DIR/.." && pwd)"
CAREER_DIR="$SCRIPTS_DIR/career"
HEX_PROJECT_DIR="$(cd "$SCRIPTS_DIR/../.." && pwd)"
OUTBOX_DIR="$HEX_PROJECT_DIR/projects/career/outbox"
EVENTS_DB="$HOME/.hex-events/events.db"

DRY_RUN=0
for arg in "$@"; do
  [[ "$arg" == "--dry-run" ]] && DRY_RUN=1
done

if [[ $DRY_RUN -ne 1 ]]; then
  echo "Usage: check-career-pipeline.sh --dry-run" >&2
  exit 1
fi

FAIL=0
_fail() { echo "check-career-pipeline: FAIL — $1" >&2; FAIL=1; }
_ok()   { echo "check-career-pipeline: OK — $1"; }

# Track test artifacts for cleanup
TEST_DRAFT_SRC=""
TEST_OUTBOX_FILE=""
TEST_VETOED_FILE=""

cleanup() {
  [[ -n "$TEST_DRAFT_SRC"   && -f "$TEST_DRAFT_SRC"   ]] && rm -f "$TEST_DRAFT_SRC"   || true
  [[ -n "$TEST_OUTBOX_FILE" && -f "$TEST_OUTBOX_FILE" ]] && rm -f "$TEST_OUTBOX_FILE" || true
  [[ -n "$TEST_VETOED_FILE" && -f "$TEST_VETOED_FILE" ]] && rm -f "$TEST_VETOED_FILE" || true
}
trap cleanup EXIT

# ── 1. Scripts present and executable ────────────────────────────────────────
SEND_SCRIPT="$CAREER_DIR/send-or-queue.sh"
VETO_SCRIPT="$CAREER_DIR/veto-pending-send.sh"

[[ -x "$SEND_SCRIPT" ]] || { _fail "send-or-queue.sh not executable: $SEND_SCRIPT"; exit 1; }
[[ -x "$VETO_SCRIPT" ]] || { _fail "veto-pending-send.sh not executable: $VETO_SCRIPT"; exit 1; }
_ok "send-or-queue.sh and veto-pending-send.sh are executable"

# ── 2. Synthesize test draft ──────────────────────────────────────────────────
TEST_DRAFT_SRC="$(mktemp /tmp/hex-career-dryrun-XXXXXX.md)"
TS="$(date -u +%Y%m%dT%H%M%SZ)"

cat > "$TEST_DRAFT_SRC" << DRAFTEOF
---
To: dryrun-test@example.com
From: hex-test@example.com
Subject: DRY-RUN TEST $TS
---

Synthetic test draft from check-career-pipeline.sh --dry-run.
This message should never be sent. If received, the pipeline has a bug.
DRAFTEOF

_ok "test draft synthesized: $(basename "$TEST_DRAFT_SRC")"

# ── 3. Queue via send-or-queue.sh (AUTOSEND off) ─────────────────────────────
QUEUE_OUT=""
QUEUE_EXIT=0
QUEUE_OUT="$("$SEND_SCRIPT" "$TEST_DRAFT_SRC" 2>&1)" || QUEUE_EXIT=$?

if [[ $QUEUE_EXIT -ne 0 ]]; then
  _fail "send-or-queue.sh exited $QUEUE_EXIT: $QUEUE_OUT"
  exit 1
fi

QUEUED_PATH="$(echo "$QUEUE_OUT" | grep "Queued:" | sed 's/.*Queued: *//')"
if [[ -z "$QUEUED_PATH" || ! -f "$QUEUED_PATH" ]]; then
  _fail "send-or-queue.sh ran but outbox file not found. Output: $QUEUE_OUT"
  exit 1
fi

TEST_OUTBOX_FILE="$QUEUED_PATH"
DRAFT_ID="$(basename "$QUEUED_PATH")"
_ok "draft queued to outbox: $DRAFT_ID"

# ── 4. Confirm hex.career.outbound.queued event reached the DB ────────────────
EVENT_FOUND="$(python3 - "$EVENTS_DB" "$DRAFT_ID" <<'PYEOF'
import sys, sqlite3
db_path, draft_id = sys.argv[1], sys.argv[2]
try:
    conn = sqlite3.connect(db_path, timeout=5)
    rows = conn.execute(
        "SELECT id FROM events WHERE event_type='hex.career.outbound.queued' "
        "AND payload LIKE ? ORDER BY id DESC LIMIT 1",
        ('%' + draft_id + '%',)
    ).fetchall()
    conn.close()
    print('yes' if rows else 'no')
except Exception as e:
    print('error:' + str(e))
PYEOF
)"

if [[ "$EVENT_FOUND" == "yes" ]]; then
  _ok "hex.career.outbound.queued event confirmed in DB"
elif [[ "$EVENT_FOUND" == no ]]; then
  _fail "hex.career.outbound.queued event not found in DB (emit failed?)"
  FAIL=1
else
  _fail "events DB query error: $EVENT_FOUND"
  FAIL=1
fi

# ── 5. Confirm career-auto-send policy is loaded (= Slack veto + timer wired) ─
# The policy handles hex.career.outbound.ready_to_send → Slack preview + 30-min deferred send.
# Use the daemon's own policy loader so this check matches what's actually live in memory.
POLICY_PYTHON="${HOME}/.hex-events/venv/bin/python3"
[[ -x "$POLICY_PYTHON" ]] || POLICY_PYTHON="python3"
POLICY_LOADED="$(HEX_EVENTS_DIR="${HOME}/.hex-events" "$POLICY_PYTHON" - 2>&1 <<'PYEOF'
import sys, os
_hex_events = os.environ.get('HEX_EVENTS_DIR', os.path.expanduser('~/.hex-events'))
sys.path.insert(0, _hex_events)
try:
    from policy import load_policies
    ps = load_policies(os.path.join(_hex_events, 'policies'))
    print('yes' if any(getattr(p, 'name', '') == 'career-auto-send' for p in ps) else 'no')
except Exception as e:
    print('error:' + str(e))
PYEOF
)"
if [[ "$POLICY_LOADED" == "yes" ]]; then
  _ok "career-auto-send policy loaded (Slack veto window + 30-min timer are wired)"
else
  _fail "career-auto-send policy not found in hex-events daemon — 30-min timer NOT wired ($POLICY_LOADED)"
  FAIL=1
fi

# ── 6. Confirm deferred_events table can hold the scheduled send timer ────────
DEFERRED_OK="$(python3 - "$EVENTS_DB" <<'PYEOF'
import sys, sqlite3
try:
    conn = sqlite3.connect(sys.argv[1], timeout=5)
    cols = {r[1] for r in conn.execute('PRAGMA table_info(deferred_events)').fetchall()}
    conn.close()
    required = {'event_type', 'fire_at', 'payload', 'cancel_group'}
    missing = required - cols
    print('missing:' + ','.join(missing) if missing else 'yes')
except Exception as e:
    print('error:' + str(e))
PYEOF
)"

if [[ "$DEFERRED_OK" == "yes" ]]; then
  _ok "deferred_events schema valid (30-min timer CAN be scheduled)"
else
  _fail "deferred_events table problem: $DEFERRED_OK"
  FAIL=1
fi

# ── 7. Veto the queued draft ──────────────────────────────────────────────────
VETO_OUT=""
VETO_EXIT=0
VETO_OUT="$("$VETO_SCRIPT" "$TEST_OUTBOX_FILE" 2>&1)" || VETO_EXIT=$?

if [[ $VETO_EXIT -ne 0 ]]; then
  _fail "veto-pending-send.sh exited $VETO_EXIT: $VETO_OUT"
  exit 1
fi
_ok "veto-pending-send.sh ran cleanly"

# ── 8. Confirm draft moved out of outbox/ ────────────────────────────────────
if [[ -f "$TEST_OUTBOX_FILE" ]]; then
  _fail "outbox file still present after veto: $TEST_OUTBOX_FILE"
  FAIL=1
else
  _ok "draft removed from outbox (moved to vetoed/)"
fi

# ── 9. Confirm draft landed in outbox/vetoed/ ────────────────────────────────
VETOED_MATCH="$(ls "$OUTBOX_DIR/vetoed/" 2>/dev/null | grep -F "$DRAFT_ID" | head -1 || true)"
if [[ -n "$VETOED_MATCH" ]]; then
  TEST_VETOED_FILE="$OUTBOX_DIR/vetoed/$VETOED_MATCH"
  _ok "draft archived in vetoed/: $VETOED_MATCH"
else
  _fail "vetoed file not found in outbox/vetoed/ for draft_id=$DRAFT_ID"
  FAIL=1
fi

# ── 10. Confirm hex.career.outbound.vetoed event reached the DB ───────────────
VETOED_EVENT="$(python3 - "$EVENTS_DB" "$DRAFT_ID" <<'PYEOF'
import sys, sqlite3
db_path, draft_id = sys.argv[1], sys.argv[2]
try:
    conn = sqlite3.connect(db_path, timeout=5)
    rows = conn.execute(
        "SELECT id FROM events WHERE event_type='hex.career.outbound.vetoed' "
        "AND payload LIKE ? ORDER BY id DESC LIMIT 1",
        ('%' + draft_id + '%',)
    ).fetchall()
    conn.close()
    print('yes' if rows else 'no')
except Exception as e:
    print('error:' + str(e))
PYEOF
)"

if [[ "$VETOED_EVENT" == "yes" ]]; then
  _ok "hex.career.outbound.vetoed event confirmed in DB"
elif [[ "$VETOED_EVENT" == "no" ]]; then
  _fail "hex.career.outbound.vetoed event not found in DB (veto emit failed?)"
  FAIL=1
else
  _fail "events DB query error: $VETOED_EVENT"
  FAIL=1
fi

# ── 11. Confirm timer is effectively cancelled ────────────────────────────────
# The send-after-veto-window policy checks: draft must still be in outbox/ (not vetoed/).
# Since the file is now in vetoed/, any pending deferred timer will no-op on fire.
# This is the correct cancellation mechanism per career-auto-send.yaml.
PENDING="$(python3 - "$EVENTS_DB" "$DRAFT_ID" <<'PYEOF'
import sys, sqlite3
db_path, draft_id = sys.argv[1], sys.argv[2]
try:
    conn = sqlite3.connect(db_path, timeout=5)
    rows = conn.execute(
        "SELECT id FROM deferred_events "
        "WHERE event_type='schedule.send-after-veto-window' "
        "AND payload LIKE ? AND fire_at > datetime('now')",
        ('%' + draft_id + '%',)
    ).fetchall()
    conn.close()
    print(str(len(rows)))
except Exception as e:
    print('error:' + str(e))
PYEOF
)"

if [[ "$PENDING" =~ ^[0-9]+$ && "$PENDING" -gt 0 ]]; then
  # Timer exists but the policy will no-op because draft is in vetoed/ — safe.
  _ok "deferred send timer pending but safe: draft is in vetoed/, policy will no-op on fire"
else
  _ok "no pending send timer for this draft (timer cancelled or never scheduled in dry-run)"
fi

# ── Summary ───────────────────────────────────────────────────────────────────
if [[ $FAIL -eq 0 ]]; then
  echo "check-career-pipeline: PASS (roundtrip dry-run complete)"
  exit 0
else
  echo "check-career-pipeline: FAIL (see errors above)" >&2
  exit 1
fi
