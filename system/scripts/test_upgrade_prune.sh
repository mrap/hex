#!/bin/bash
# test_upgrade_prune.sh — Integration tests for upgrade.sh prune pass
#
# Builds a fake AGENT_DIR fixture in a tmpdir, exercises --prune and
# --prune-apply, checks dry-run output, apply mutations, idempotency,
# and malformed-JSON robustness.

set -uo pipefail

SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PASS_COUNT=0
FAIL_COUNT=0

pass() {
    PASS_COUNT=$((PASS_COUNT + 1))
    echo "PASS: $1"
}

fail() {
    FAIL_COUNT=$((FAIL_COUNT + 1))
    echo "FAIL: $1"
}

# ── Fixture setup ────────────────────────────────────────────────────────────

WORK_DIR=""
FAKE_HEX=""
FAKE_HOME=""
FAKE_SRC=""

setup_fixture() {
    WORK_DIR=$(mktemp -d)
    FAKE_HEX="$WORK_DIR/hex"
    FAKE_HOME="$WORK_DIR/home"
    FAKE_SRC="$WORK_DIR/source"

    # Fake HEX_DIR structure (upgrade.sh derives HEX_DIR from its own location)
    mkdir -p "$FAKE_HEX/.hex/scripts"
    mkdir -p "$FAKE_HEX/.hex/hooks"
    mkdir -p "$FAKE_HEX/.hex/_archive"
    mkdir -p "$FAKE_HEX/.claude"

    # Fake HOME — keeps all side effects inside tmpdir
    mkdir -p "$FAKE_HOME/.hex-events/policies"
    mkdir -p "$FAKE_HOME/.hex-events/venv/bin"
    mkdir -p "$FAKE_HOME/.boi"

    # hex_eventd.py sentinel → install_hex_events takes "no git repo" path
    touch "$FAKE_HOME/.hex-events/hex_eventd.py"

    # Fake hex-events binary so verify_hex_events succeeds without COMPONENT_WARNINGS
    cat > "$FAKE_HOME/.hex-events/venv/bin/hex-events" << 'HEXEOF'
#!/bin/bash
exit 0
HEXEOF
    chmod +x "$FAKE_HOME/.hex-events/venv/bin/hex-events"

    # BOI config sentinel → install_boi takes "no installer found" path (verify_boi
    # returns immediately because .boi/src/boi.sh doesn't exist in FAKE_HOME)
    echo '{}' > "$FAKE_HOME/.boi/config.json"

    # Minimal v2 hex-foundation source layout (empty dirs are fine — rsync is a no-op)
    mkdir -p "$FAKE_SRC/system/scripts"
    mkdir -p "$FAKE_SRC/system/skills"
    mkdir -p "$FAKE_SRC/system/commands"
    mkdir -p "$FAKE_SRC/system/hooks"
    mkdir -p "$FAKE_SRC/templates"
    touch "$FAKE_SRC/templates/CLAUDE.md"

    # Copy upgrade.sh + all prune helpers into fake HEX_DIR so SCRIPT_DIR resolves
    # to the fake location and HEX_DIR = FAKE_HEX
    cp "$SCRIPTS_DIR/upgrade.sh"      "$FAKE_HEX/.hex/scripts/"
    cp "$SCRIPTS_DIR/path-mapping.sh" "$FAKE_HEX/.hex/scripts/"
    for f in "$SCRIPTS_DIR"/_prune_*.py; do
        cp "$f" "$FAKE_HEX/.hex/scripts/"
    done

    # Create a script that EXISTS — hooks pointing here are "valid"
    cat > "$FAKE_HEX/.hex/scripts/valid_script.sh" << 'VALIDEOF'
#!/bin/bash
echo "valid"
VALIDEOF
    chmod +x "$FAKE_HEX/.hex/scripts/valid_script.sh"
    # ghost_script.sh intentionally does NOT exist — hooks pointing here are "stale"

    # settings.json: one valid hook + one stale hook
    cat > "$FAKE_HEX/.claude/settings.json" << EOF
{
  "hooks": {
    "PreToolUse": [
      {
        "hooks": [
          {"type": "command", "command": "$FAKE_HEX/.hex/scripts/valid_script.sh"},
          {"type": "command", "command": "$FAKE_HEX/.hex/scripts/ghost_script.sh"}
        ]
      }
    ]
  }
}
EOF

    # required-hooks.json: one valid entry (relative script) + one stale entry
    cat > "$FAKE_HEX/.hex/hooks/required-hooks.json" << 'EOF'
{
  "PostToolUse": [
    {"id": "valid-hook",  "script": ".hex/scripts/valid_script.sh"},
    {"id": "stale-hook",  "script": ".hex/scripts/ghost_script.sh"}
  ]
}
EOF

    # hex-events policy: one valid action + one stale action
    cat > "$FAKE_HOME/.hex-events/policies/test_policy.yaml" << EOF
rules:
  - name: valid-rule
    actions:
      - type: shell
        command: $FAKE_HEX/.hex/scripts/valid_script.sh
  - name: stale-rule
    actions:
      - type: shell
        command: $FAKE_HEX/.hex/scripts/ghost_script.sh
EOF
}

