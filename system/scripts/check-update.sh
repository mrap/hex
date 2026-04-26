#!/bin/bash
# check-update.sh — Check if a newer version of hex is available
#
# Runs silently. Writes state to flag files in .hex/:
#   .update-available  → present with remote SHA if update available; absent if up to date
#   .update-checked    → timestamp of last successful check (throttle: once per 24h)
#
# Designed to run in the background (non-blocking from startup.sh).
# Always exits 0. Network failures are silent.

set -uo pipefail

# ─── Resolve paths ───────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
CLAUDE_DIR="$AGENT_DIR/.hex"
CONFIG_FILE="$CLAUDE_DIR/upgrade.json"
UPDATE_CHECKED="$CLAUDE_DIR/.update-checked"
UPDATE_AVAILABLE="$CLAUDE_DIR/.update-available"

# ─── Throttle: skip if checked within last 24h ───────────────────────────────
if [ -f "$UPDATE_CHECKED" ]; then
  last_check=$(cat "$UPDATE_CHECKED" 2>/dev/null || echo "0")
  now=$(date +%s)
  age=$(( now - last_check ))
  if [ "$age" -lt 86400 ]; then
    # Checked within 24 hours — skip network call
    exit 0
  fi
fi

# ─── Read config ─────────────────────────────────────────────────────────────
if [ ! -f "$CONFIG_FILE" ]; then
  # No config file — cannot check
  exit 0
fi

REPO_URL=$(python3 -c "import json; print(json.load(open('$CONFIG_FILE')).get('repo', ''))" 2>/dev/null || echo "")
if [ -z "$REPO_URL" ]; then
  exit 0
fi

LAST_SHA=$(python3 -c "import json; print(json.load(open('$CONFIG_FILE')).get('last_remote_sha', ''))" 2>/dev/null || echo "")

# ─── Fetch remote SHA ────────────────────────────────────────────────────────
# Use git-native timeout env vars (works on all platforms, no coreutils needed)
# Aborts if transfer drops below 1KB/s for 5 seconds
REMOTE_SHA=$(GIT_HTTP_LOW_SPEED_LIMIT=1000 GIT_HTTP_LOW_SPEED_TIME=5 \
  git ls-remote --heads "$REPO_URL" refs/heads/main 2>/dev/null | cut -f1 || echo "")

if [ -z "$REMOTE_SHA" ]; then
  # Network failure or offline — leave existing flag files unchanged
  exit 0
fi

# ─── Compare and update flag files ───────────────────────────────────────────
if [ -z "$LAST_SHA" ]; then
  # No baseline SHA (fresh install, never upgraded) — skip to avoid false positive.
  # Record the current remote as baseline so future checks work.
  # Silently seed last_remote_sha so next check has a baseline
  python3 -c "
import json, os, sys
sha, path = sys.argv[1], sys.argv[2]
try:
    with open(path) as f: data = json.load(f)
except Exception: data = {}
data['last_remote_sha'] = sha
tmp = path + '.tmp'
with open(tmp, 'w') as f: json.dump(data, f, indent=2); f.write('\n')
os.rename(tmp, path)
" "$REMOTE_SHA" "$CONFIG_FILE" 2>/dev/null || true
  rm -f "$UPDATE_AVAILABLE"
elif [ "$REMOTE_SHA" != "$LAST_SHA" ]; then
  # Update available
  printf '%s\n' "$REMOTE_SHA" > "${UPDATE_AVAILABLE}.tmp"
  mv "${UPDATE_AVAILABLE}.tmp" "$UPDATE_AVAILABLE"
else
  # Up to date
  rm -f "$UPDATE_AVAILABLE"
fi

# Record timestamp of this check
printf '%s\n' "$(date +%s)" > "${UPDATE_CHECKED}.tmp"
mv "${UPDATE_CHECKED}.tmp" "$UPDATE_CHECKED"

exit 0
