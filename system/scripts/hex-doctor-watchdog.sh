#!/bin/bash
# hex-doctor-watchdog.sh — Background health check that feeds into startup.sh
#
# Runs hex-doctor --fix --quiet and writes issues to .hex/doctor-alert so
# startup.sh surfaces them at the beginning of each session.
#
# On healthy system: removes any stale doctor-alert (startup stays silent).
# On issues:         writes summary to doctor-alert (startup surfaces it).
#
# Called by: launchd com.hex.doctor-watchdog (every 15min + at load)
# Surfaced by: startup.sh's existing doctor-alert check

set -uo pipefail

# Source shared environment so we have the same PATH/env as interactive shell
HEX_ENV="${HEX_DIR:-${HEX_DIR:-$HOME/hex}}/.hex/scripts/env.sh"
[ -f "$HEX_ENV" ] && source "$HEX_ENV"

# ─── Resolve HEX_DIR ────────────────────────────────────────────────────────
HEX_DIR="${HEX_DIR:-}"
if [ -z "$HEX_DIR" ]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  # Walk up from scripts/ to find CLAUDE.md (agent root)
  candidate="$(dirname "$SCRIPT_DIR")"
  while [ "$candidate" != "/" ]; do
    if [ -f "$candidate/CLAUDE.md" ]; then
      HEX_DIR="$candidate"
      break
    fi
    candidate="$(dirname "$candidate")"
  done
  HEX_DIR="${HEX_DIR:-${HEX_DIR:-$HOME/hex}}"
fi

HEX_CHECKS_DIR="$HEX_DIR/.hex/scripts"
HEX_DOCTOR="$HEX_CHECKS_DIR/doctor.sh"
ALERT_DIR="$HEX_DIR/.hex"
ALERT_FILE="$ALERT_DIR/doctor-alert"

# Ensure .hex dir exists
mkdir -p "$ALERT_DIR"

# hex-doctor must be present and executable
if [ ! -x "$HEX_DOCTOR" ]; then
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] hex-doctor not found at $HEX_DOCTOR — skipping" >&2
  exit 0
fi

# ── Agent auto-recovery: check for circuit-tripped agents ─────────────────────
# If an agent self-halted via circuit breaker, verify the underlying issue is
# fixed (claude binary reachable via env.sh), then remove HALT to let it retry.
# Agent list is discovered from charters via hex-agent list — no hardcoded IDs.
HEX_AGENT="$HEX_DIR/.hex/bin/hex"
if [ ! -x "$HEX_AGENT" ]; then
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] hex-agent binary not found at $HEX_AGENT — skipping agent recovery" >&2
fi
while IFS= read -r _aid; do
  [ -z "$_aid" ] && continue
  _halt="$HOME/.hex-${_aid}-HALT"
  [ -f "$_halt" ] || continue

  # Check if claude is reachable via env.sh
  if bash -c "source '$HEX_ENV' && command -v claude" &>/dev/null; then
    # Underlying issue is fixed — remove HALT so agent can retry
    rm -f "$_halt"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] auto-recovered agent $_aid (claude reachable, HALT removed)" >&2
  else
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] agent $_aid halted — claude still not reachable, keeping HALT" >&2
  fi
done < <([ -x "$HEX_AGENT" ] && "$HEX_AGENT" agent list 2>/dev/null)

# ── Run doctor checks ────────────────────────────────────────────────────────
doctor_out=""
doctor_exit=0
doctor_out=$("$HEX_DOCTOR" --fix --quiet 2>&1) || doctor_exit=$?

if [ $doctor_exit -eq 0 ]; then
  # All healthy — clear any stale alert
  rm -f "$ALERT_FILE"
  exit 0
fi

# Issues remain after auto-fix attempt — write alert for startup.sh to surface
# Strip ANSI escape codes before writing (startup.sh reads this as plain text)
doctor_plain=$(echo "$doctor_out" | sed 's/\x1b\[[0-9;]*m//g')
{
  echo "Checked at $(date '+%Y-%m-%d %H:%M:%S') — exit $doctor_exit"
  echo ""
  echo "$doctor_plain" | grep -E '\[ERROR\]|\[FAIL\]|\[WARN\]' | head -15
  echo ""
  echo "Run hex-doctor --fix to attempt repairs."
} > "$ALERT_FILE"

# Exit 0 for warnings (exit 2) so launchd doesn't mark the agent as failed.
# Only propagate actual errors (exit 1 or other non-2 codes).
if [ $doctor_exit -eq 2 ]; then
  exit 0
fi
exit $doctor_exit