cleanup_fixture() {
    if [[ -n "${WORK_DIR:-}" && -d "${WORK_DIR:-}" ]]; then
        rm -rf "$WORK_DIR"
    fi
}

run_upgrade() {
    # Run the fake upgrade.sh (SCRIPT_DIR = FAKE_HEX/.hex/scripts → HEX_DIR = FAKE_HEX)
    # with a completely fake HOME so no real files are touched.
    local extra_args=("$@")
    HOME="$FAKE_HOME" bash "$FAKE_HEX/.hex/scripts/upgrade.sh" \
        --local "$FAKE_SRC" \
        "${extra_args[@]}" 2>&1
}

# ── Test 1: Dry-run flags stale entries, leaves files unchanged ───────────────

test_dryrun() {
    local out
    out=$(run_upgrade --dry-run --prune)

    # Stale entries should be reported
    if echo "$out" | grep -q "ghost_script.sh"; then
        pass "T1: dry-run flags stale ghost_script.sh"
    else
        fail "T1: dry-run did not flag ghost_script.sh"
    fi

    if echo "$out" | grep -qi "stale"; then
        pass "T1: dry-run prints STALE marker"
    else
        fail "T1: dry-run missing STALE marker"
    fi

    # Valid script should NOT appear in a STALE line
    local stale_lines
    stale_lines=$(echo "$out" | grep -i "STALE" || true)
    if echo "$stale_lines" | grep -q "valid_script.sh"; then
        fail "T1: valid_script.sh incorrectly flagged as stale"
    else
        pass "T1: valid_script.sh not flagged as stale"
    fi

    # Files must be unmodified (dry-run never writes)
    if grep -q "ghost_script.sh" "$FAKE_HEX/.claude/settings.json"; then
        pass "T1: settings.json unchanged after dry-run"
    else
        fail "T1: settings.json was modified during dry-run"
    fi

    if grep -q "ghost_script.sh" "$FAKE_HEX/.hex/hooks/required-hooks.json"; then
        pass "T1: hooks.json unchanged after dry-run"
    else
        fail "T1: hooks.json was modified during dry-run"
    fi
}

# ── Test 2: Apply removes stale entries and creates backup ────────────────────

