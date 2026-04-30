#!/usr/bin/env bash
set -uo pipefail

# Run osascript in background with manual timeout (no `timeout` on macOS)
TMPOUT=$(mktemp)
osascript -e 'tell application "Contacts" to count every person' >"$TMPOUT" 2>&1 &
OSPID=$!

# Wait up to 15 seconds
WAITED=0
while kill -0 "$OSPID" 2>/dev/null; do
  sleep 1
  WAITED=$((WAITED + 1))
  if [[ $WAITED -ge 15 ]]; then
    kill "$OSPID" 2>/dev/null || true
    rm -f "$TMPOUT"
    echo "ERROR: osascript timed out after 15s — Contacts.app may be frozen" >&2
    exit 1
  fi
done

wait "$OSPID"
OSASCRIPT_EXIT=$?
OUTPUT=$(cat "$TMPOUT")
rm -f "$TMPOUT"

if [[ $OSASCRIPT_EXIT -ne 0 ]]; then
  echo "ERROR: osascript failed (exit $OSASCRIPT_EXIT): $OUTPUT" >&2
  exit 1
fi

# Strip whitespace
COUNT=$(echo "$OUTPUT" | tr -d '[:space:]')

# Validate output is a number
if ! [[ "$COUNT" =~ ^[0-9]+$ ]]; then
  echo "ERROR: osascript returned non-numeric output: $OUTPUT" >&2
  exit 1
fi

if [[ "$COUNT" -eq 0 ]]; then
  # A-3 known issue: returns 0 for known contacts — treat as warning only
  echo "WARN: Contacts count is 0 — possible A-3 bug or TCC permission issue (non-fatal)" >&2
  exit 0
fi

echo "OK: Contacts.app returned $COUNT contacts" >&2
exit 0
