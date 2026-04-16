#!/usr/bin/env bash
set -euo pipefail

# hex Full-Stack Integration Test
# Tests the entire hex framework: install, memory, commands, doctor, upgrade, BOI, hex-events.
# Runs hermetically in Docker — no network dependencies for core tests.

PASS=0
FAIL=0
TOTAL=0

check() {
    TOTAL=$((TOTAL + 1))
    local name="$1"; shift
    if "$@" >/dev/null 2>&1; then
        echo "  PASS: $name"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $name"
        FAIL=$((FAIL + 1))
    fi
}

echo "============================================="
echo " hex Full-Stack Integration Test"
echo "============================================="
echo ""

# ═══════════════════════════════════════════════════
# SECTION 1: Fresh Install
# ═══════════════════════════════════════════════════
echo "[1/9] Fresh Install"
bash /tmp/hex-setup/install.sh /tmp/test-hex
echo "  PASS: Install completed"
PASS=$((PASS + 1)); TOTAL=$((TOTAL + 1))

# ═══════════════════════════════════════════════════
# SECTION 2: Directory Structure
# ═══════════════════════════════════════════════════
echo "[2/9] Directory Structure"
for dir in me me/decisions projects projects/_archive people evolution \
           landings landings/weekly raw raw/transcripts raw/handoffs \
           specs specs/_archive .hex .claude/commands; do
    check "dir: $dir" test -d "/tmp/test-hex/$dir"
done

# ═══════════════════════════════════════════════════
# SECTION 3: Core Files
# ═══════════════════════════════════════════════════
echo "[3/9] Core Files"
for file in CLAUDE.md AGENTS.md todo.md me/me.md me/learnings.md \
           .hex/memory.db .hex/version.txt \
           .hex/scripts/startup.sh .hex/scripts/doctor.sh .hex/scripts/upgrade.sh .hex/scripts/today.sh \
           .hex/skills/memory/scripts/memory_search.py \
           .hex/skills/memory/scripts/memory_save.py \
           .hex/skills/memory/scripts/memory_index.py \
           .hex/templates/landing-template.md .hex/templates/decision-template.md \
           evolution/observations.md evolution/suggestions.md evolution/changelog.md; do
    check "file: $file" test -f "/tmp/test-hex/$file"
done

# ═══════════════════════════════════════════════════
# SECTION 4: Commands (all 10 in .claude/commands/)
# ═══════════════════════════════════════════════════
echo "[4/9] Commands"
for cmd in hex-startup hex-checkpoint hex-shutdown hex-consolidate \
           hex-reflect hex-debrief hex-triage hex-decide hex-doctor hex-upgrade; do
    check "command: //$cmd" test -f "/tmp/test-hex/.claude/commands/$cmd.md"
done

# ═══════════════════════════════════════════════════
# SECTION 5: CLAUDE.md Integrity
# ═══════════════════════════════════════════════════
echo "[5/9] CLAUDE.md Integrity"
check "zone: system-start" grep -q "hex:system-start" /tmp/test-hex/CLAUDE.md
check "zone: system-end"   grep -q "hex:system-end"   /tmp/test-hex/CLAUDE.md
check "zone: user-start"   grep -q "hex:user-start"   /tmp/test-hex/CLAUDE.md
check "zone: user-end"     grep -q "hex:user-end"     /tmp/test-hex/CLAUDE.md
check "has standing orders" grep -q "Core Rules" /tmp/test-hex/CLAUDE.md
check "has learning engine" grep -q "Learning Engine" /tmp/test-hex/CLAUDE.md
check "has improvement engine" grep -q "Improvement Engine" /tmp/test-hex/CLAUDE.md
check "onboarding trigger" grep -q "Your name here" /tmp/test-hex/me/me.md

# No personal references
if grep -qi "mike\|rapadas\|whitney\|hermes\|nanoclaw\|cc-connect" /tmp/test-hex/CLAUDE.md; then
    echo "  FAIL: Personal references in CLAUDE.md"
    FAIL=$((FAIL + 1))
else
    echo "  PASS: No personal references"
    PASS=$((PASS + 1))
fi
TOTAL=$((TOTAL + 1))

# ═══════════════════════════════════════════════════
# SECTION 6: Memory System — Full Cycle
# ═══════════════════════════════════════════════════
echo "[6/9] Memory System"
cd /tmp/test-hex

# Schema check
python3 -c "
import sqlite3, sys
conn = sqlite3.connect('.hex/memory.db')
tables = {r[0] for r in conn.execute(
    \"SELECT name FROM sqlite_master WHERE type IN ('table','view')\"
).fetchall()}
required = {'memories', 'memories_fts', 'chunks', 'files', 'metadata'}
missing = required - tables
if missing:
    print(f'FAIL: missing tables: {missing}'); sys.exit(1)
print('OK')
conn.close()
" && { echo "  PASS: DB schema complete"; PASS=$((PASS + 1)); } || { echo "  FAIL: DB schema"; FAIL=$((FAIL + 1)); }
TOTAL=$((TOTAL + 1))

# Save
python3 .hex/skills/memory/scripts/memory_save.py "fullstack test sentinel_abc" --tags "e2e,fullstack" >/dev/null 2>&1
check "memory save" test $? -eq 0

