#!/bin/bash
# Backup current Claude Code session .jsonl to the agent's raw/transcripts/
# Called by hooks on UserPromptSubmit and Stop
# Works on both macOS and Linux

set -uo pipefail

PROJECTS_DIR="$HOME/.claude/projects"

# Resolve agent root from script location (.hex/hooks/scripts/)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ "$SCRIPT_DIR" == */.hex/hooks/scripts ]]; then
    HEX_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
elif [[ "$SCRIPT_DIR" == */hooks/scripts ]]; then
    HEX_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
else
    HEX_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
fi
BACKUP_DIR="$HEX_DIR/raw/transcripts"

mkdir -p "$BACKUP_DIR"

# Log subshell output to a daily file instead of discarding it.
LOG_DIR="$HEX_DIR/.hex/logs/stop-hooks"
mkdir -p "$LOG_DIR"
LOG_FILE="$LOG_DIR/backup_session-$(date +%Y-%m-%d).log"
find "$LOG_DIR" -name '*.log' -mtime +14 -delete 2>/dev/null || true

# Run the copy in the background so the hook returns immediately.
(
    LATEST=""

    # Fast path: use CLAUDE_SESSION_ID + CLAUDE_PROJECT_DIR when available (O(1) vs O(N) find).
    if [[ -n "${CLAUDE_SESSION_ID:-}" && -n "${CLAUDE_PROJECT_DIR:-}" ]]; then
        _SLUG=$(printf '%s' "$CLAUDE_PROJECT_DIR" | sed 's|/|-|g')
        _CANDIDATE="$PROJECTS_DIR/${_SLUG}/${CLAUDE_SESSION_ID}.jsonl"
        if [[ -f "$_CANDIDATE" ]]; then
            LATEST="$_CANDIDATE"
        fi
    fi

    # Fallback: scan for most-recently-modified .jsonl (legacy path, slower).
    if [[ -z "$LATEST" ]]; then
        if [[ "$OSTYPE" == "darwin"* ]]; then
            LATEST=$(find "$PROJECTS_DIR" -maxdepth 2 -name "*.jsonl" -exec stat -f '%m %N' {} \; 2>/dev/null \
                | sort -rn | head -1 | cut -d' ' -f2-)
        else
            LATEST=$(find "$PROJECTS_DIR" -maxdepth 2 -name "*.jsonl" -printf '%T@ %p\n' 2>/dev/null \
                | sort -rn | head -1 | cut -d' ' -f2-)
        fi
    fi

    if [ -n "${LATEST:-}" ]; then
        FILENAME=$(basename "$LATEST")
        cp "$LATEST" "$BACKUP_DIR/$FILENAME" 2>/dev/null || true
    fi
) </dev/null >>"$LOG_FILE" 2>&1 &
disown 2>/dev/null || true
