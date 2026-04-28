#!/usr/bin/env bash
# helpers.sh — Shared test helpers for the core E2E suite.
# Source this file before running any suite. Do not execute directly.
set -uo pipefail

# ── Counters ─────────────────────────────────────────────────────────────────
PASS=0
FAIL=0

# ── Color helpers ─────────────────────────────────────────────────────────────
red()   { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
yellow(){ printf '\033[33m%s\033[0m\n' "$*"; }

# ── Core assert functions ─────────────────────────────────────────────────────

assert_pass() {
    PASS=$((PASS + 1))
    green "  ✓ $*"
}

assert_fail() {
    FAIL=$((FAIL + 1))
    red "  ✗ $*"
}

# assert_exit <expected_code> <actual_code> <description>
assert_exit() {
    local expected=$1 actual=$2 desc="$3"
    if [ "$actual" -eq "$expected" ]; then
        assert_pass "$desc (exit $actual)"
    else
        assert_fail "$desc (expected exit $expected, got exit $actual)"
    fi
}

# assert_contains <output> <pattern> <description>
assert_contains() {
    local output="$1" pattern="$2" desc="$3"
    if echo "$output" | grep -q "$pattern"; then
        assert_pass "$desc"
    else
        assert_fail "$desc — expected '$pattern' in output; got: $(echo "$output" | head -3)"
    fi
}

# assert_not_contains <output> <pattern> <description>
assert_not_contains() {
    local output="$1" pattern="$2" desc="$3"
    if echo "$output" | grep -q "$pattern"; then
        assert_fail "$desc — found '$pattern' in output (should be absent)"
    else
        assert_pass "$desc"
    fi
}

# assert_file_exists <path> <description>
assert_file_exists() {
    local path="$1" desc="$2"
    if [ -f "$path" ]; then
        assert_pass "$desc"
    else
        assert_fail "$desc — file not found: $path"
    fi
}

# assert_dir_exists <path> <description>
assert_dir_exists() {
    local path="$1" desc="$2"
    if [ -d "$path" ]; then
        assert_pass "$desc"
    else
        assert_fail "$desc — directory not found: $path"
    fi
}

# ── Suite summary ─────────────────────────────────────────────────────────────

# print_suite_summary <suite_name> <pass_before> <fail_before>
# Prints delta for the suite just finished. Call with the counters snapshotted
# before the suite started.
print_suite_summary() {
    local suite="$1"
    local pass_before="${2:-0}"
    local fail_before="${3:-0}"
    local suite_pass=$((PASS - pass_before))
    local suite_fail=$((FAIL - fail_before))
    local suite_total=$((suite_pass + suite_fail))
    echo ""
    if [ "$suite_fail" -eq 0 ]; then
        green "  → $suite: $suite_pass/$suite_total passed"
    else
        red   "  → $suite: $suite_pass/$suite_total passed ($suite_fail FAILED)"
    fi
}
