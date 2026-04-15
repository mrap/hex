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
bash /tmp/hex-setup/install.sh /tmp/test-hex
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
REINSTALL_OUTPUT=$(bash /tmp/hex-setup/install.sh /tmp/test-hex 2>&1 || true)
if echo "$REINSTALL_OUTPUT" | grep -q "already exists"; then
    echo "  PASS: Re-install blocked"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Should refuse re-install"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Test 12: No personal references ───────────────────────────────
echo "[12] No personal references"
if grep -qi "mike\|rapadas\|whitney\|hermes\|nanoclaw\|cc-connect\|mrap" /tmp/test-hex/CLAUDE.md; then
    echo "  FAIL: Personal references found in CLAUDE.md"
    FAIL=$((FAIL + 1))
else
    echo "  PASS: No personal references"
    PASS=$((PASS + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Test 13: Commands installed to .claude/commands/ ───────────────
echo "[13] Commands"
for cmd in hex-startup hex-checkpoint hex-shutdown hex-consolidate hex-reflect hex-debrief hex-triage hex-decide hex-doctor hex-upgrade; do
    check "command: $cmd" test -f "/tmp/test-hex/.claude/commands/$cmd.md"
done

# ── Test 14: Doctor passes on fresh install ────────────────────────
echo "[14] Doctor"
cd /tmp/test-hex
DOCTOR_OUT=$(HEX_DIR=/tmp/test-hex bash .hex/scripts/doctor.sh 2>&1 || true)
if echo "$DOCTOR_OUT" | grep -q "hex is healthy"; then
    echo "  PASS: Doctor passes on fresh install"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Doctor found issues"
    echo "$DOCTOR_OUT" | grep "✗\|FAIL" | head -5
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Test 15: Startup script runs ──────────────────────────────────
echo "[15] Startup script"
cd /tmp/test-hex
STARTUP_OUT=$(HEX_DIR=/tmp/test-hex bash .hex/scripts/startup.sh 2>&1)
if echo "$STARTUP_OUT" | grep -q "Ready"; then
    echo "  PASS: Startup script runs"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Startup script failed"
    echo "  Output: $STARTUP_OUT"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Test 16: Upgrade — zone merge preserves user content ───────────
echo "[16] Upgrade zone merge"
cd /tmp/test-hex

# Add custom rule to user zone
python3 -c "
text = open('CLAUDE.md').read()
text = text.replace(
    'Add your own rules',
    'MY_CUSTOM_RULE_12345\n\nAdd your own rules'
)
open('CLAUDE.md', 'w').write(text)
"

# Create a local git repo to serve as the upgrade source
mkdir -p /tmp/hex-upgrade-repo
cp -r /tmp/hex-setup/* /tmp/hex-upgrade-repo/ 2>/dev/null || true
cp -r /tmp/hex-setup/.gitignore /tmp/hex-upgrade-repo/ 2>/dev/null || true
echo "0.2.0" > /tmp/hex-upgrade-repo/system/version.txt
cd /tmp/hex-upgrade-repo && git init -q && git add -A && git commit -q -m "v0.2.0"
cd /tmp/test-hex

# Run upgrade pointing to local repo
HEX_DIR=/tmp/test-hex HEX_REPO_URL=/tmp/hex-upgrade-repo bash /tmp/test-hex/.hex/scripts/upgrade.sh 2>&1 || true

# Verify user zone preserved
if grep -q "MY_CUSTOM_RULE_12345" /tmp/test-hex/CLAUDE.md; then
    echo "  PASS: User zone preserved after upgrade"
    PASS=$((PASS + 1))
else
    echo "  FAIL: User zone lost during upgrade"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# Verify system zone updated (version should be 0.2.0)
NEW_VER=$(cat /tmp/test-hex/.hex/version.txt 2>/dev/null)
if [ "$NEW_VER" = "0.2.0" ]; then
    echo "  PASS: System files updated to 0.2.0"
    PASS=$((PASS + 1))
else
    echo "  FAIL: System version not updated (got: $NEW_VER)"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Test 17: Unit tests ───────────────────────────────────────────
echo "[17] Unit tests"
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
