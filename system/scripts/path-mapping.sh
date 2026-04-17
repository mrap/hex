#!/usr/bin/env bash
# path-mapping.sh — translate paths between v1 (dot-claude/) and v2 (system/ + templates/)
# layouts when syncing between mrap/hex (v1) and mrap/hex-foundation (v2).
#
# Functions are pure: no I/O, no globals. Safe to source from any script.

# Note: set -euo pipefail is intentionally omitted. This file is a source-only
# library; applying strict mode here would override the caller's error mode
# upon sourcing.

# Translate a v1 source-relative path to the v2 equivalent.
# Prints the v2 path to stdout; returns 0 on success, 1 if path has no v2 equivalent.
v1_to_v2() {
    local v1_path="$1"
    case "$v1_path" in
        dot-claude/scripts/*)   echo "system/scripts/${v1_path#dot-claude/scripts/}" ;;
        dot-claude/skills/*)    echo "system/skills/${v1_path#dot-claude/skills/}" ;;
        dot-claude/commands/*)  echo "system/commands/${v1_path#dot-claude/commands/}" ;;
        CLAUDE.md)              echo "templates/CLAUDE.md" ;;
        *) return 1 ;;
    esac
}

# Translate a v2 source-relative path to the v1 equivalent.
# Prints the v1 path to stdout; returns 0 on success, 1 if path has no v1 equivalent.
v2_to_v1() {
    local v2_path="$1"
    case "$v2_path" in
        system/scripts/*)    echo "dot-claude/scripts/${v2_path#system/scripts/}" ;;
        system/skills/*)     echo "dot-claude/skills/${v2_path#system/skills/}" ;;
        system/commands/*)   echo "dot-claude/commands/${v2_path#system/commands/}" ;;
        templates/CLAUDE.md) echo "CLAUDE.md" ;;
        *) return 1 ;;
    esac
}

# Detect the source repo layout (v1 = dot-claude/, v2 = system/ + templates/).
# Takes one argument: the source repo root directory.
# Prints "v1", "v2", or "unknown" to stdout. Exit 0 for known layouts, 1 for unknown.
detect_layout() {
    local root="$1"
    if [ -d "$root/dot-claude" ]; then
        # v1 takes priority if both layouts coexist (migration state)
        echo "v1"
        return 0
    fi
    if [ -d "$root/system" ] && [ -f "$root/templates/CLAUDE.md" ]; then
        echo "v2"
        return 0
    fi
    echo "unknown"
    return 1
}
