#!/bin/bash
# doctor-checks/boi.sh — BOI worker fleet health checks
#
# Sourced by doctor.sh (or run via run-boi-checks.sh). Requires:
#   FIX    — true/false whether auto-fix is enabled
#   SMOKE  — true/false whether smoke tests are enabled
#   _pass / _warn / _error / _fixed / _rec / _info — output helpers
#
# Integration: source this file, then call run_boi_checks()
#
# Daemon: managed by macOS LaunchAgent (com.hex.boi-daemon), Rust binary at ~/.boi/bin/boi.
# Legacy: ~/.boi/daemon.pid and ~/.boi/src/daemon.py (Python daemon, deprecated).
# Config: ~/.boi/config.json
# Workers: ~/.boi/worktrees/boi-worker-N

set -uo pipefail

BOI_DIR="${BOI_DIR:-$HOME/.boi}"
BOI_PID_FILE="$BOI_DIR/daemon.pid"
BOI_STATE_FILE="$BOI_DIR/daemon-state.json"
_BOI_DB_PATH_SH="$(dirname "${BASH_SOURCE[0]}")/../lib/boi_db_path.sh"
BOI_DB="$(bash "$_BOI_DB_PATH_SH")"
BOI_CONFIG="$BOI_DIR/config.json"
BOI_DAEMON_SRC="$BOI_DIR/src/daemon.py"
BOI_CMD="$BOI_DIR/boi"
BOI_LAUNCH_AGENT_LABEL="com.hex.boi-daemon"
BOI_LAUNCH_AGENT_PLIST="$HOME/Library/LaunchAgents/${BOI_LAUNCH_AGENT_LABEL}.plist"

# Find the BOI daemon PID via (1) LaunchAgent, (2) PID file, (3) pgrep fallback.
# Prints PID to stdout if found, exits non-zero otherwise.
_boi_find_daemon_pid() {
  if [ "$(uname)" = "Darwin" ]; then
    local agent_pid
    agent_pid=$(launchctl list 2>/dev/null | awk -v label="$BOI_LAUNCH_AGENT_LABEL" '$3 == label {print $1}')
    if [ -n "$agent_pid" ] && [ "$agent_pid" != "-" ] && kill -0 "$agent_pid" 2>/dev/null; then
      echo "$agent_pid"
      return 0
    fi
  fi

  if [ -f "$BOI_PID_FILE" ]; then
    local file_pid
    file_pid=$(cat "$BOI_PID_FILE" 2>/dev/null | tr -d '[:space:]') || file_pid=""
    if [ -n "$file_pid" ] && kill -0 "$file_pid" 2>/dev/null; then
      echo "$file_pid"
      return 0
    fi
  fi

  # Match the canonical binary path so editor windows, log tails, and grep
  # processes that happen to mention "boi daemon" don't false-positive.
  local boi_bin="$BOI_DIR/bin/boi"
  local pgrep_pid
  pgrep_pid=$(pgrep -f "^${boi_bin} daemon" 2>/dev/null | head -1)
  if [ -n "$pgrep_pid" ]; then
    echo "$pgrep_pid"
    return 0
  fi

  return 1
}

# Start the BOI daemon. Prefer the LaunchAgent (kickstart). Fall back to legacy Python daemon.
_boi_start_daemon() {
  if [ "$(uname)" = "Darwin" ] && [ -f "$BOI_LAUNCH_AGENT_PLIST" ]; then
    launchctl kickstart -k "gui/$UID/$BOI_LAUNCH_AGENT_LABEL" >/dev/null 2>&1 || \
      launchctl load "$BOI_LAUNCH_AGENT_PLIST" >/dev/null 2>&1
    sleep 3
    return 0
  fi
  if [ -f "$BOI_DAEMON_SRC" ]; then
    cd "$BOI_DIR" && python3 src/daemon.py >/dev/null 2>&1 &
    sleep 5
    return 0
  fi
  return 1
}

