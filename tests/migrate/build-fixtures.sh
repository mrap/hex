#!/usr/bin/env bash
# sync-safe
# build-fixtures.sh — Generate synthetic v1 hex fixtures for migration tests.
#
# Three fixtures, each a self-contained git repo on disk:
#   v1-minimal/   — bare v1: just scripts/, skills/, commands/, settings.json, CLAUDE.md
#   v1-standard/  — adds hooks/, lib/, templates/, memory.db, secrets/
#   v1-heavy/     — adds workflows/, boi-scripts/, evolution/, pycache junk, unknown dir
#
# Used by test-migrate.sh to verify the migrator handles layout variations.
#
# Usage: bash build-fixtures.sh <output-dir>
#   Output dir will contain v1-minimal/, v1-standard/, v1-heavy/

set -euo pipefail

OUTPUT_DIR="${1:-./fixtures}"
OUTPUT_DIR="$(cd "$(dirname "$OUTPUT_DIR")" && pwd)/$(basename "$OUTPUT_DIR")"

mkdir -p "$OUTPUT_DIR"
echo "[fixtures] output: $OUTPUT_DIR"

# ─── Shared helpers ───────────────────────────────────────────────────────────

make_git_repo() {
    local dir="$1"
    cd "$dir"
    git init -q
    git config user.email "fixture@hex.local"
    git config user.name "Fixture"
    git add -A
    git commit -q -m "initial fixture" || true
    cd - >/dev/null
}

write_file() {
    local path="$1"
    shift
    mkdir -p "$(dirname "$path")"
    printf '%s\n' "$@" > "$path"
}

make_settings_json() {
    cat > "$1" <<'JSON'
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "bash \"$CLAUDE_PROJECT_DIR/.claude/hooks/scripts/backup_session.sh\""
          }
        ]
      }
    ]
  }
}
JSON
}

make_claude_md() {
    cat > "$1" <<'MD'
# Fixture hex instance

This is a synthetic fixture. It references `.claude/scripts/`, `.claude/skills/`,
and `$AGENT_DIR/.claude/hooks/scripts/` in prose and in code blocks.

See `.claude/scripts/startup.sh` for the startup script.
See `.claude/skills/memory/SKILL.md` for the memory skill.
MD
}

# ─── v1-minimal ───────────────────────────────────────────────────────────────

build_minimal() {
    local root="$OUTPUT_DIR/v1-minimal"
    rm -rf "$root"
    mkdir -p "$root"/.claude/{scripts,skills/memory,commands}

    make_claude_md "$root/CLAUDE.md"
    make_settings_json "$root/.claude/settings.json"

    # Fake scripts with path refs
    write_file "$root/.claude/scripts/startup.sh" \
        '#!/bin/bash' \
        '# Fake startup for v1 minimal fixture' \
        'echo "hex v1 startup at $AGENT_DIR/.claude/scripts/"' \
        'if [ -f "$AGENT_DIR/.claude/memory.db" ]; then' \
        '    echo "memory present"' \
        'fi'
    chmod +x "$root/.claude/scripts/startup.sh"

    write_file "$root/.claude/skills/memory/SKILL.md" \
        '---' \
        'name: memory' \
        '---' \
        '# Memory' \
        '' \
        'See `.claude/skills/memory/scripts/` for implementation.'

    write_file "$root/.claude/commands/test-command.md" \
        '# Test command' \
        '' \
        'Runs `.claude/scripts/startup.sh`.'

    write_file "$root/.gitignore" \
        '.claude/memory.db'

    make_git_repo "$root"
    echo "[fixtures] built v1-minimal at $root"
}

# ─── v1-standard ──────────────────────────────────────────────────────────────

build_standard() {
    local root="$OUTPUT_DIR/v1-standard"
    rm -rf "$root"
    mkdir -p "$root"/.claude/{scripts,skills/memory/scripts,commands,hooks/scripts,lib,templates,secrets}

    make_claude_md "$root/CLAUDE.md"
    make_settings_json "$root/.claude/settings.json"

    # Minimal subset (reuse minimal's content conceptually)
    write_file "$root/.claude/scripts/startup.sh" \
        '#!/bin/bash' \
        'echo "hex v1 standard at $AGENT_DIR/.claude/scripts/"'
    chmod +x "$root/.claude/scripts/startup.sh"

    write_file "$root/.claude/skills/memory/SKILL.md" \
        '---' \
        'name: memory' \
        '---' \
        '# Memory'

    write_file "$root/.claude/skills/memory/scripts/memory_index.py" \
        '#!/usr/bin/env python3' \
        '"""Fake memory indexer. References .claude/memory.db."""' \
        'print("indexing .claude/memory.db")'

    write_file "$root/.claude/hooks/scripts/backup_session.sh" \
        '#!/bin/bash' \
        '# Fake backup hook' \
        'echo "backup from $AGENT_DIR/.claude/hooks/scripts/"'
    chmod +x "$root/.claude/hooks/scripts/backup_session.sh"

    write_file "$root/.claude/lib/helper.sh" \
        '#!/bin/bash' \
        'hex_helper() { echo "from .claude/lib/"; }'

    write_file "$root/.claude/templates/meeting.md" \
        '# Meeting — {{topic}}'

    write_file "$root/.claude/commands/test-command.md" \
        '# Test' \
        '' \
        'Runs `.claude/scripts/startup.sh`.'

    # Fake memory.db (gitignored, runtime artifact)
    printf 'SQLITE_FAKE\n' > "$root/.claude/memory.db"

    # Secrets file
    write_file "$root/.claude/secrets/.gitkeep" ''

    write_file "$root/.gitignore" \
        '.claude/memory.db' \
        '.claude/secrets/'

    make_git_repo "$root"
    echo "[fixtures] built v1-standard at $root"
}

