#!/usr/bin/env bats

setup() {
    SCRIPT_DIR="${BATS_TEST_DIRNAME}/../system/scripts"
    FAKE_REPO_DIR=$(mktemp -d)
    FAKE_AGENT_DIR=$(mktemp -d)
    mkdir -p "$FAKE_AGENT_DIR/.hex"
    # Minimal upgrade.json so upgrade.sh has something to parse
    echo '{"repo": "https://example.invalid/none.git", "last_upgrade": "2026-01-01"}' \
        > "$FAKE_AGENT_DIR/.hex/upgrade.json"
}

teardown() {
    rm -rf "$FAKE_REPO_DIR" "$FAKE_AGENT_DIR"
}

@test "upgrade.sh --dry-run accepts a v1 (dot-claude) source tree" {
    mkdir -p "$FAKE_REPO_DIR/dot-claude/scripts"
    mkdir -p "$FAKE_REPO_DIR/dot-claude/skills"
    mkdir -p "$FAKE_REPO_DIR/dot-claude/commands"
    echo "echo hi" > "$FAKE_REPO_DIR/dot-claude/scripts/hello.sh"
    echo "# CLAUDE.md stub" > "$FAKE_REPO_DIR/CLAUDE.md"
    run env HEX_DIR="$FAKE_AGENT_DIR" bash "$SCRIPT_DIR/upgrade.sh" --dry-run --local "$FAKE_REPO_DIR"
    [ "$status" -eq 0 ]
    [[ "$output" == *"Source layout: v1"* ]]
}

@test "upgrade.sh --dry-run accepts a v2 (system/templates) source tree" {
    mkdir -p "$FAKE_REPO_DIR/system/scripts"
    mkdir -p "$FAKE_REPO_DIR/system/skills"
    mkdir -p "$FAKE_REPO_DIR/system/commands"
    mkdir -p "$FAKE_REPO_DIR/templates"
    echo "echo hi" > "$FAKE_REPO_DIR/system/scripts/hello.sh"
    echo "# CLAUDE.md stub" > "$FAKE_REPO_DIR/templates/CLAUDE.md"
    run env HEX_DIR="$FAKE_AGENT_DIR" bash "$SCRIPT_DIR/upgrade.sh" --dry-run --local "$FAKE_REPO_DIR"
    [ "$status" -eq 0 ]
    [[ "$output" == *"Source layout: v2"* ]]
}

@test "upgrade.sh --dry-run rejects unknown layout" {
    mkdir -p "$FAKE_REPO_DIR/random-content"
    run env HEX_DIR="$FAKE_AGENT_DIR" bash "$SCRIPT_DIR/upgrade.sh" --dry-run --local "$FAKE_REPO_DIR"
    [ "$status" -ne 0 ]
    [[ "$output" == *"No recognized hex layout"* ]]
}
