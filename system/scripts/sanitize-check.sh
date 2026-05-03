#!/usr/bin/env bash
# sanitize-check.sh — Scan for personalization that would break for other users.
# Exits 0 if clean, 1 with a list of violations if any are found.
#
# Usage:
#   bash system/scripts/sanitize-check.sh
#   bash system/scripts/sanitize-check.sh --verbose  # show each matching line

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SELF="$(basename "${BASH_SOURCE[0]}")"
VERBOSE=false
for arg in "$@"; do [[ "$arg" == "--verbose" ]] && VERBOSE=true; done

cd "$REPO_DIR"

red()   { printf '\033[31m%s\033[0m\n' "$*" >&2; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }

VIOLATIONS=()

# run_check LABEL GREP_PATTERN [extra grep args...]
# Runs grep, filters common false positives, records violations.
run_check() {
    local label="$1"
    local pattern="$2"
    shift 2
    local results
    results=$(grep -rn "$pattern" . \
        --exclude-dir=.git \
        --exclude-dir=target \
        --exclude-dir=node_modules \
        --exclude-dir=.boi \
        --exclude-dir=worktrees \
        --exclude-dir=__pycache__ \
        --exclude-dir=dist \
        --exclude-dir=.hex \
        --exclude-dir=.claude \
        "$@" 2>/dev/null \
        | grep -v "/${SELF}:" \
        | grep -v "personalization-audit" \
    | grep -v "PATH=.*opt.homebrew" \
        | grep -v "Co-Authored" \
        | grep -v -i "# example\|# e\.g\.\|example:\|# Example" \
        || true)
    if [ -n "$results" ]; then
        VIOLATIONS+=("$label")
        COUNT=$(echo "$results" | wc -l | tr -d ' ')
        if $VERBOSE; then
            red "  [$label] $COUNT violation(s):"
            echo "$results" | while IFS= read -r line; do
                printf '    %s\n' "$line" >&2
            done
        else
            red "  [$label] $COUNT violation(s)"
        fi
    fi
}

echo "Scanning for personalization violations..."
echo ""

# Absolute user home paths (hardcoded, not via $HOME or ~)
run_check "hardcoded /Users/ path" "/Users/[a-zA-Z]" \
    --include="*.py" --include="*.sh" --include="*.yaml" \
    --include="*.md" --include="*.json" --include="*.toml"

# mrap-specific identifiers (exclude projects/ — development workspace, not distributed content)
run_check "mrap-specific identifier" \
    "mrap-hex\|mrap-mrap\|mike@mrap\|mrap\.me\|Mike Rapadas" \
    --exclude-dir=projects \
    --include="*.py" --include="*.sh" --include="*.yaml" \
    --include="*.md" --include="*.json"

# Slack-specific channel IDs
run_check "Slack channel IDs" \
    "C0AQZR31EET\|C0AUEAFASQP\|C0B05456Z2L"

# Tailscale hostname/IP specific to this machine
run_check "Tailscale hostname/IP" \
    "tailbd5748\|mac-mini\.tail\|100\.101\.9\."

# macOS LaunchAgent plists tied to com.mrap namespace
run_check "com.mrap. LaunchAgent" \
    "com\.mrap\." \
    --include="*.py" --include="*.sh" --include="*.plist"

# Hardcoded /opt/homebrew when NOT behind an existence guard.
# Legitimate uses (inside "if [ -d /opt/homebrew ]" blocks, macOS VM builders) are excluded.
BREW_VIOLATIONS=$(grep -rn "/opt/homebrew" . \
    --exclude-dir=.git \
    --exclude-dir=eval \
    --exclude-dir=tests \
    --include="*.py" --include="*.sh" 2>/dev/null \
    | grep -v "/${SELF}:" \
    | grep -v 'if.*-d.*opt/homebrew' \
    | grep -v '\[ -d.*opt/homebrew' \
    | grep -v '\[\[ -d.*opt/homebrew' \
    | grep -v 'opt/homebrew.*&&\|&&.*opt/homebrew' \
    | grep -v '_add_to_path' \
    | grep -v "personalization-audit" \
    | grep -v "PATH=.*opt.homebrew" \
    || true)
if [ -n "$BREW_VIOLATIONS" ]; then
    VIOLATIONS+=("hardcoded /opt/homebrew")
    COUNT=$(echo "$BREW_VIOLATIONS" | wc -l | tr -d ' ')
    if $VERBOSE; then
        red "  [hardcoded /opt/homebrew] $COUNT violation(s):"
        echo "$BREW_VIOLATIONS" | while IFS= read -r line; do
            printf '    %s\n' "$line" >&2
        done
    else
        red "  [hardcoded /opt/homebrew] $COUNT violation(s)"
    fi
fi

# Hardcoded secrets paths with actual credentials (not generic placeholders)
SECRETS_VIOLATIONS=$(grep -rn "secrets/slack-bot-token\|\.hex/secrets/[a-zA-Z][a-zA-Z0-9_-]*\.\(env\|key\)" . \
    --exclude-dir=.git \
    --include="*.py" --include="*.sh" 2>/dev/null \
    | grep -v "/${SELF}:" \
    | grep -v "personalization-audit" \
    | grep -v "PATH=.*opt.homebrew" \
    | grep -v '<name>\|REPLACE_ME\|YOUR_' \
    | grep -v 'HEX_DIR.*secrets\|HEX_ROOT.*secrets' \
    || true)
