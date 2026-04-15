#!/usr/bin/env bash
# hex startup — Run at the beginning of each session.
# Called by /hex-startup command.

set -euo pipefail

HEX_DIR="${HEX_DIR:-$(pwd)}"

echo "hex startup..."

# Rebuild memory index (incremental — only re-indexes changed files)
if [ -f "$HEX_DIR/.hex/skills/memory/scripts/memory_index.py" ]; then
    python3 "$HEX_DIR/.hex/skills/memory/scripts/memory_index.py" 2>/dev/null && \
        echo "  Memory index rebuilt ✓" || \
        echo "  Memory index: skipped (no changes)"
fi

# Check memory.db exists
if [ ! -f "$HEX_DIR/.hex/memory.db" ]; then
    echo "  WARNING: memory.db not found. Run install.sh to initialize."
fi

# Show today's date
TODAY=$(date +%Y-%m-%d)
echo "  Date: $TODAY"

# Check for today's landings
if [ -f "$HEX_DIR/landings/$TODAY.md" ]; then
    echo "  Landings: found for today"
else
    echo "  Landings: none for today (will propose during startup)"
fi

echo "  Ready."
