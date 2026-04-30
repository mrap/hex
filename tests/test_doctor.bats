#!/usr/bin/env bats
# Unit tests for doctor.sh BOI checks (check_17).
# Uses DOCTOR_SOURCE_ONLY=1 to source functions without executing the full suite.

DOCTOR_SH="${BATS_TEST_DIRNAME}/../system/scripts/doctor.sh"

# ── Helpers ──────────────────────────────────────────────────────────────────

# Write a fake boi binary that responds to --help, --version, status.
# Usage: make_fake_boi <path> <help_exit> <version_str> <status_exit>
make_fake_boi() {
  local path="$1" help_exit="${2:-0}" version_str="${3:-boi 1.0.0}" status_exit="${4:-0}"
  mkdir -p "$(dirname "$path")"
  cat > "$path" << EOF
#!/bin/bash
case "\$1" in
  --help)    printf "dispatch\\nstatus\\nbench\\ncancel\\n"; exit $help_exit ;;
  --version) echo "$version_str"; exit 0 ;;
  status)    exit $status_exit ;;
  *)         exit 0 ;;
esac
EOF
  chmod +x "$path"
}

# ── Setup / teardown ─────────────────────────────────────────────────────────

setup() {
  FAKE_HOME=$(mktemp -d)
  FAKE_HEX=$(mktemp -d)
  mkdir -p "$FAKE_HEX/.hex"
  printf 'BOI_VERSION=v1.0.0\nHARNESS_VERSION=v0.8.0\n' > "$FAKE_HEX/VERSIONS"

  # Variables doctor.sh reads at source time
  export HEX_DIR="$FAKE_HEX"
  export HOME="$FAKE_HOME"

  # Source the file in test-only mode; doctor.sh sets -uo pipefail — reset after.
  # shellcheck disable=SC1090
  DOCTOR_SOURCE_ONLY=1 source "$DOCTOR_SH"
  set +euo pipefail  # restore bats-compatible options

  # Reset counters and override output helpers to capture results silently
  PASS_COUNT=0; WARN_COUNT=0; ERROR_COUNT=0; FIXED_COUNT=0
  HAS_ERRORS=false; HAS_WARNINGS=false
  FIX=false; JSON_MODE=false; QUIET=false

  _pass()  { PASS_COUNT=$((PASS_COUNT + 1)); }
  _warn()  { WARN_COUNT=$((WARN_COUNT + 1)); HAS_WARNINGS=true; }
  _error() { ERROR_COUNT=$((ERROR_COUNT + 1)); HAS_ERRORS=true; }
  _info()  { :; }
  _fixed() { FIXED_COUNT=$((FIXED_COUNT + 1)); }
  _rec()   { :; }
}

teardown() {
  rm -rf "$FAKE_HOME" "$FAKE_HEX"
}

# ── Tests ─────────────────────────────────────────────────────────────────────

@test "check_17: BOI not installed → non-critical (no error)" {
  # No binary at ~/.boi/bin/boi
  check_17
  [ "$ERROR_COUNT" -eq 0 ]
}

@test "check_17: dangling symlink at ~/.boi/bin/boi → error" {
  mkdir -p "$FAKE_HOME/.boi/bin"
  ln -s "/nonexistent/boi_gone_$$" "$FAKE_HOME/.boi/bin/boi"
  check_17
  [ "$ERROR_COUNT" -ge 1 ]
}

@test "check_17: boi --help exits non-zero → error" {
  make_fake_boi "$FAKE_HOME/.boi/bin/boi" 1 "boi 1.0.0" 0
  check_17
  [ "$ERROR_COUNT" -ge 1 ]
}

@test "check_17: boi --version mismatches VERSIONS → error" {
  # Binary reports 0.5.0 but VERSIONS says v1.0.0
  make_fake_boi "$FAKE_HOME/.boi/bin/boi" 0 "boi 0.5.0" 0
  check_17
  [ "$ERROR_COUNT" -ge 1 ]
}

@test "check_17: boi status exits non-zero → warning not error" {
  make_fake_boi "$FAKE_HOME/.boi/bin/boi" 0 "boi 1.0.0" 1
  check_17
  [ "$WARN_COUNT" -ge 1 ]
  # Should still record passes for --help and --version
  [ "$PASS_COUNT" -ge 1 ]
}

@test "check_17: wrapper missing → warning not error" {
  make_fake_boi "$FAKE_HOME/.boi/bin/boi" 0 "boi 1.0.0" 0
  # No wrapper at ~/.boi/boi
  check_17
  [ "$WARN_COUNT" -ge 1 ]
  [ "$ERROR_COUNT" -eq 0 ]
}

@test "check_17: broken wrapper chain → error" {
  make_fake_boi "$FAKE_HOME/.boi/bin/boi" 0 "boi 1.0.0" 0
  # Wrapper that always fails
  mkdir -p "$FAKE_HOME/.boi"
  printf '#!/bin/bash\nexit 1\n' > "$FAKE_HOME/.boi/boi"
  chmod +x "$FAKE_HOME/.boi/boi"
  check_17
  [ "$ERROR_COUNT" -ge 1 ]
}

@test "check_17: all healthy → no errors or warnings" {
  make_fake_boi "$FAKE_HOME/.boi/bin/boi" 0 "boi 1.0.0" 0
  # Wrapper that delegates to the real binary
  mkdir -p "$FAKE_HOME/.boi"
  printf '#!/bin/bash\nexec "%s/.boi/bin/boi" "$@"\n' "$FAKE_HOME" > "$FAKE_HOME/.boi/boi"
  chmod +x "$FAKE_HOME/.boi/boi"
  check_17
  [ "$ERROR_COUNT" -eq 0 ]
  [ "$WARN_COUNT" -eq 0 ]
  [ "$PASS_COUNT" -ge 4 ]
}