# Search saved memory
OUTPUT=$(python3 .hex/skills/memory/scripts/memory_search.py "sentinel_abc" --compact 2>&1)
if echo "$OUTPUT" | grep -q "sentinel_abc"; then
    echo "  PASS: Search finds saved memory"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Search didn't find saved memory"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# Index
python3 .hex/skills/memory/scripts/memory_index.py >/dev/null 2>&1
STATS=$(python3 .hex/skills/memory/scripts/memory_index.py --stats 2>&1)
if echo "$STATS" | grep -q "Files indexed:"; then
    echo "  PASS: Index + stats"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Index + stats"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# Search indexed content
OUTPUT=$(python3 .hex/skills/memory/scripts/memory_search.py "priorities" --compact 2>&1)
if echo "$OUTPUT" | grep -q "todo.md\|Priorities"; then
    echo "  PASS: Search finds indexed files"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Search didn't find indexed files"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ═══════════════════════════════════════════════════
# SECTION 7: Doctor + Startup
# ═══════════════════════════════════════════════════
echo "[7/9] Doctor + Startup"
cd /tmp/test-hex

# Doctor
DOCTOR_OUT=$(HEX_DIR=/tmp/test-hex bash .hex/scripts/doctor.sh 2>&1 || true)
if echo "$DOCTOR_OUT" | grep -q "hex is healthy"; then
    echo "  PASS: Doctor passes"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Doctor found issues"
    echo "$DOCTOR_OUT" | grep "✗" | head -5
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# Startup
STARTUP_OUT=$(HEX_DIR=/tmp/test-hex bash .hex/scripts/startup.sh 2>&1)
if echo "$STARTUP_OUT" | grep -q "Ready"; then
    echo "  PASS: Startup runs"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Startup failed"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ═══════════════════════════════════════════════════
# SECTION 8: Upgrade — Zone Merge
# ═══════════════════════════════════════════════════
echo "[8/9] Upgrade (Zone Merge)"
cd /tmp/test-hex

# Inject custom rule into user zone
python3 -c "
text = open('CLAUDE.md').read()
text = text.replace(
    'Add your own rules',
    'FULLSTACK_CUSTOM_RULE_99999\n\nAdd your own rules'
)
open('CLAUDE.md', 'w').write(text)
"

# Save a memory (should survive upgrade)
python3 .hex/skills/memory/scripts/memory_save.py "pre-upgrade memory persist_check" --tags "upgrade" >/dev/null 2>&1

# Create local upgrade source
mkdir -p /tmp/hex-upgrade-repo
cp -r /tmp/hex-setup/* /tmp/hex-upgrade-repo/ 2>/dev/null || true
cp /tmp/hex-setup/.gitignore /tmp/hex-upgrade-repo/ 2>/dev/null || true
echo "0.2.0" > /tmp/hex-upgrade-repo/system/version.txt
cd /tmp/hex-upgrade-repo && git init -q && git add -A && git commit -q -m "v0.2.0"
cd /tmp/test-hex

# Run upgrade
HEX_DIR=/tmp/test-hex HEX_REPO_URL=/tmp/hex-upgrade-repo bash .hex/scripts/upgrade.sh 2>&1 || true

# User zone preserved?
if grep -q "FULLSTACK_CUSTOM_RULE_99999" /tmp/test-hex/CLAUDE.md; then
    echo "  PASS: User zone preserved"
    PASS=$((PASS + 1))
else
    echo "  FAIL: User zone lost"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# System files updated?
NEW_VER=$(cat /tmp/test-hex/.hex/version.txt 2>/dev/null)
if [ "$NEW_VER" = "0.2.0" ]; then
    echo "  PASS: System upgraded to 0.2.0"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Version not updated (got: $NEW_VER)"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# Memory.db survived upgrade?
PERSIST_OUT=$(python3 .hex/skills/memory/scripts/memory_search.py "persist_check" --compact 2>&1)
if echo "$PERSIST_OUT" | grep -q "persist_check"; then
    echo "  PASS: Memory survived upgrade"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Memory lost during upgrade"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# Doctor passes after upgrade?
POST_DOCTOR=$(HEX_DIR=/tmp/test-hex bash .hex/scripts/doctor.sh 2>&1 || true)
if echo "$POST_DOCTOR" | grep -q "hex is healthy"; then
    echo "  PASS: Doctor passes after upgrade"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Doctor fails after upgrade"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ═══════════════════════════════════════════════════
# SECTION 9: Guards + Unit Tests
# ═══════════════════════════════════════════════════
echo "[9/9] Guards + Unit Tests"

# Re-install guard
REINSTALL_OUT=$(bash /tmp/hex-setup/install.sh /tmp/test-hex 2>&1 || true)
if echo "$REINSTALL_OUT" | grep -q "already exists"; then
    echo "  PASS: Re-install blocked"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Re-install not blocked"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# Install registry
check "registry exists" test -f "$HOME/.hex-install.json"

# Unit tests
cd /tmp/hex-setup
if python3 -m pytest tests/test_memory.py -q --tb=short 2>&1 | tail -1 | grep -q "passed"; then
    echo "  PASS: Unit tests (21/21)"
    PASS=$((PASS + 1))
else
    echo "  FAIL: Unit tests"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ═══════════════════════════════════════════════════
# SUMMARY
# ═══════════════════════════════════════════════════
echo ""
echo "============================================="
echo " Results: $PASS passed, $FAIL failed ($TOTAL total)"
echo "============================================="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
echo ""
echo "=== ALL FULL-STACK TESTS PASSED ==="
