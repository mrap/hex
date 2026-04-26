#!/usr/bin/env bash
# hex-session-reset.sh — Reset the cc-connect session for a clean start
# Called by the agent after checkpointing context to files.
# The next user message will start a fresh Claude Code session.
#
# Usage: bash hex-session-reset.sh [channel_key]
# Default channel_key: slack:C0AQGHS8RNG:U0AQACA26NS (hex-main)

set -euo pipefail

CC_CONNECT="$HOME/bin/cc-connect"
SESSION_DIR="$HOME/.cc-connect/sessions"
CHANNEL_KEY="${1:-slack:C0AQGHS8RNG:U0AQACA26NS}"

echo "[hex-session-reset] Resetting session for $CHANNEL_KEY"

# 1. Find and modify the session file — clear agent_session_id so next message gets a fresh CC session
for f in "$SESSION_DIR"/*.json; do
    [ -f "$f" ] || continue
    if python3 -c "
import json, sys
with open('$f') as fh:
    d = json.load(fh)
if '$CHANNEL_KEY' in d.get('active_session', {}):
    # Archive the session ID before clearing
    for sid in d.get('user_sessions', {}).get('$CHANNEL_KEY', []):
        if sid in d.get('sessions', {}):
            old_id = d['sessions'][sid].get('agent_session_id', '')
            print(f'[hex-session-reset] Archived session {sid} (agent: {old_id[:20]}...)')
            # Clear the agent session so cc-connect creates a fresh one
            d['sessions'][sid]['agent_session_id'] = ''
            d['sessions'][sid]['history'] = []
            d['sessions'][sid]['name'] = 'checkpointed'
    with open('$f', 'w') as fh:
        json.dump(d, fh, indent=2)
    print('[hex-session-reset] Session cleared')
    sys.exit(0)
sys.exit(1)
" 2>/dev/null; then
        break
    fi
done

# 2. Restart cc-connect daemon to pick up the cleared session
echo "[hex-session-reset] Restarting cc-connect..."
"$CC_CONNECT" daemon stop 2>/dev/null || true
sleep 1
"$CC_CONNECT" daemon install --work-dir "$HOME/.cc-connect" --force 2>/dev/null
echo "[hex-session-reset] Done. Next message will start a fresh Claude Code session."
