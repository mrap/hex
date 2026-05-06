#!/usr/bin/env bash
# check-hex-events-policy-load.sh — surface POLICY LOAD/VALIDATION ERROR
# entries from the hex-events daemon log.
#
# The daemon logs these to ~/.hex-events/daemon.log but never emits an event,
# so legacy-schema policies, deprecated-field policies, and invalid configs
# all sit silently dead until someone reads the log. Earlier this session
# we found 4 such policies running undetected.
#
# Exits 0 if no errors in the last $WINDOW_HOURS hours, 1 otherwise.

set -uo pipefail

DAEMON_LOG="${HEX_EVENTS_DAEMON_LOG:-$HOME/.hex-events/daemon.log}"
WINDOW_HOURS="${POLICY_LOAD_WINDOW_HOURS:-2}"
HEX_ALERT="${HEX_ALERT:-${HEX_DIR:-$HOME/hex}/.hex/scripts/hex-alert.sh}"

if [ ! -f "$DAEMON_LOG" ]; then
  echo "check-hex-events-policy-load: SKIP — daemon log not found at $DAEMON_LOG"
  exit 0
fi

# Get the cutoff timestamp.
if [ "$(uname)" = "Darwin" ]; then
  cutoff="$(date -u -v -${WINDOW_HOURS}H +%Y-%m-%d\ %H:%M:%S)"
else
  cutoff="$(date -u -d "${WINDOW_HOURS} hours ago" +%Y-%m-%d\ %H:%M:%S)"
fi

# Count errors in the window. Daemon log lines look like:
#   2026-05-05 21:33:22,849 hex-events ERROR [POLICY VALIDATION ERROR] /path/...
# The daemon also echoes un-timestamped duplicates starting with '[POLICY ...'.
# We require the line to start with a 4-digit year to exclude those duplicates.
# Use awk for time comparison so we don't need GNU date / coreutils.
errors="$(awk -v cutoff="$cutoff" '
  /^[0-9]{4}-/ && /POLICY (LOAD|VALIDATION) ERROR/ {
    ts = $1 " " substr($2, 1, 8)
    if (ts >= cutoff) print
  }
' "$DAEMON_LOG" | tail -50)"

if [ -z "$errors" ]; then
  echo "check-hex-events-policy-load: ok — no policy load/validation errors in last ${WINDOW_HOURS}h"
  exit 0
fi

count="$(echo "$errors" | wc -l | tr -d ' ')"
# Extract unique policy filenames for a compact alert message.
unique_files="$(echo "$errors" | grep -oE '/[^ ]+\.yaml' | sort -u | head -10)"

msg="${count} POLICY LOAD/VALIDATION ERROR(s) in last ${WINDOW_HOURS}h.\nFiles:\n${unique_files}\nLog: ${DAEMON_LOG}"

echo "check-hex-events-policy-load: FAIL — $msg" >&2

if [ -x "$HEX_ALERT" ] && [ "${SKIP_ALERT:-0}" != "1" ]; then
  "$HEX_ALERT" ERROR "policy-load-errors" "$msg"
fi

exit 1
