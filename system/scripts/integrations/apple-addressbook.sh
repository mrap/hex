#!/usr/bin/env bash
# probe.sh — apple-addressbook integration health check
# Requires TCC grant: System Preferences > Privacy > Contacts → allow Terminal/claude
set -uo pipefail

emit_event() {
  local event="$1" status="$2" msg="$3"
  printf '{"event":"%s","status":"%s","message":"%s","ts":"%s"}\n' \
    "$event" "$status" "$msg" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >&2
}

echo "[apple-addressbook/probe] searching contacts for 'Mike'..."

COUNT=$(osascript <<'APPLESCRIPT' 2>/dev/null
tell application "Contacts"
  set matches to (every person whose name contains "Mike")
  return count of matches
end tell
APPLESCRIPT
) || COUNT=0

if [[ -z "$COUNT" ]]; then
  COUNT=0
fi

if [[ "$COUNT" -gt 0 ]]; then
  emit_event "hex.integration.apple-addressbook.probe_ok" "ok" "found $COUNT contact(s) matching 'Mike'"
  echo "[apple-addressbook/probe] OK ($COUNT contacts found)"
  exit 0
else
  emit_event "hex.integration.apple-addressbook.probe_fail" "fail" "no contacts found — TCC grant may be missing (see A-3)"
  echo "[apple-addressbook/probe] FAIL: 0 contacts found — check TCC grant (A-3)" >&2
  exit 1
fi
