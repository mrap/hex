#!/usr/bin/env bash
set -euo pipefail

# hex setup — creates directory structure and initializes memory database
# Safe to run multiple times (idempotent)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HEX_DIR="$SCRIPT_DIR/.hex"

echo "hex · setting up persistent memory..."

# Create directory structure
mkdir -p "$HEX_DIR/memory"
mkdir -p "$HEX_DIR/landings"
mkdir -p "$HEX_DIR/evolution"
mkdir -p "$HEX_DIR/standing-orders"

# Initialize SQLite FTS5 memory database
python3 -c "
import sqlite3, os

db_path = os.path.join('$HEX_DIR', 'memory', 'memory.db')
conn = sqlite3.connect(db_path)
c = conn.cursor()

# Create memories table if it doesn't exist
c.execute('''CREATE TABLE IF NOT EXISTS memories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    tags TEXT DEFAULT '',
    source TEXT DEFAULT '',
    timestamp TEXT NOT NULL
)''')

# Create FTS5 virtual table if it doesn't exist
# Check first to avoid 'table already exists' error
c.execute(\"SELECT name FROM sqlite_master WHERE type='table' AND name='memories_fts'\")
if not c.fetchone():
    c.execute('''CREATE VIRTUAL TABLE memories_fts USING fts5(
        content,
        tags,
        source,
        content=memories,
        content_rowid=id
    )''')
    # Create triggers to keep FTS index in sync
    c.execute('''CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
        INSERT INTO memories_fts(rowid, content, tags, source)
        VALUES (new.id, new.content, new.tags, new.source);
    END''')
    c.execute('''CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
        INSERT INTO memories_fts(memories_fts, rowid, content, tags, source)
        VALUES ('delete', old.id, old.content, old.tags, old.source);
    END''')
    c.execute('''CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
        INSERT INTO memories_fts(memories_fts, rowid, content, tags, source)
        VALUES ('delete', old.id, old.content, old.tags, old.source);
        INSERT INTO memories_fts(rowid, content, tags, source)
        VALUES (new.id, new.content, new.tags, new.source);
    END''')

conn.commit()
conn.close()
print('  ✓ Memory database initialized')
"

# Create today's landing from template if landings dir is empty
if [ -z "$(ls -A "$HEX_DIR/landings/" 2>/dev/null | grep -v TEMPLATE.md)" ]; then
    TODAY=$(date +%Y-%m-%d)
    if [ -f "$HEX_DIR/landings/TEMPLATE.md" ] && [ ! -f "$HEX_DIR/landings/$TODAY.md" ]; then
        cp "$HEX_DIR/landings/TEMPLATE.md" "$HEX_DIR/landings/$TODAY.md"
        echo "  ✓ Created today's landing: .hex/landings/$TODAY.md"
    fi
fi

echo "  ✓ Directory structure ready"
echo ""
echo "hex is ready. Your agent now has persistent memory."
echo ""
echo "  Search:  python3 .hex/memory/search.py 'query'"
echo "  Save:    python3 .hex/memory/save.py 'content' --tags 'tag1,tag2'"
echo "  Index:   python3 .hex/memory/index.py"
