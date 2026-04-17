#!/usr/bin/env bash
set -euo pipefail

# hex install — Creates a hex instance on the user's machine.
# Usage: bash install.sh [target_dir]
#
# hex is an all-or-nothing package. BOI (parallel workers) and hex-events
# (reactive automation) are integral — there are no flags to skip them.
#
# The repo is the installer, not the workspace. This script creates a
# separate instance directory. The repo is disposable after install.

VERSION=$(cat "$(dirname "${BASH_SOURCE[0]}")/system/version.txt" 2>/dev/null || echo "0.1.0")
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_DIR=""

for arg in "$@"; do
    case "$arg" in
        --help|-h)   echo "Usage: bash install.sh [target_dir]"; exit 0 ;;
        -*)          echo "Unknown flag: $arg"; exit 1 ;;
        *)           TARGET_DIR="$arg" ;;
    esac
done

TARGET_DIR="${TARGET_DIR:-$HOME/hex}"
TARGET_DIR="${TARGET_DIR/#\~/$HOME}"

echo "hex v${VERSION} installer"
echo "========================"
echo ""

# ── Phase 1: Validate environment ──────────────────────────────────

echo "Checking prerequisites..."

if ! command -v python3 &>/dev/null; then
    echo "ERROR: Python 3 is required but not found."
    echo "  Install: https://www.python.org/downloads/"
    exit 1
fi

PY_VERSION=$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')
PY_MAJOR=$(echo "$PY_VERSION" | cut -d. -f1)
PY_MINOR=$(echo "$PY_VERSION" | cut -d. -f2)
if [ "$PY_MAJOR" -lt 3 ] || { [ "$PY_MAJOR" -eq 3 ] && [ "$PY_MINOR" -lt 9 ]; }; then
    echo "ERROR: Python 3.9+ required (found $PY_VERSION)."
    echo "  Install: https://www.python.org/downloads/"
    exit 1
fi

if ! command -v git &>/dev/null; then
    echo "ERROR: git is required but not found."
    echo "  Install: https://git-scm.com/downloads"
    exit 1
fi

if ! command -v claude &>/dev/null; then
    echo "NOTE: Claude Code CLI not found. Install it to use hex:"
    echo "  npm install -g @anthropic-ai/claude-code"
    echo ""
fi

if [ -d "$TARGET_DIR" ]; then
    echo "ERROR: $TARGET_DIR already exists."
    echo "  To upgrade:   bash \"$TARGET_DIR/.hex/scripts/upgrade.sh\""
    echo "  To reinstall: rm -rf \"$TARGET_DIR\" && bash install.sh"
    exit 1
fi

echo "  Python $PY_VERSION  ✓"
echo "  git               ✓"
echo ""

# ── Phase 2: Create instance directory structure ───────────────────

echo "Creating hex instance at $TARGET_DIR..."

mkdir -p "$TARGET_DIR"/{me/decisions,projects/_archive,people}
mkdir -p "$TARGET_DIR"/evolution
mkdir -p "$TARGET_DIR"/landings/weekly
mkdir -p "$TARGET_DIR"/raw/{transcripts,handoffs}
mkdir -p "$TARGET_DIR"/specs/_archive

# Copy system files → .hex/
cp -r "$SCRIPT_DIR/system" "$TARGET_DIR/.hex"

# Copy root templates
cp "$SCRIPT_DIR/templates/CLAUDE.md"  "$TARGET_DIR/CLAUDE.md"
cp "$SCRIPT_DIR/templates/AGENTS.md"  "$TARGET_DIR/AGENTS.md"
cp "$SCRIPT_DIR/templates/todo.md"    "$TARGET_DIR/todo.md"

# Copy user data templates
cp "$SCRIPT_DIR/templates/me.md"            "$TARGET_DIR/me/me.md"
cp "$SCRIPT_DIR/templates/learnings.md"     "$TARGET_DIR/me/learnings.md"
cp "$SCRIPT_DIR/templates/observations.md"  "$TARGET_DIR/evolution/observations.md"
cp "$SCRIPT_DIR/templates/suggestions.md"   "$TARGET_DIR/evolution/suggestions.md"
cp "$SCRIPT_DIR/templates/changelog.md"     "$TARGET_DIR/evolution/changelog.md"

# Copy tests
if [ -d "$SCRIPT_DIR/tests" ]; then
    cp -r "$SCRIPT_DIR/tests" "$TARGET_DIR/tests"
fi

# Copy commands to .claude/commands/ (where Claude Code discovers them)
if [ -d "$SCRIPT_DIR/system/commands" ]; then
    mkdir -p "$TARGET_DIR/.claude/commands"
    cp "$SCRIPT_DIR/system/commands/"*.md "$TARGET_DIR/.claude/commands/"
fi

