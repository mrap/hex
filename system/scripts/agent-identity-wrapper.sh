#!/usr/bin/env bash
# agent-identity-wrapper.sh
#
# cli_path wrapper for cc-connect. Intercepts claude invocations and injects
# agent identity context when the message arrives in a bound agent channel.
#
# Environment vars provided by cc-connect before calling cli_path:
#   CC_CHAT_NAME   — Slack channel name (e.g., "hex-brand")
#   CC_SESSION_KEY — Full session key (slack:CHANNEL:USER)
#   CC_PROJECT     — cc-connect project name
#
# Behavior:
#   - Bound channel (in agent-channels.yaml): prepends AGENT IDENTITY preamble
#     (charter + last-5 working memory) to --append-system-prompt, then calls claude.
#   - Unbound channel (#hex-main, system channels): passes $@ through unchanged.

set -uo pipefail

BINDINGS_FILE="$HOME/.cc-connect/agent-channels.yaml"
HEX_DIR="${AGENT_DIR:-$HOME/hex}"
CHANNEL="${CC_CHAT_NAME:-}"

# No channel name or no bindings file → generic hex behavior
if [[ -z "$CHANNEL" || ! -f "$BINDINGS_FILE" ]]; then
    exec claude "$@"
fi

# Look up agent_id for this channel (empty string if unbound)
AGENT_ID=$(python3 - <<PYEOF 2>/dev/null
import yaml
with open('$BINDINGS_FILE') as f:
    d = yaml.safe_load(f)
ch = d.get('channels', {}).get('$CHANNEL', {})
print(ch.get('agent_id', ''))
PYEOF
)

# Unbound channel → generic hex behavior
if [[ -z "$AGENT_ID" ]]; then
    exec claude "$@"
fi

# ── Load agent context ────────────────────────────────────────────────────────

CHARTER_REL=$(python3 - <<PYEOF 2>/dev/null
import yaml
with open('$BINDINGS_FILE') as f:
    d = yaml.safe_load(f)
print(d['channels']['$CHANNEL']['charter'])
PYEOF
)

STATE_REL=$(python3 - <<PYEOF 2>/dev/null
import yaml
with open('$BINDINGS_FILE') as f:
    d = yaml.safe_load(f)
print(d['channels']['$CHANNEL']['state'])
PYEOF
)

CHARTER_PATH="$HEX_DIR/$CHARTER_REL"
STATE_PATH="$HEX_DIR/$STATE_REL"

CHARTER_TEXT=""
if [[ -f "$CHARTER_PATH" ]]; then
    CHARTER_TEXT=$(cat "$CHARTER_PATH")
fi

# Last 5 working memory entries from state.json
MEMORY_SUMMARY=""
if [[ -f "$STATE_PATH" ]]; then
    MEMORY_SUMMARY=$(python3 - <<PYEOF 2>/dev/null
import json
with open('$STATE_PATH') as f:
    s = json.load(f)
mem = s.get('memory', [])
if isinstance(mem, list):
    recent = mem[-5:]
elif isinstance(mem, dict):
    recent = list(mem.items())[-5:]
else:
    recent = []
for item in recent:
    print('-', item if isinstance(item, str) else json.dumps(item))
PYEOF
)
fi

# ── Build identity preamble ───────────────────────────────────────────────────

PREAMBLE="AGENT IDENTITY — READ THIS FIRST

You are the $AGENT_ID agent operating in the hex workspace. This message arrived
in your dedicated Slack channel (#$CHANNEL). You respond as the $AGENT_ID agent,
not as generic hex.

The hex CLAUDE.md below provides workspace context and capabilities — treat it as
ambient context, not as your primary identity. Your charter below defines who you
are and what you own.

## Your Charter

$CHARTER_TEXT

## Your Working Memory (last 5 entries)

${MEMORY_SUMMARY:-No working memory entries.}

---
END AGENT IDENTITY
"

# ── Inject preamble into --append-system-prompt ───────────────────────────────
# Strategy: find existing --append-system-prompt in $@ and prepend to it.
# If not present, append a new one at the end.

ARGS=("$@")
FOUND=0
NEW_ARGS=()
i=0
while [[ $i -lt ${#ARGS[@]} ]]; do
    arg="${ARGS[$i]}"
    if [[ "$arg" == "--append-system-prompt" && $((i+1)) -lt ${#ARGS[@]} ]]; then
        NEW_ARGS+=("--append-system-prompt" "${PREAMBLE}

${ARGS[$((i+1))]}")
        i=$((i+2))
        FOUND=1
    else
        NEW_ARGS+=("$arg")
        i=$((i+1))
    fi
done

if [[ $FOUND -eq 0 ]]; then
    NEW_ARGS+=("--append-system-prompt" "$PREAMBLE")
fi

exec claude "${NEW_ARGS[@]}"
