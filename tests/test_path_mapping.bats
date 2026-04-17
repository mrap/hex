#!/usr/bin/env bats

setup() {
    source "${BATS_TEST_DIRNAME}/../system/scripts/path-mapping.sh"
    TEST_TMPDIR=$(mktemp -d)
}

teardown() {
    rm -rf "$TEST_TMPDIR"
}

@test "v1_to_v2 maps dot-claude/scripts to system/scripts" {
    run v1_to_v2 "dot-claude/scripts/foo.sh"
    [ "$status" -eq 0 ]
    [ "$output" = "system/scripts/foo.sh" ]
}

@test "v1_to_v2 maps dot-claude/skills to system/skills" {
    run v1_to_v2 "dot-claude/skills/memory/SKILL.md"
    [ "$status" -eq 0 ]
    [ "$output" = "system/skills/memory/SKILL.md" ]
}

@test "v1_to_v2 maps dot-claude/commands to system/commands" {
    run v1_to_v2 "dot-claude/commands/hex-startup.md"
    [ "$status" -eq 0 ]
    [ "$output" = "system/commands/hex-startup.md" ]
}

@test "v1_to_v2 maps CLAUDE.md to templates/CLAUDE.md" {
    run v1_to_v2 "CLAUDE.md"
    [ "$status" -eq 0 ]
    [ "$output" = "templates/CLAUDE.md" ]
}

@test "v1_to_v2 returns empty and exit 1 for unmappable path" {
    run v1_to_v2 "dot-claude/ui/static/foo.css"
    [ "$status" -eq 1 ]
    [ -z "$output" ]
}

@test "v2_to_v1 maps system/scripts to dot-claude/scripts" {
    run v2_to_v1 "system/scripts/foo.sh"
    [ "$status" -eq 0 ]
    [ "$output" = "dot-claude/scripts/foo.sh" ]
}

@test "v2_to_v1 maps system/skills to dot-claude/skills" {
    run v2_to_v1 "system/skills/memory/SKILL.md"
    [ "$status" -eq 0 ]
    [ "$output" = "dot-claude/skills/memory/SKILL.md" ]
}

@test "v2_to_v1 maps system/commands to dot-claude/commands" {
    run v2_to_v1 "system/commands/hex-startup.md"
    [ "$status" -eq 0 ]
    [ "$output" = "dot-claude/commands/hex-startup.md" ]
}

@test "v2_to_v1 maps templates/CLAUDE.md to CLAUDE.md" {
    run v2_to_v1 "templates/CLAUDE.md"
    [ "$status" -eq 0 ]
    [ "$output" = "CLAUDE.md" ]
}

@test "v2_to_v1 returns 1 for non-syncable path" {
    run v2_to_v1 "tests/eval/run_eval.py"
    [ "$status" -eq 1 ]
    [ -z "$output" ]
}

@test "detect_layout identifies v1 from dot-claude directory" {
    local tmpdir="$TEST_TMPDIR/v1_test"
    mkdir -p "$tmpdir/dot-claude/scripts"
    run detect_layout "$tmpdir"
    [ "$status" -eq 0 ]
    [ "$output" = "v1" ]
}

@test "detect_layout identifies v2 from system and templates directories" {
    local tmpdir="$TEST_TMPDIR/v2_test"
    mkdir -p "$tmpdir/system/scripts" "$tmpdir/templates"
    touch "$tmpdir/templates/CLAUDE.md"
    run detect_layout "$tmpdir"
    [ "$status" -eq 0 ]
    [ "$output" = "v2" ]
}

@test "detect_layout returns unknown when neither layout present" {
    local tmpdir="$TEST_TMPDIR/unknown_test"
    mkdir -p "$tmpdir/random"
    run detect_layout "$tmpdir"
    [ "$status" -eq 1 ]
    [ "$output" = "unknown" ]
}

@test "detect_layout returns v1 when both layouts coexist (v1 wins)" {
    local tmpdir
    tmpdir="$TEST_TMPDIR/migration"
    mkdir -p "$tmpdir/dot-claude" "$tmpdir/system" "$tmpdir/templates"
    touch "$tmpdir/templates/CLAUDE.md"
    run detect_layout "$tmpdir"
    [ "$status" -eq 0 ]
    [ "$output" = "v1" ]
}
