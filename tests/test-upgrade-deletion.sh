#!/usr/bin/env bash
# test-upgrade-deletion.sh — Fixture: verifies O5 fix (deletion pass for files
# removed from foundation). Simulates a stale file present after a v1→v2 upgrade.

set -uo pipefail

PASS=0; FAIL=0; TOTAL=0
pass() { printf "  PASS: %s\n" "$1"; PASS=$((PASS+1)); TOTAL=$((TOTAL+1)); }
fail() { printf "  FAIL: %s\n" "$1"; FAIL=$((FAIL+1)); TOTAL=$((TOTAL+1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
UPGRADE_SH="$REPO_DIR/system/scripts/upgrade.sh"
PATH_MAP_SH="$REPO_DIR/system/scripts/path-mapping.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

echo "=== Deletion Pass Tests (O5 fix) ==="
echo ""

# ── Isolated environment ──────────────────────────────────────────────────────
INSTALL="$WORK/install"
SOURCE="$WORK/foundation"
FAKE_HOME="$WORK/home"
FAKE_BIN="$WORK/bin"

# Install dir: mirrors real hex workspace (v2 layout)
mkdir -p "$INSTALL/.hex/bin" "$INSTALL/.hex/scripts" "$INSTALL/.hex/skills" \
         "$INSTALL/.hex/commands" "$INSTALL/.claude/commands"
echo "# stub" > "$INSTALL/CLAUDE.md"
echo '{"repo":"https://example.invalid/none.git"}' > "$INSTALL/.hex/upgrade.json"

# Symlink upgrade.sh + path-mapping.sh so HEX_DIR resolves to $INSTALL
ln -sf "$UPGRADE_SH"  "$INSTALL/.hex/scripts/upgrade.sh"
ln -sf "$PATH_MAP_SH" "$INSTALL/.hex/scripts/path-mapping.sh"

# Foundation source dir (v2 layout — system/ prefix)
mkdir -p "$SOURCE/system/scripts" "$SOURCE/system/skills" \
         "$SOURCE/system/commands" "$SOURCE/templates"
echo "# CLAUDE.md stub" > "$SOURCE/templates/CLAUDE.md"

# Fake HOME stubs so install side-effects are no-ops
mkdir -p "$FAKE_HOME/.boi/src"
printf '#!/bin/bash\nexit 0\n' > "$FAKE_HOME/.boi/src/boi.sh"
chmod +x "$FAKE_HOME/.boi/src/boi.sh"

HEX_EVENTS_SRC_DIR="$FAKE_HOME/github.com/mrap/hex-events"
mkdir -p "$HEX_EVENTS_SRC_DIR"
touch "$HEX_EVENTS_SRC_DIR/hex_eventd.py"
printf '#!/bin/bash\nexit 0\n' > "$HEX_EVENTS_SRC_DIR/install.sh"
chmod +x "$HEX_EVENTS_SRC_DIR/install.sh"
git -C "$HEX_EVENTS_SRC_DIR" init -q 2>/dev/null || true

mkdir -p "$FAKE_BIN"
for cmd in hex-events codesign; do
  printf '#!/bin/bash\nexit 0\n' > "$FAKE_BIN/$cmd"
  chmod +x "$FAKE_BIN/$cmd"
done
# Fake hex binary so --version doesn't fail
printf '#!/bin/bash\necho "hex 0.1.0"\n' > "$INSTALL/.hex/bin/hex"
chmod +x "$INSTALL/.hex/bin/hex"

# ── Helpers ───────────────────────────────────────────────────────────────────
run_upgrade() {
  HOME="$FAKE_HOME" \
    HEX_EVENTS_SRC="$HEX_EVENTS_SRC_DIR" \
    HEX_EVENTS_DIR="$FAKE_HOME/.hex-events" \
    PATH="$FAKE_BIN:$PATH" \
    bash "$INSTALL/.hex/scripts/upgrade.sh" --local "$SOURCE" 2>&1
}

# ── Test 1: stale script removed from foundation is pruned ───────────────────
echo "[1] stale script in .hex/scripts/ is removed"

# "v1" foundation had old-script.sh; "v2" foundation dropped it
printf '#!/bin/bash\necho legacy\n' > "$INSTALL/.hex/scripts/old-script.sh"
# Current foundation has only new-script.sh (no old-script.sh)
printf '#!/bin/bash\necho new\n' > "$SOURCE/system/scripts/new-script.sh"

out=$(run_upgrade) || true

if [ ! -f "$INSTALL/.hex/scripts/old-script.sh" ]; then
  pass "old-script.sh removed from .hex/scripts/"
else
  fail "old-script.sh still present — deletion pass did not run"
  printf '%s\n' "$out" | tail -20 >&2
fi

# Verify backup was created
backup_count=$(ls -d "$INSTALL/.hex/.upgrade-backup-"* 2>/dev/null | wc -l | tr -d ' ')
if [ "$backup_count" -gt 0 ]; then
  backup_dir=$(ls -d "$INSTALL/.hex/.upgrade-backup-"* 2>/dev/null | head -1)
  if [ -f "$backup_dir/old-script.sh" ]; then
    pass "old-script.sh backed up before deletion"
  else
    fail "backup dir exists but old-script.sh not in backup"
  fi
else
  fail "no backup directory created"
fi

# Verify the deletion was logged
if echo "$out" | grep -qE "rm.*old-script|not in foundation.*old-script"; then
  pass "deletion logged in output"
else
  fail "deletion not mentioned in upgrade output"
  printf '%s\n' "$out" | grep -iE "delet|prune|rm" >&2 || true
fi

# ── Test 2: stale command pruned from both .hex/commands and .claude/commands ─
echo "[2] stale command pruned from both command mirrors"

# Re-seed: "legacy" command in install dirs, not in foundation
printf '#!/usr/bin/env bash\necho old-cmd\n' > "$INSTALL/.hex/commands/old-cmd.md"
printf '#!/usr/bin/env bash\necho old-cmd\n' > "$INSTALL/.claude/commands/old-cmd.md"
# Foundation only has new-cmd
printf '# new cmd\n' > "$SOURCE/system/commands/new-cmd.md"

out=$(run_upgrade) || true

hd_gone=false
rc_gone=false
[ ! -f "$INSTALL/.hex/commands/old-cmd.md" ]   && hd_gone=true
[ ! -f "$INSTALL/.claude/commands/old-cmd.md" ] && rc_gone=true

$hd_gone && pass  "old-cmd.md removed from .hex/commands/"     || fail "old-cmd.md still in .hex/commands/"
$rc_gone && pass  "old-cmd.md removed from .claude/commands/"  || fail "old-cmd.md still in .claude/commands/"

# ── Test 3: current foundation files are NOT removed ─────────────────────────
echo "[3] files still in foundation are preserved"

printf '#!/bin/bash\necho keep\n' > "$SOURCE/system/scripts/keep-me.sh"

out=$(run_upgrade) || true

if [ -f "$INSTALL/.hex/scripts/keep-me.sh" ]; then
  pass "keep-me.sh preserved (exists in foundation)"
else
  fail "keep-me.sh was incorrectly deleted"
  printf '%s\n' "$out" | tail -10 >&2
fi

# ── Results ───────────────────────────────────────────────────────────────────
echo ""
echo "Results: $PASS/$TOTAL passed"
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
