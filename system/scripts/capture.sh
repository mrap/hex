#!/usr/bin/env bash
# hex-capture: Zero-friction context capture for hex agents.
# Works OUTSIDE of Claude Code. No LLM needed. Pure save.
#
# Usage:
#   hex-capture "your thought here"
#   echo "piped text" | hex-capture
#   hex-capture <<EOF
#   multi-line
#   content
#   EOF
#   hex-capture          # opens $EDITOR or reads from stdin

set -uo pipefail

# Auto-detect workspace: this script lives at {workspace}/.hex/scripts/capture.sh
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="$(cd "$SCRIPT_DIR/../.." && pwd)"
CAPTURES_DIR="$WORKSPACE/raw/captures"

# Ensure captures directory exists
mkdir -p "$CAPTURES_DIR"

# Collect text from args, stdin, or $EDITOR
TEXT=""

if [ $# -gt 0 ]; then
    # Inline: hex-capture "some text"
    TEXT="$*"
elif [ ! -t 0 ]; then
    # Piped or heredoc: echo "text" | hex-capture
    TEXT="$(cat)"
else
    # Interactive: open editor or read from terminal
    if [ -n "${EDITOR:-}" ]; then
        TMPFILE="$(mktemp)"
        trap 'rm -f "$TMPFILE"' EXIT
        "$EDITOR" "$TMPFILE"
        TEXT="$(cat "$TMPFILE")"
    else
        echo "Type your capture (Ctrl+D when done):"
        TEXT="$(cat)"
    fi
fi

# Bail if empty
if [ -z "${TEXT// /}" ]; then
    echo "Nothing to capture."
    exit 0
fi

# Generate filename and timestamp
# Use configured timezone from .hex/timezone (if set)
if [ -z "${TZ:-}" ] && [ -f "$AGENT_DIR/.hex/timezone" ]; then
  TZ="$(cat "$AGENT_DIR/.hex/timezone" | tr -d '[:space:]')"; export TZ
fi
TIMESTAMP="$(date '+%Y-%m-%dT%H:%M:%S')"
FILENAME="$(date '+%Y-%m-%d_%H-%M-%S').md"
OUTFILE="$CAPTURES_DIR/$FILENAME"

# Write capture with metadata header (atomic: write to .tmp, then mv)
TMPOUT="$OUTFILE.tmp"
cat > "$TMPOUT" <<CAPTURE_EOF
---
captured: $TIMESTAMP
source: cli
---

$TEXT
CAPTURE_EOF

mv "$TMPOUT" "$OUTFILE"

# Emit telemetry: capture created
"$WORKSPACE/.hex/bin/hex-emit.sh" "capture.created" \
  "{\"path\":\"$OUTFILE\",\"source\":\"cli\",\"timestamp\":\"$TIMESTAMP\"}" \
  "capture-script" || true

echo "Captured. Will triage on next session startup."