test_apply() {
    local out exit_code
    # Allow non-zero exit: upgrade.sh may exit 1 if any component check warns
    out=$(run_upgrade --prune-apply 2>&1) && exit_code=0 || exit_code=$?

    # Stale entries should have been removed from settings.json
    if grep -q "ghost_script.sh" "$FAKE_HEX/.claude/settings.json"; then
        fail "T2: settings.json still contains stale ghost_script.sh after apply"
    else
        pass "T2: stale ghost_script.sh removed from settings.json"
    fi

    # Valid entry must survive
    if grep -q "valid_script.sh" "$FAKE_HEX/.claude/settings.json"; then
        pass "T2: valid_script.sh kept in settings.json"
    else
        fail "T2: valid_script.sh was incorrectly removed from settings.json"
    fi

    # Stale entry removed from hooks.json
    if grep -q "stale-hook" "$FAKE_HEX/.hex/hooks/required-hooks.json"; then
        fail "T2: hooks.json still contains stale-hook after apply"
    else
        pass "T2: stale-hook removed from hooks.json"
    fi

    # Valid hook entry kept
    if grep -q "valid-hook" "$FAKE_HEX/.hex/hooks/required-hooks.json"; then
        pass "T2: valid-hook kept in hooks.json"
    else
        fail "T2: valid-hook was incorrectly removed from hooks.json"
    fi

    # Archive dir must exist with backup copies of the mutated files
    local archive_dir
    archive_dir=$(find "$FAKE_HEX/.hex/_archive" -maxdepth 1 -type d -name "upgrade-prune-*" 2>/dev/null | head -1)
    if [[ -n "$archive_dir" ]]; then
        pass "T2: archive dir created at ${archive_dir##*/}"
    else
        fail "T2: no archive dir found under .hex/_archive/"
    fi

    if [[ -n "$archive_dir" && -f "$archive_dir/settings.json" ]]; then
        pass "T2: settings.json backup exists in archive"
    else
        fail "T2: settings.json backup missing from archive"
    fi

    if [[ -n "$archive_dir" && -f "$archive_dir/required-hooks.json" ]]; then
        pass "T2: required-hooks.json backup exists in archive"
    else
        fail "T2: required-hooks.json backup missing from archive"
    fi

    # Archive copies must contain the original stale entries (not the pruned version)
    if [[ -n "$archive_dir" && -f "$archive_dir/settings.json" ]] \
       && grep -q "ghost_script.sh" "$archive_dir/settings.json"; then
        pass "T2: archive settings.json preserves original stale hook"
    else
        fail "T2: archive settings.json does not contain original stale hook"
    fi
}

# ── Test 3: Idempotency — second prune is a no-op ────────────────────────────

test_idempotency() {
    local out
    out=$(run_upgrade --dry-run --prune)

    if echo "$out" | grep -qi "no stale entries"; then
        pass "T3: idempotency — second prune reports no stale entries"
    else
        # Accept as passing if ghost_script.sh is absent from any STALE output
        if echo "$out" | grep -qi "stale" && echo "$out" | grep -q "ghost_script.sh"; then
            fail "T3: idempotency — stale ghost_script.sh re-appears after apply"
        else
            pass "T3: idempotency — ghost_script.sh absent from stale output after apply"
        fi
    fi
}

# ── Test 4: Malformed JSON — upgrade does not crash, just warns ───────────────

test_malformed_json() {
    # Overwrite settings.json with invalid JSON
    printf '{THIS IS NOT VALID JSON}' > "$FAKE_HEX/.claude/settings.json"

    local out exit_code
    out=$(run_upgrade --dry-run --prune 2>&1) && exit_code=0 || exit_code=$?

    # Upgrade should complete without a hard crash; 139 = SIGSEGV
    if [[ "$exit_code" -ne 139 ]]; then
        pass "T4: upgrade did not crash on malformed JSON (exit $exit_code)"
    else
        fail "T4: upgrade crashed (segfault) on malformed JSON"
    fi

    # Should emit a warning about the malformed file
    if echo "$out" | grep -qi "invalid json\|malformed\|skipping prune"; then
        pass "T4: warning emitted for malformed settings.json"
    else
        fail "T4: no warning about malformed settings.json in output"
    fi
}

# ── Main ─────────────────────────────────────────────────────────────────────

setup_fixture
trap cleanup_fixture EXIT

echo "=== test_upgrade_prune.sh ==="
echo ""

echo "--- Test 1: dry-run flags stale, leaves files unchanged ---"
test_dryrun

echo ""
echo "--- Test 2: apply removes stale entries and backs up ---"
test_apply

echo ""
echo "--- Test 3: idempotency (second prune is a no-op) ---"
test_idempotency

echo ""
echo "--- Test 4: malformed JSON does not crash upgrade ---"
test_malformed_json

echo ""
echo "=== Results: $PASS_COUNT passed, $FAIL_COUNT failed ==="
echo ""

if [[ "$FAIL_COUNT" -eq 0 ]]; then
    echo "PASS"
    exit 0
else
    echo "FAIL: $FAIL_COUNT test(s) failed"
    exit 1
fi
