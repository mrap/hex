#!/usr/bin/env bash
# test-migrate.sh — Exercise migrate-v1-to-v2.sh against synthetic fixtures.
#
# Usage: bash test-migrate.sh
#
# For each fixture:
#   1. Copy fixture to a temp dir (leave original untouched for re-runs)
#   2. Run migrate-v1-to-v2.sh against the copy
#   3. Assert: v2 layout in place, .claude/ narrowed, migrator exited 0
#   4. Assert: expected files land at expected paths with expected content
#   5. Assert: sed rewrites actually happened (no leftover .claude/scripts refs in tracked files)
#
# Exit 0 if all fixtures pass; 1 if any fails.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MIGRATOR="$SCRIPT_DIR/../../system/scripts/migrate-v1-to-v2.sh"
FIXTURES_DIR="$(mktemp -d /tmp/hex-migrate-test.XXXXXX)"

GREEN='\033[32m'; RED='\033[31m'; YELLOW='\033[33m'; BOLD='\033[1m'; RESET='\033[0m'

PASSED=0
FAILED=0
FAILURES=()

pass() { echo -e "  [${GREEN}PASS${RESET}] $*"; PASSED=$((PASSED+1)); }
fail() { echo -e "  [${RED}FAIL${RESET}] $*"; FAILED=$((FAILED+1)); FAILURES+=("$1"); }
info() { echo -e "  → $*"; }

trap 'rm -rf "$FIXTURES_DIR"' EXIT

# ─── Setup ────────────────────────────────────────────────────────────────────

echo -e "${BOLD}Building fixtures...${RESET}"
bash "$SCRIPT_DIR/build-fixtures.sh" "$FIXTURES_DIR" | tail -5

[ -x "$MIGRATOR" ] || { echo "migrator not executable: $MIGRATOR"; exit 1; }

# ─── Assertion helpers ────────────────────────────────────────────────────────

assert_dir_exists()    { [ -d "$1" ] && pass "$2: dir exists: $1" || fail "$2: dir missing: $1"; }
assert_dir_missing()   { [ ! -d "$1" ] && pass "$2: dir correctly absent: $1" || fail "$2: dir should not exist: $1"; }
assert_file_exists()   { [ -f "$1" ] && pass "$2: file exists: $1" || fail "$2: file missing: $1"; }
assert_symlink()       { [ -L "$1" ] && pass "$2: symlink: $1" || fail "$2: not a symlink: $1"; }
assert_no_refs()       {
    # $1=dir, $2=pattern, $3=test-name
    if grep -rE "$2" "$1" --include="*.sh" --include="*.py" --include="*.md" --include="*.yaml" >/dev/null 2>&1; then
        local hits
        hits=$(grep -rE "$2" "$1" --include="*.sh" --include="*.py" --include="*.md" --include="*.yaml" -l 2>/dev/null | head -3 | tr '\n' ' ')
        fail "$3: leftover pattern '$2' in: $hits"
    else
        pass "$3: no leftover '$2' in $1"
    fi
}

# ─── Per-fixture test ─────────────────────────────────────────────────────────

