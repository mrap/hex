#!/usr/bin/env bash
# hex-agent-initiative-wrapper.sh
#
# Drop-in wrapper for hex-agent that ensures the initiative execution loop runs
# on EVERY wake for initiative-owning agents, before the LLM is invoked.
#
# WHY: The initiative loop is a harness-level responsibility — it must not depend
# on the agent deciding to run it. This wrapper mechanically runs the loop first,
# writes its output as a context file (injected into the agent's prompt via
# context_files in charter.yaml), and appends an audit entry. The agent always
# sees initiative recommendations; the loop's actions (dispatches, baselines)
# always execute.
#
# Usage: identical to hex-agent
#   hex-agent-initiative-wrapper.sh --agent <id> --trigger <event> [--payload <json>]
#
# Integration:
#   1. This script installed at $HEX_ROOT/.hex/scripts/hex-agent-initiative-wrapper.sh
#   2. Charter context_files includes projects/<agent>/initiative-context.md
#   3. hex-events policies invoke this wrapper instead of hex-agent directly

set -uo pipefail

HEX_ROOT="${HEX_ROOT:-${AGENT_DIR:-$HOME/hex}}"
# Prefer the installed -bin alongside the wrapper; fall back to build output
if [[ -f "$HEX_ROOT/.hex/bin/hex-agent-bin" ]]; then
    HEX_AGENT_BIN="$HEX_ROOT/.hex/bin/hex-agent-bin"
else
    HEX_AGENT_BIN="$HEX_ROOT/.hex/harness/target/release/hex-agent"
fi
AUDIT_FILE="$HEX_ROOT/.hex/audit/actions.jsonl"
SCRIPTS_DIR="$HEX_ROOT/.hex/scripts"

# ── Parse agent ID from args ──────────────────────────────────────────────────
# Supports two call forms:
#   hex-agent wake <agent_id> [--trigger X] [--payload Y]   (policy invocations)
#   hex-agent --agent <agent_id> --trigger X                 (legacy form)

AGENT_ID=""
PASS_ARGS=("$@")

_ARGS=("$@")
_LEN="${#_ARGS[@]}"
for (( i=0; i<_LEN; i++ )); do
    case "${_ARGS[$i]}" in
        --agent)
            AGENT_ID="${_ARGS[$((i+1))]:-}"
            ;;
        wake)
            # Next positional arg after 'wake' is the agent id
            if [[ $((i+1)) -lt $_LEN ]]; then
                _NEXT="${_ARGS[$((i+1))]}"
                if [[ "$_NEXT" != --* ]]; then
                    AGENT_ID="$_NEXT"
                fi
            fi
            ;;
    esac
done

if [[ -z "$AGENT_ID" ]]; then
    # Not a wake command (e.g. status, fleet, list) — pass through directly
    exec "$HEX_AGENT_BIN" "${PASS_ARGS[@]}"
fi

# ── Paths ─────────────────────────────────────────────────────────────────────

CONTEXT_FILE="$HEX_ROOT/projects/$AGENT_ID/initiative-context.md"
LOOP_SCRIPT="$SCRIPTS_DIR/hex-initiative-loop-v2.py"
TS=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

# ── Helper: write audit entry ─────────────────────────────────────────────────

_audit() {
    local action="$1"
    local detail="$2"
    local entry
    entry=$(printf '{"ts":"%s","agent":"%s","action":"%s","detail":%s}' \
        "$TS" "$AGENT_ID" "$action" "$detail")
    mkdir -p "$(dirname "$AUDIT_FILE")"
    echo "$entry" >> "$AUDIT_FILE"
}

# ── Step 1: Check if agent owns initiatives ───────────────────────────────────

INITIATIVE_COUNT=0
if [[ -d "$HEX_ROOT/initiatives" ]]; then
    INITIATIVE_COUNT=$(grep -rl "owner: $AGENT_ID" "$HEX_ROOT/initiatives/" 2>/dev/null | wc -l | tr -d ' ')
fi

if [[ "$INITIATIVE_COUNT" -eq 0 ]]; then
    # Agent owns no initiatives — skip loop, pass through directly
    exec "$HEX_AGENT_BIN" "${PASS_ARGS[@]}"
fi

# ── Step 2: Run the initiative loop ──────────────────────────────────────────

_audit "initiative-loop-pre-wake-start" \
    "{\"agent\":\"$AGENT_ID\",\"initiative_count\":$INITIATIVE_COUNT,\"context_file\":\"$CONTEXT_FILE\"}"

LOOP_OUTPUT=""
LOOP_EXIT=0

if [[ -f "$LOOP_SCRIPT" ]]; then
    # Run the loop and capture JSON output
    if LOOP_OUTPUT=$(python3 "$LOOP_SCRIPT" --agent "$AGENT_ID" 2>&1); then
        LOOP_EXIT=0
    else
        LOOP_EXIT=$?
    fi
else
    LOOP_OUTPUT='{"error":"loop script not found","actions":[]}'
    LOOP_EXIT=1
fi

# ── Step 3: Write context file for prompt injection ───────────────────────────

mkdir -p "$(dirname "$CONTEXT_FILE")"
{
    echo "# Initiative Loop — Pre-Wake Execution Report"
    echo ""
    echo "**Generated:** $TS"
    echo "**Agent:** $AGENT_ID"
    echo "**Status:** $([ $LOOP_EXIT -eq 0 ] && echo 'completed' || echo 'errored')"
    echo ""
    echo "The initiative execution loop ran before this wake. These actions were taken:"
    echo ""
    echo '```json'
    # Pretty-print if possible, fallback to raw
    if echo "$LOOP_OUTPUT" | python3 -m json.tool 2>/dev/null; then
        : # printed by python3
    else
        echo "$LOOP_OUTPUT"
    fi
    echo '```'
    echo ""
    echo "**YOUR ROLE NOW**: Review the loop output above. Acknowledge what was dispatched."
    echo "If any KR is still at 0% with no active experiment, the loop failed to dispatch —"
    echo "investigate why and take manual action this wake. You cannot park on a 0% KR."
} > "$CONTEXT_FILE.tmp"
mv "$CONTEXT_FILE.tmp" "$CONTEXT_FILE"

# ── Step 4: Parse action count and write audit summary ───────────────────────

ACTION_COUNT=0
DISPATCH_COUNT=0
if echo "$LOOP_OUTPUT" | python3 -c "
import sys, json
d = json.load(sys.stdin)
actions = d.get('actions', [])
print(len(actions))
print(sum(1 for a in actions if a.get('action') == 'dispatch_spec'))
" 2>/dev/null; then
    # Read counts from stdout (two lines)
    COUNTS=$(echo "$LOOP_OUTPUT" | python3 -c "
import sys, json
d = json.load(sys.stdin)
actions = d.get('actions', [])
print(len(actions), sum(1 for a in actions if a.get('action') == 'dispatch_spec'))
" 2>/dev/null || echo "0 0")
    ACTION_COUNT=$(echo "$COUNTS" | awk '{print $1}')
    DISPATCH_COUNT=$(echo "$COUNTS" | awk '{print $2}')
fi

_audit "initiative-loop-pre-wake-complete" \
    "{\"agent\":\"$AGENT_ID\",\"exit_code\":$LOOP_EXIT,\"action_count\":${ACTION_COUNT:-0},\"dispatch_count\":${DISPATCH_COUNT:-0},\"context_file\":\"$CONTEXT_FILE\"}"

# ── Step 5: Call the real hex-agent ───────────────────────────────────────────

exec "$HEX_AGENT_BIN" "${PASS_ARGS[@]}"
