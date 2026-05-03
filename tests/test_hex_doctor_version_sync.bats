#!/usr/bin/env bats
# Fixture tests for hex-doctor check_67 (hex binary version-sync).
# Simulates a stale binary by creating a fake HEX_DIR with mismatched versions.

HEX_DOCTOR_SCRIPT="${BATS_TEST_DIRNAME}/../system/scripts/hex-doctor"

# ── Helpers ──────────────────────────────────────────────────────────────────

make_fake_hex_bin() {
  local version="$1"
  mkdir -p "$FAKE_HEX/.hex/bin"
  cat > "$FAKE_HEX/.hex/bin/hex" << SCRIPT
#!/bin/bash
case "\$1" in
  --version) echo "hex $version"; exit 0 ;;
  events)    exit 0 ;;
  *)         exit 0 ;;
esac
SCRIPT
  chmod +x "$FAKE_HEX/.hex/bin/hex"
}

make_cargo_toml() {
  local version="$1"
  mkdir -p "$FAKE_HEX/.hex/harness"
  printf '[package]\nname = "hex"\nversion = "%s"\n' "$version" \
    > "$FAKE_HEX/.hex/harness/Cargo.toml"
}

# ── Setup / teardown ─────────────────────────────────────────────────────────

setup() {
  FAKE_HEX=$(mktemp -d)
  # Create the checks dir so the module block runs (individual module files absent = skipped)
  mkdir -p "$FAKE_HEX/.hex/scripts/doctor-checks"
  mkdir -p "$FAKE_HEX/.hex/scripts"
  export HEX_DIR="$FAKE_HEX"
}

teardown() {
  unset HEX_DIR
  rm -rf "$FAKE_HEX"
}

# ── Tests ─────────────────────────────────────────────────────────────────────

@test "check_67: stale binary (binary != Cargo.toml) → error exit and both versions in output" {
  make_fake_hex_bin "0.5.0"
  make_cargo_toml "0.6.0"

  run bash "$HEX_DOCTOR_SCRIPT"

  [ "$status" -eq 1 ]
  echo "$output" | grep -q "version-sync"
  echo "$output" | grep -q "0.5.0"
  echo "$output" | grep -q "0.6.0"
}

@test "check_67: matching versions → version-sync PASS in output" {
  make_fake_hex_bin "0.6.0"
  make_cargo_toml "0.6.0"

  run bash "$HEX_DOCTOR_SCRIPT"

  echo "$output" | grep -q "version-sync: Cargo.toml == binary == 0.6.0"
}

@test "check_67: missing Cargo.toml → warn (not error) exit" {
  make_fake_hex_bin "0.6.0"
  # No Cargo.toml created

  run bash "$HEX_DOCTOR_SCRIPT"

  # Exit 2 = warnings only; exit 1 = errors; either accepted here since other checks may warn
  [ "$status" -ne 0 ]
  echo "$output" | grep -qE "version-sync.*not found|version-sync.*skipping"
}

@test "check_67: missing hex binary → error exit" {
  make_cargo_toml "0.6.0"
  # No hex binary created

  run bash "$HEX_DOCTOR_SCRIPT"

  [ "$status" -eq 1 ]
  echo "$output" | grep -q "version-sync"
}