echo "  Directory structure  ✓"

# ── Phase 3: Initialize memory ─────────────────────────────────────

echo "Initializing memory database..."

python3 -c "
import sqlite3, os
db = os.path.join('$TARGET_DIR', '.hex', 'memory.db')
conn = sqlite3.connect(db)
conn.executescript('''
    CREATE TABLE IF NOT EXISTS memories (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        content TEXT NOT NULL,
        tags TEXT DEFAULT \"\",
        source TEXT DEFAULT \"\",
        created_at TEXT NOT NULL
    );
    CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
        content, tags, source,
        content=memories, content_rowid=id,
        tokenize=\"unicode61\"
    );
    CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
        INSERT INTO memories_fts(rowid, content, tags, source)
        VALUES (new.id, new.content, new.tags, new.source);
    END;
    CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
        INSERT INTO memories_fts(memories_fts, rowid, content, tags, source)
        VALUES (\"delete\", old.id, old.content, old.tags, old.source);
    END;
    CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
        source_path, heading, chunk_index, content,
        tokenize=\"unicode61\"
    );
    CREATE TABLE IF NOT EXISTS files (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        path TEXT UNIQUE NOT NULL,
        mtime REAL NOT NULL,
        content_hash TEXT NOT NULL DEFAULT \"\",
        indexed_at TEXT NOT NULL,
        chunk_count INTEGER DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS metadata (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
''')
conn.commit()
conn.close()
"

echo "  Memory database     ✓"

# ── Phase 4: Create standing-orders reference ──────────────────────

mkdir -p "$TARGET_DIR/.hex/standing-orders"
cat > "$TARGET_DIR/.hex/standing-orders/README.md" << 'SOEOF'
# Standing Orders

The 20 core rules, 10 situational rules, and 6 product judgment rules are
defined in CLAUDE.md (system zone). This directory holds extended reference
copies with examples and context for each rule.

See CLAUDE.md → Standing Orders for the working copy.
SOEOF

echo "  Standing orders     ✓"

# ── Phase 5: Install companions ────────────────────────────────────

echo "Installing companions..."

# Read pinned versions from VERSIONS file (keeps install.sh in lock-step with
# tested boi/hex-events releases). Fork-friendly: HEX_BOI_REPO and
# HEX_EVENTS_REPO env vars override the default source.
VERSIONS_FILE="$SCRIPT_DIR/VERSIONS"
if [ ! -f "$VERSIONS_FILE" ]; then
    echo "ERROR: $VERSIONS_FILE not found — this hex-foundation checkout is incomplete."
    exit 1
fi
BOI_VERSION=$(grep "^BOI_VERSION=" "$VERSIONS_FILE" | cut -d= -f2)
HEX_EVENTS_VERSION=$(grep "^HEX_EVENTS_VERSION=" "$VERSIONS_FILE" | cut -d= -f2)
BOI_REPO="${HEX_BOI_REPO:-https://github.com/mrap/boi.git}"
HEX_EVENTS_REPO="${HEX_EVENTS_REPO:-https://github.com/mrap/hex-events.git}"

# BOI — parallel worker dispatch
if [ -d "$HOME/.boi" ]; then
    echo "  BOI already installed  ✓"
else
    if git clone --depth 1 --branch "$BOI_VERSION" "$BOI_REPO" "$HOME/.boi" 2>/dev/null; then
        echo "  BOI installed ($BOI_VERSION)  ✓"
    else
        echo "  BOI: failed to clone $BOI_REPO @ $BOI_VERSION (will install on next upgrade)"
    fi
fi

# hex-events — reactive event system
if [ -d "$HOME/.hex-events" ]; then
    echo "  hex-events already installed  ✓"
else
    if git clone --depth 1 --branch "$HEX_EVENTS_VERSION" "$HEX_EVENTS_REPO" "$HOME/.hex-events" 2>/dev/null; then
        echo "  hex-events installed ($HEX_EVENTS_VERSION)  ✓"
    else
        echo "  hex-events: failed to clone $HEX_EVENTS_REPO @ $HEX_EVENTS_VERSION (will install on next upgrade)"
    fi
fi

# ── Phase 6: Register install ──────────────────────────────────────

python3 -c "
import json, os
from datetime import datetime, timezone
info = {
    'install_path': '$TARGET_DIR',
    'install_date': datetime.now(timezone.utc).isoformat(),
    'version': '$VERSION'
}
with open(os.path.expanduser('~/.hex-install.json'), 'w') as f:
    json.dump(info, f, indent=2)
"

echo ""
echo "========================================="
echo " hex installed at $TARGET_DIR"
echo "========================================="
echo ""
echo "Start your first session:"
echo "  cd $TARGET_DIR && claude"
echo ""
echo "Your agent will walk you through setup."