# ─── v1-heavy ─────────────────────────────────────────────────────────────────

build_heavy() {
    local root="$OUTPUT_DIR/v1-heavy"
    rm -rf "$root"
    mkdir -p "$root"/.claude/{scripts,skills/memory/scripts,commands,hooks/scripts,lib,templates,workflows,boi-scripts,hex-events-policies,handoffs,evolution,secrets,claudeclaw}

    make_claude_md "$root/CLAUDE.md"
    make_settings_json "$root/.claude/settings.json"

    # Reuse standard content
    write_file "$root/.claude/scripts/startup.sh" \
        '#!/bin/bash' \
        'echo "hex v1 heavy at $AGENT_DIR/.claude/scripts/"'
    chmod +x "$root/.claude/scripts/startup.sh"

    write_file "$root/.claude/skills/memory/SKILL.md" '---' 'name: memory' '---' '# Memory'

    write_file "$root/.claude/hooks/scripts/backup_session.sh" \
        '#!/bin/bash' 'echo "backup"'
    chmod +x "$root/.claude/hooks/scripts/backup_session.sh"

    write_file "$root/.claude/workflows/example.yaml" \
        'name: example' \
        'trigger: manual'

    write_file "$root/.claude/boi-scripts/hex_fleet.py" \
        '#!/usr/bin/env python3' \
        '"""Fake boi script referencing .claude/ paths."""' \
        'print("boi workflow at .claude/boi-scripts/")'

    write_file "$root/.claude/hex-events-policies/healthcheck.yaml" \
        'name: healthcheck' \
        'action:' \
        '  type: shell' \
        '  command: bash $AGENT_DIR/.claude/scripts/startup.sh'

    write_file "$root/.claude/evolution/meta-eval.py" \
        '#!/usr/bin/env python3' \
        '# Evolution script — references .claude/ paths' \
        'import os' \
        'DB = os.path.expanduser("$AGENT_DIR/.claude/memory.db")'

    write_file "$root/.claude/handoffs/.gitkeep" ''
    write_file "$root/.claude/secrets/.gitkeep" ''

    # Regeneratable junk (common in real instances)
    mkdir -p "$root/.claude/__pycache__"
    touch "$root/.claude/__pycache__/foo.pyc"
    mkdir -p "$root/.claude/skills/memory/scripts/.pytest_cache"
    touch "$root/.claude/skills/memory/scripts/.pytest_cache/README.md"
    mkdir -p "$root/.claude/skills/memory/scripts/__pycache__"
    touch "$root/.claude/skills/memory/scripts/__pycache__/memory_index.cpython-312.pyc"
    touch "$root/.claude/.update-available"
    touch "$root/.claude/.update-checked"

    # Companion/claudeclaw (stays in .claude/; unknown to migrator → needs --force)
    write_file "$root/.claude/claudeclaw/session.json" '{"id":"fake"}'

    # memory.db
    printf 'SQLITE_FAKE_HEAVY\n' > "$root/.claude/memory.db"

    # upgrade.json
    write_file "$root/.claude/upgrade.json" \
        '{"repo": "https://github.com/mrap/hex-foundation.git", "last_upgrade": "2026-01-01"}'

    # statusline.sh
    write_file "$root/.claude/statusline.sh" \
        '#!/bin/bash' 'echo "status"'
    chmod +x "$root/.claude/statusline.sh"

    # timezone
    printf 'America/New_York\n' > "$root/.claude/timezone"

    write_file "$root/.gitignore" \
        '.claude/memory.db' \
        '.claude/secrets/' \
        '.claude/__pycache__/' \
        '.claude/.pytest_cache/' \
        '.claude/.update-available' \
        '.claude/.update-checked' \
        '**/__pycache__/' \
        '**/.pytest_cache/'

    make_git_repo "$root"
    echo "[fixtures] built v1-heavy at $root"
}

build_minimal
build_standard
build_heavy

echo ""
echo "[fixtures] done. ${OUTPUT_DIR}/{v1-minimal,v1-standard,v1-heavy}"
