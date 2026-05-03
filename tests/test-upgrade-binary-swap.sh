#!/usr/bin/env bash
# test-upgrade-binary-swap.sh — Fixture: verifies O1 fix (binary rebuild + swap on version mismatch).
# Uses a fake 'cargo' binary so no real compilation occurs.

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

echo "=== Binary Swap Tests (O1 fix) ==="
echo ""

# ── Isolated environment ──────────────────────────────────────────────────────
INSTALL="$WORK/install"
SOURCE="$WORK/foundation"
FAKE_HOME="$WORK/home"
FAKE_BIN="$WORK/bin"

# Install dir: mirrors real hex workspace
mkdir -p "$INSTALL/.hex/bin" "$INSTALL/.hex/skills" "$INSTALL/.hex/scripts" "$INSTALL/.hex/commands"
echo "# stub" > "$INSTALL/CLAUDE.md"
echo '{"repo":"https://example.invalid/none.git"}' > "$INSTALL/.hex/upgrade.json"

# Symlink upgrade.sh + path-mapping.sh so HEX_DIR resolves to $INSTALL
ln -sf "$UPGRADE_SH"   "$INSTALL/.hex/scripts/upgrade.sh"
ln -sf "$PATH_MAP_SH"  "$INSTALL/.hex/scripts/path-mapping.sh"

# Foundation source dir (v2 layout)
mkdir -p "$SOURCE/system/scripts" "$SOURCE/system/skills" "$SOURCE/system/commands"
mkdir -p "$SOURCE/system/harness/src" "$SOURCE/templates"
echo "# CLAUDE.md stub" > "$SOURCE/templates/CLAUDE.md"
echo 'fn main() {}' > "$SOURCE/system/harness/src/main.rs"

# Fake HOME: pre-stub .boi and hex-events so install steps are no-ops
mkdir -p "$FAKE_HOME/.boi/src"
printf '#!/bin/bash\nexit 0\n' > "$FAKE_HOME/.boi/src/boi.sh"
chmod +x "$FAKE_HOME/.boi/src/boi.sh"

HEX_EVENTS_SRC_DIR="$FAKE_HOME/github.com/mrap/hex-events"
mkdir -p "$HEX_EVENTS_SRC_DIR"
touch "$HEX_EVENTS_SRC_DIR/hex_eventd.py"
printf '#!/bin/bash\nexit 0\n' > "$HEX_EVENTS_SRC_DIR/install.sh"
chmod +x "$HEX_EVENTS_SRC_DIR/install.sh"
git -C "$HEX_EVENTS_SRC_DIR" init -q 2>/dev/null || true

# Fake bin: hex-events and codesign stubs
mkdir -p "$FAKE_BIN"
for cmd in hex-events codesign; do
  printf '#!/bin/bash\nexit 0\n' > "$FAKE_BIN/$cmd"
  chmod +x "$FAKE_BIN/$cmd"
done

# ── Helpers ───────────────────────────────────────────────────────────────────
setup_versions() {
  local old_ver="$1" new_ver="$2"

  # Ensure a new file appears in source each run so upgrade proceeds past "nothing to do"
  rm -f "$INSTALL/.hex/scripts/binary-swap-sentinel.sh"
  printf '#!/bin/bash\n# binary-swap-test-sentinel\n' > "$SOURCE/system/scripts/binary-swap-sentinel.sh"

  # Cargo.toml in foundation source with new version
  cat > "$SOURCE/system/harness/Cargo.toml" <<EOF
[package]
name = "hex"
version = "$new_ver"
edition = "2021"
EOF

  # VERSIONS file with old version
  echo "HEX_FOUNDATION_VERSION=v${old_ver}" > "$INSTALL/VERSIONS"

  # Installed binary reporting old version
  printf '#!/bin/bash\necho "hex %s"\n' "$old_ver" > "$INSTALL/.hex/bin/hex"
  chmod +x "$INSTALL/.hex/bin/hex"

  # Fake cargo: creates a binary reporting new version (no actual compilation)
  cat > "$FAKE_BIN/cargo" <<EOF
#!/bin/bash
if [[ "\$*" == *build* ]] && [[ "\$*" == *--release* ]]; then
  mkdir -p target/release
  printf '#!/bin/bash\necho "hex ${new_ver}"\n' > target/release/hex
  chmod +x target/release/hex
fi
exit 0
EOF
  chmod +x "$FAKE_BIN/cargo"
}

run_upgrade() {
  HOME="$FAKE_HOME" \
    HEX_EVENTS_SRC="$HEX_EVENTS_SRC_DIR" \
    HEX_EVENTS_DIR="$FAKE_HOME/.hex-events" \
    PATH="$FAKE_BIN:$PATH" \
    bash "$INSTALL/.hex/scripts/upgrade.sh" --local "$SOURCE" 2>&1
}

# ── Test 1: version mismatch triggers rebuild and binary swap ─────────────────
echo "[1] binary version mismatch triggers rebuild and swap"
setup_versions "0.8.0" "0.9.0"
out=$(run_upgrade) || true

if echo "$out" | grep -q "rebuilt and swapped"; then
  got=$("$INSTALL/.hex/bin/hex" 2>/dev/null || echo "")
  if [ "$got" = "hex 0.9.0" ]; then
    pass "binary upgraded 0.8.0 → 0.9.0"
  else
    fail "binary not swapped: reports '$got'"
    printf '%s\n' "$out" | tail -20 >&2
  fi
else
  fail "'rebuilt and swapped' not found in output"
  printf '%s\n' "$out" | tail -20 >&2
fi

# ── Test 2: matching version skips rebuild ────────────────────────────────────
echo "[2] matching version — no rebuild"
setup_versions "0.9.0" "0.9.0"
out=$(run_upgrade) || true

if echo "$out" | grep -q "no rebuild needed"; then
  pass "no rebuild when version already matches"
else
  fail "'no rebuild needed' not found in output"
  printf '%s\n' "$out" | tail -20 >&2
fi

# ── Results ───────────────────────────────────────────────────────────────────
echo ""
echo "Results: $PASS/$TOTAL passed"
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
