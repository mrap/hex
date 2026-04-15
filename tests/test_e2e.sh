#!/usr/bin/env bash
set -euo pipefail

PASS=0
FAIL=0
TOTAL=0

check() {
    TOTAL=$((TOTAL + 1))
    local name="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        echo "  PASS: $name"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $name"
        FAIL=$((FAIL + 1))
    fi
}

echo "=== hex E2E Test ==="
echo ""

# ── Test 1: Install ────────────────────────────────────────────────
echo "[1] Install"
bash /tmp/hex-setup/install.sh /tmp/test-hex --no-boi --no-events
echo "  PASS: Install completed"
PASS=$((PASS + 1))
TOTAL=$((TOTAL + 1))

# ── Test 2: Directory structure ────────────────────────────────────
echo "[2] Directory structure"
for dir in me me/decisions projects people evolution landings landings/weekly raw raw/transcripts raw/handoffs specs; do
    check "dir exists: $dir" test -d "/tmp/test-hex/$dir"
done

# ── Test 3: Key files ──────────────────────────────────────────────
echo "[3] Key files"
for file in CLAUDE.md AGENTS.md todo.md me/me.md me/learnings.md .hex/memory.db .hex/version.txt; do
    check "file exists: $file" test -f "/tmp/test-hex/$file"
done

# ── Test 4: Onboarding trigger ─────────────────────────────────────
echo "[4] Onboarding trigger"
check "me.md has placeholder" grep -q "Your name here" /tmp/test-hex/me/me.md

# ── Test 5: CLAUDE.md zone markers ─────────────────────────────────
echo "[5] CLAUDE.md zone markers"
check "system-start marker" grep -q "hex:system-start" /tmp/test-hex/CLAUDE.md
check "system-end marker"   grep -q "hex:system-end"   /tmp/test-hex/CLAUDE.md
check "user-start marker"   grep -q "hex:user-start"   /tmp/test-hex/CLAUDE.md
check "user-end marker"     grep -q "hex:user-end"     /tmp/test-hex/CLAUDE.md

# ── Test 6: Memory database schema ─────────────────────────────────
echo "[6] Memory database schema"
python3 -c "
import sqlite3, sys
conn = sqlite3.connect('/tmp/test-hex/.hex/memory.db')
tables = {r[0] for r in conn.execute(
    \"SELECT name FROM sqlite_master WHERE type IN ('table','view')\"
).fetchall()}
required = {'memories', 'memories_fts', 'chunks', 'files', 'metadata'}
missing = required - tables
if missing:
    print(f'  FAIL: missing tables: {missing}')
    sys.exit(1)
print('  PASS: All required tables exist')
conn.close()
"
PASS=$((PASS + 1))
TOTAL=$((TOTAL + 1))

# ── Test 7: Memory save + search cycle ──────────────────────────────
echo "[7] Memory save + search"
cd /tmp/test-hex
python3 .hex/skills/memory/scripts/memory_save.py "test memory sentinel_xyz" --tags "e2e"
OUTPUT=$(python3 .hex/skills/memory/scripts/memory_search.py "sentinel_xyz" --compact 2>&1)
if echo "$OUTPUT" | grep -q "sentinel_xyz"; then
    echo "  PASS: Save + search round-trip works"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Search didn't find saved memory"
    echo "  Output: $OUTPUT"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Test 8: Memory index ───────────────────────────────────────────
echo "[8] Memory index"
cd /tmp/test-hex
python3 .hex/skills/memory/scripts/memory_index.py
STATS=$(python3 .hex/skills/memory/scripts/memory_index.py --stats 2>&1)
if echo "$STATS" | grep -q "Files indexed:"; then
    echo "  PASS: Index + stats works"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Unexpected stats output"
    echo "  Output: $STATS"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Test 9: Search indexed content ──────────────────────────────────
echo "[9] Search indexed content"
cd /tmp/test-hex
OUTPUT=$(python3 .hex/skills/memory/scripts/memory_search.py "priorities" --compact 2>&1)
if echo "$OUTPUT" | grep -q "todo.md\|Priorities"; then
    echo "  PASS: Search finds indexed file content"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Search didn't find indexed content"
    echo "  Output: $OUTPUT"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Test 10: Install registry ──────────────────────────────────────
echo "[10] Install registry"
check "~/.hex-install.json exists" test -f "$HOME/.hex-install.json"
python3 -c "
import json, sys
with open('$HOME/.hex-install.json') as f:
    data = json.load(f)
assert data['install_path'] == '/tmp/test-hex', f'Wrong path: {data[\"install_path\"]}'
assert 'version' in data, 'Missing version'
print('  PASS: Registry content correct')
"
PASS=$((PASS + 1))
TOTAL=$((TOTAL + 1))

# ── Test 11: Re-install guard ──────────────────────────────────────
echo "[11] Re-install guard"
if bash /tmp/hex-setup/install.sh /tmp/test-hex 2>&1 | grep -q "already exists"; then
    echo "  PASS: Re-install blocked"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Should refuse re-install"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Test 12: No personal references in CLAUDE.md ───────────────────
echo "[12] No personal references"
if grep -qi "mike\|rapadas\|whitney\|hermes\|nanoclaw\|cc-connect\|mrap" /tmp/test-hex/CLAUDE.md; then
    echo "  FAIL: Personal references found in CLAUDE.md"
    FAIL=$((FAIL + 1))
else
    echo "  PASS: No personal references"
    PASS=$((PASS + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Test 13: Unit tests ────────────────────────────────────────────
echo "[13] Unit tests"
cd /tmp/hex-setup
if python3 -m pytest tests/test_memory.py -v 2>&1; then
    echo "  PASS: All unit tests pass"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Unit tests failed"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Summary ─────────────────────────────────────────────────────────
echo ""
echo "========================================="
echo " Results: $PASS passed, $FAIL failed ($TOTAL total)"
echo "========================================="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
echo ""
echo "=== ALL TESTS PASSED ==="