# check_41: boi.daemon-running — daemon is running (LaunchAgent or legacy)
check_41() {
  local pid stat_char source
  if pid=$(_boi_find_daemon_pid); then
    # Mirror _boi_find_daemon_pid's precedence: launchctl is authoritative when
    # the LaunchAgent owns the process; reporting "pidfile" because both happen
    # to point at the same PID was misleading (code-review 2026-05-05).
    if [ "$(uname)" = "Darwin" ] \
       && launchctl list 2>/dev/null | awk -v label="$BOI_LAUNCH_AGENT_LABEL" '$3 == label {found=1} END {exit !found}'; then
      source="launchctl"
    elif [ -f "$BOI_PID_FILE" ] && [ "$pid" = "$(cat "$BOI_PID_FILE" 2>/dev/null | tr -d '[:space:]')" ]; then
      source="pidfile"
    else
      source="pgrep"
    fi

    if [ "$(uname)" = "Darwin" ]; then
      stat_char=$(ps -p "$pid" -o stat= 2>/dev/null | tr -d ' ' | cut -c1) || stat_char="?"
    else
      stat_char=$(cat "/proc/$pid/stat" 2>/dev/null | awk '{print $3}') || stat_char="?"
    fi

    if [ "$stat_char" = "T" ]; then
      _error "boi.daemon-running: PID $pid is in stopped state (T)"
      _rec 41 "boi.daemon-running" "error" "PID $pid is stopped (state=T, source=$source)"
      return
    fi

    _pass "boi.daemon-running: PID $pid alive (state=${stat_char}, via ${source})"
    _rec 41 "boi.daemon-running" "pass" "PID $pid running, state=${stat_char}, source=${source}"
    return
  fi

  if $FIX; then
    if _boi_start_daemon; then
      if pid=$(_boi_find_daemon_pid); then
        _fixed "boi.daemon-running: daemon started (PID $pid)"
        _rec 41 "boi.daemon-running" "fixed" "daemon started, new PID $pid"
        return
      fi
    fi
  fi

  _error "boi.daemon-running: no boi daemon process found (checked launchctl, $BOI_PID_FILE, pgrep)"
  _rec 41 "boi.daemon-running" "error" "no daemon process found"
}

# check_42: boi.workers-healthy — all configured workers have valid worktrees
check_42() {
  if [ ! -f "$BOI_CONFIG" ]; then
    _warn "boi.workers-healthy: config not found at $BOI_CONFIG"
    _rec 42 "boi.workers-healthy" "warn" "config.json not found"
    return
  fi

  local result
  result=$(python3 - "$BOI_CONFIG" "$BOI_DIR" 2>/dev/null <<'PYEOF'
import json, os, sys

config_path = sys.argv[1]
boi_dir = sys.argv[2]

try:
    with open(config_path) as f:
        config = json.load(f)
except Exception as e:
    print(f"ERROR:cannot parse config: {e}")
    sys.exit(0)

workers = config.get("workers", [])
if not workers:
    print("WARN:no workers defined in config")
    sys.exit(0)

missing = []
for w in workers:
    wid = w.get("id", "?")
    wpath = w.get("worktree_path", "")
    if not wpath:
        missing.append(f"{wid}: no worktree_path in config")
        continue
    git_dir = os.path.join(wpath, ".git")
    if not os.path.exists(git_dir):
        missing.append(f"{wid}: worktree missing at {wpath}")

if missing:
    print(f"MISSING:{len(missing)}:{len(workers)}")
    for m in missing:
        print(f"  {m}")
else:
    print(f"OK:{len(workers)}")
PYEOF
  )

  if [[ "$result" == ERROR:* ]]; then
    _warn "boi.workers-healthy: ${result#ERROR:}"
    _rec 42 "boi.workers-healthy" "warn" "${result#ERROR:}"
  elif [[ "$result" == WARN:* ]]; then
    _warn "boi.workers-healthy: ${result#WARN:}"
    _rec 42 "boi.workers-healthy" "warn" "${result#WARN:}"
  elif [[ "$result" == OK:* ]]; then
    local total="${result#OK:}"
    _pass "boi.workers-healthy: all ${total} worker worktrees valid"
    _rec 42 "boi.workers-healthy" "pass" "all ${total} workers have valid worktrees"
  elif [[ "$result" == MISSING:* ]]; then
    local counts="${result%%$'\n'*}"
    local miss_count="${counts#MISSING:}"
    miss_count="${miss_count%%:*}"
    local total_count="${counts##*:}"
    local details="${result#*$'\n'}"
    _warn "boi.workers-healthy: ${miss_count}/${total_count} worker worktrees missing:"
    while IFS= read -r line; do
      [ -n "$line" ] && _info "$line"
    done <<< "$details"
    _rec 42 "boi.workers-healthy" "warn" "${miss_count}/${total_count} worker worktrees missing"
  else
    _warn "boi.workers-healthy: unexpected output from check"
    _rec 42 "boi.workers-healthy" "warn" "unexpected check output"
  fi
}