if [ -n "$SECRETS_VIOLATIONS" ]; then
    VIOLATIONS+=("hardcoded secrets path")
    COUNT=$(echo "$SECRETS_VIOLATIONS" | wc -l | tr -d ' ')
    if $VERBOSE; then
        red "  [hardcoded secrets path] $COUNT violation(s):"
        echo "$SECRETS_VIOLATIONS" | while IFS= read -r line; do
            printf '    %s\n' "$line" >&2
        done
    else
        red "  [hardcoded secrets path] $COUNT violation(s)"
    fi
fi

# Claude-Code-only: direct 'claude -p' or 'claude exec' invocations that bypass
# the runtime abstraction (hex_invoke / env.sh wrapper). New scripts should use
# hex_invoke so they work on both Claude Code and Codex runtimes.
CLAUDE_BIN_VIOLATIONS=$(grep -rn 'claude\s\+-p\b\|exec\s\+claude\b\|\bcodex exec\b' . \
    --exclude-dir=.git \
    --exclude-dir=tests \
    --exclude-dir=eval \
    --include="*.sh" 2>/dev/null \
    | grep -v "/${SELF}:" \
    | grep -v '/env\.sh:' \
    | grep -v '/runtime\.sh:' \
    | grep -v 'hex-agent-spawn\.sh:' \
    | grep -v 'llm-cli\.sh:' \
    | grep -v 'meeting-prep\.sh:' \
    | grep -v 'system-introspection\.sh:' \
    | grep -v 'hex-ui-feedback-tick\.sh:' \
    | grep -v "personalization-audit" \
    | grep -v "PATH=.*opt.homebrew" \
    | grep -v '^\s*#' \
    || true)
if [ -n "$CLAUDE_BIN_VIOLATIONS" ]; then
    VIOLATIONS+=("hardcoded-runtime-binary")
    COUNT=$(echo "$CLAUDE_BIN_VIOLATIONS" | wc -l | tr -d ' ')
    if $VERBOSE; then
        red "  [hardcoded-runtime-binary] $COUNT violation(s) — use hex_invoke instead of direct claude/codex invocation:"
        echo "$CLAUDE_BIN_VIOLATIONS" | while IFS= read -r line; do
            printf '    %s\n' "$line" >&2
        done
    else
        red "  [hardcoded-runtime-binary] $COUNT violation(s) — use hex_invoke instead of direct claude/codex invocation"
    fi
fi

# Claude-Code-only: .claude/ paths hardcoded without a runtime guard or fallback.
# Scripts referencing .claude/ directly break on Codex which uses .codex/.
# Legitimate exceptions: env.sh, runtime.sh, doctor.sh, and files that also
# reference HEX_RUNTIME or .codex/ as a fallback.
CLAUDE_PATH_FILES=$(grep -rln '\.claude/' . \
    --exclude-dir=.git \
    --exclude-dir=tests \
    --exclude-dir=eval \
    --include="*.sh" --include="*.py" 2>/dev/null \
    | grep -v "/${SELF}" \
    | grep -v '/env\.sh$' \
    | grep -v '/runtime\.sh$' \
    | grep -v '/doctor\.sh$' \
    | grep -v '/install\.sh$' \
    | grep -v '/bootstrap-migrate\.sh$' \
    | grep -v '/migrate-v1-to-v2\.sh$' \
    | grep -v '/backup_session\.sh$' \
    | grep -v '/consolidate\.sh$' \
    | grep -v '/cleanup-project-jsonl\.sh$' \
    | grep -v '/test_upgrade_prune\.sh$' \
    | grep -v "personalization-audit" \
    | grep -v "PATH=.*opt.homebrew" \
    || true)
CLAUDE_PATH_VIOLATIONS=""
while IFS= read -r fpath; do
    [ -z "$fpath" ] && continue
    # Allow if the file also references HEX_RUNTIME, .codex, or CLAUDE_PROJECT_MEMORY as fallback guard
    if ! grep -q 'HEX_RUNTIME\|\.codex\|CLAUDE_PROJECT_MEMORY' "$fpath" 2>/dev/null; then
        matches=$(grep -n '\.claude/' "$fpath" 2>/dev/null || true)
        CLAUDE_PATH_VIOLATIONS="${CLAUDE_PATH_VIOLATIONS}${matches}"$'\n'
    fi
done <<< "$CLAUDE_PATH_FILES"
CLAUDE_PATH_VIOLATIONS=$(echo "$CLAUDE_PATH_VIOLATIONS" | grep -v '^\s*$' || true)
if [ -n "$CLAUDE_PATH_VIOLATIONS" ]; then
    VIOLATIONS+=("hardcoded-.claude/-path-no-fallback")
    COUNT=$(echo "$CLAUDE_PATH_VIOLATIONS" | wc -l | tr -d ' ')
    if $VERBOSE; then
        red "  [hardcoded-.claude/-path-no-fallback] $COUNT violation(s) — add HEX_RUNTIME guard or .codex fallback:"
        echo "$CLAUDE_PATH_VIOLATIONS" | while IFS= read -r line; do
            printf '    %s\n' "$line" >&2
        done
    else
        red "  [hardcoded-.claude/-path-no-fallback] $COUNT violation(s) — add HEX_RUNTIME guard or .codex fallback"
    fi
fi

echo ""

if [ ${#VIOLATIONS[@]} -eq 0 ]; then
    green "CLEAN — no personalization violations found"
    exit 0
else
    red "VIOLATIONS FOUND in: ${VIOLATIONS[*]}"
    red "Run with --verbose for details. Fix before pushing."
    exit 1
fi