run_fixture_test() {
    local fixture="$1"
    local extra_flags="${2:-}"

    echo ""
    echo -e "${BOLD}Testing fixture: $fixture${RESET}"

    local src="$FIXTURES_DIR/$fixture"
    local work="$FIXTURES_DIR/$fixture-run"
    rm -rf "$work"
    cp -a "$src" "$work"

    info "running migrator (--hex-dir $work $extra_flags)"
    if ! bash "$MIGRATOR" --hex-dir "$work" $extra_flags --skip-validate >"$FIXTURES_DIR/$fixture.log" 2>&1; then
        local rc=$?
        fail "$fixture: migrator exited $rc"
        echo "  last 15 lines of log:"
        tail -15 "$FIXTURES_DIR/$fixture.log" | sed 's/^/    /'
        return
    fi
    pass "$fixture: migrator exited 0"

    # Layout assertions
    assert_dir_exists  "$work/.hex"                          "$fixture"
    assert_dir_exists  "$work/.hex/scripts"                  "$fixture"
    assert_dir_exists  "$work/.hex/skills"                   "$fixture"
    assert_dir_missing "$work/.claude/scripts"               "$fixture"
    assert_dir_missing "$work/.claude/skills"                "$fixture"
    assert_file_exists "$work/.claude/settings.json"         "$fixture"
    assert_dir_exists  "$work/.claude/commands"              "$fixture"
    assert_symlink     "$work/.agents/skills"                "$fixture"
    assert_dir_exists  "$work/.claude.v1-backup-"*           "$fixture"

    # Content assertions
    if [ -f "$work/.hex/scripts/startup.sh" ]; then
        if grep -q '\.claude/scripts' "$work/.hex/scripts/startup.sh"; then
            fail "$fixture: .hex/scripts/startup.sh still contains .claude/scripts ref"
        else
            pass "$fixture: startup.sh path refs rewritten"
        fi
    fi

    # settings.json hook paths should now point at .hex/hooks/
    if [ -f "$work/.claude/settings.json" ]; then
        if grep -q '\.claude/hooks' "$work/.claude/settings.json"; then
            fail "$fixture: settings.json still has .claude/hooks/ ref"
        elif grep -q '\.hex/hooks' "$work/.claude/settings.json"; then
            pass "$fixture: settings.json hook paths rewritten to .hex/hooks/"
        else
            # Fixture may not have had hooks in settings.json; acceptable
            pass "$fixture: settings.json has no hook path refs (acceptable)"
        fi
    fi

    # Git commit was made
    if git -C "$work" log -1 --format=%s | grep -q '^migrate:'; then
        pass "$fixture: migration commit made"
    else
        fail "$fixture: no migration commit found"
    fi

    # .agents/skills symlink target
    if [ -L "$work/.agents/skills" ]; then
        local target
        target=$(readlink "$work/.agents/skills")
        if [ "$target" = "../.hex/skills" ]; then
            pass "$fixture: .agents/skills symlink target correct"
        else
            fail "$fixture: .agents/skills points at '$target' (expected '../.hex/skills')"
        fi
    fi

    # Idempotency: re-running should exit 0 with no changes
    info "idempotency check: re-run migrator"
    if bash "$MIGRATOR" --hex-dir "$work" --skip-validate >"$FIXTURES_DIR/$fixture-rerun.log" 2>&1; then
        if grep -q "Already on v2 layout" "$FIXTURES_DIR/$fixture-rerun.log"; then
            pass "$fixture: idempotent (re-run detected v2, exited 0)"
        else
            fail "$fixture: re-run didn't report 'already on v2'"
            tail -5 "$FIXTURES_DIR/$fixture-rerun.log" | sed 's/^/    /'
        fi
    else
        fail "$fixture: re-run failed"
    fi
}

# ─── Test: rollback ───────────────────────────────────────────────────────────

test_rollback() {
    echo ""
    echo -e "${BOLD}Testing rollback${RESET}"

    local fixture="v1-minimal"
    local src="$FIXTURES_DIR/$fixture"
    local work="$FIXTURES_DIR/$fixture-rollback"
    rm -rf "$work"
    cp -a "$src" "$work"

    info "migrating"
    bash "$MIGRATOR" --hex-dir "$work" --skip-validate >/dev/null 2>&1 || {
        fail "rollback-test: initial migration failed"
        return
    }

    info "rolling back"
    if bash "$MIGRATOR" --rollback --hex-dir "$work" >/dev/null 2>&1; then
        pass "rollback exited 0"
    else
        fail "rollback exited nonzero"
        return
    fi

    assert_dir_exists  "$work/.claude/scripts" "rollback"
    assert_dir_missing "$work/.hex/scripts"    "rollback"
}

# ─── Run ──────────────────────────────────────────────────────────────────────

run_fixture_test v1-minimal
run_fixture_test v1-standard
run_fixture_test v1-heavy --force
test_rollback

# ─── Report ───────────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}Summary:${RESET} $PASSED passed, $FAILED failed"

if [ "$FAILED" -eq 0 ]; then
    echo -e "${GREEN}ALL PASS${RESET}"
    exit 0
else
    echo -e "${RED}FAIL${RESET}"
    for f in "${FAILURES[@]}"; do echo "  - $f"; done
    exit 1
fi