# check_43: boi.queue-accessible — can read the queue database
check_43() {
  if [ ! -f "$BOI_DB" ]; then
    _warn "boi.queue-accessible: boi.db not found at $BOI_DB"
    _rec 43 "boi.queue-accessible" "warn" "boi.db not found"
    return
  fi

  local result
  result=$(python3 - "$BOI_DB" 2>/dev/null <<'PYEOF'
import sqlite3, sys

db_path = sys.argv[1]
try:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    # Read a small sample from specs
    total = conn.execute("SELECT COUNT(*) FROM specs").fetchone()[0]
    running = conn.execute("SELECT COUNT(*) FROM specs WHERE status='running'").fetchone()[0]
    queued = conn.execute("SELECT COUNT(*) FROM specs WHERE status='queued'").fetchone()[0]
    conn.close()
    print(f"OK:{total}:{running}:{queued}")
except Exception as e:
    print(f"ERROR:{e}")
PYEOF
  )

  if [[ "$result" == OK:* ]]; then
    local rest="${result#OK:}"
    local total="${rest%%:*}"
    local rest2="${rest#*:}"
    local running="${rest2%%:*}"
    local queued="${rest2#*:}"
    _pass "boi.queue-accessible: boi.db readable (${total} specs total, ${running} running, ${queued} queued)"
    _rec 43 "boi.queue-accessible" "pass" "boi.db readable: ${total} specs, ${running} running, ${queued} queued"
  elif [[ "$result" == ERROR:* ]]; then
    _error "boi.queue-accessible: cannot read boi.db — ${result#ERROR:}"
    _rec 43 "boi.queue-accessible" "error" "boi.db read failed: ${result#ERROR:}"
  else
    _warn "boi.queue-accessible: unexpected output from check"
    _rec 43 "boi.queue-accessible" "warn" "unexpected check output"
  fi
}

# check_44: boi.smoke-test — dispatch a trivial spec, verify it completes within 2 min
# Only runs when SMOKE=true
check_44() {
  if ! ${SMOKE:-false}; then
    _info "boi.smoke-test: skipped (use --smoke to run)"
    return
  fi

  if [ ! -x "$BOI_CMD" ] && ! command -v boi >/dev/null 2>&1; then
    _warn "boi.smoke-test: boi command not found"
    _rec 44 "boi.smoke-test" "warn" "boi command not found or not executable"
    return
  fi

  local boi_bin
  if [ -x "$BOI_CMD" ]; then
    boi_bin="$BOI_CMD"
  else
    boi_bin="boi"
  fi

  # Write a trivial test spec
  local spec_file
  spec_file=$(mktemp /tmp/boi-smoke-test-XXXXXX.spec.md)
  cat > "$spec_file" <<'SPECEOF'
# BOI Doctor Smoke Test

**Mode:** execute

## Tasks

### t-1: Smoke test task
PENDING

**Spec:** Print "boi-smoke-ok" to stdout and exit.

**Verify:** The word "boi-smoke-ok" appears in the task output.
SPECEOF

  # Dispatch
  local dispatch_output queue_id
  dispatch_output=$("$boi_bin" dispatch --spec "$spec_file" 2>&1) || {
    rm -f "$spec_file"
    _warn "boi.smoke-test: dispatch failed: $dispatch_output"
    _rec 44 "boi.smoke-test" "warn" "boi dispatch failed"
    return
  }
  rm -f "$spec_file"

  # Extract queue ID from dispatch output (e.g. "Dispatched q-123")
  queue_id=$(echo "$dispatch_output" | grep -oE 'q-[0-9]+' | head -1) || queue_id=""
  if [ -z "$queue_id" ]; then
    _warn "boi.smoke-test: could not parse queue ID from dispatch output"
    _rec 44 "boi.smoke-test" "warn" "could not parse queue ID from: $dispatch_output"
    return
  fi

  _info "boi.smoke-test: dispatched $queue_id — polling for completion (max 2 min)..."

  # Poll for completion (up to 120 seconds)
  local deadline completed
  deadline=$(( $(date +%s) + 120 ))
  completed=false

  while [ "$(date +%s)" -lt "$deadline" ]; do
    local status_out spec_status
    spec_status=$(python3 - "$BOI_DB" "$queue_id" 2>/dev/null <<'PYEOF'
import sqlite3, sys
db_path = sys.argv[1]
qid = sys.argv[2]
try:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    row = conn.execute("SELECT status FROM specs WHERE id=?", (qid,)).fetchone()
    conn.close()
    print(row[0] if row else "not_found")
except Exception as e:
    print(f"error:{e}")
PYEOF
    ) || spec_status="error"

    if [ "$spec_status" = "completed" ]; then
      completed=true
      break
    elif [ "$spec_status" = "failed" ] || [ "$spec_status" = "canceled" ]; then
      _warn "boi.smoke-test: smoke spec $queue_id ended with status: $spec_status"
      _rec 44 "boi.smoke-test" "warn" "smoke spec $queue_id ended with status $spec_status"
      return
    fi
    sleep 5
  done

  if $completed; then
    _pass "boi.smoke-test: smoke spec $queue_id dispatched and completed"
    _rec 44 "boi.smoke-test" "pass" "smoke spec $queue_id completed successfully"
  else
    _warn "boi.smoke-test: smoke spec $queue_id did not complete within 2 minutes"
    _rec 44 "boi.smoke-test" "warn" "smoke spec $queue_id timed out (still status: $spec_status)"
  fi
}

# run_boi_checks — run all BOI checks in dependency order
run_boi_checks() {
  check_41
  check_42
  check_43
  check_44
}
