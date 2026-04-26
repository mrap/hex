#!/usr/bin/env bash
# llm-cli.sh — LLM CLI abstraction layer
# Source this file to use unified LLM CLI functions.
# Supports: claude (Claude Code) and codex (OpenAI Codex CLI)
#
# Usage: source /path/to/.hex/scripts/llm-cli.sh
#
# After sourcing, $LLM_CLI is set to the detected CLI name ("claude", "codex", or "").

# Detect which LLM CLI to use.
# Priority: 1) saved preference from bootstrap, 2) runtime detection
detect_llm_cli() {
    # Check for preference saved during bootstrap
    local pref_file="${AGENT_DIR:-.}/.hex/llm-preference"
    if [ -f "$pref_file" ]; then
        local pref
        pref="$(cat "$pref_file" | tr -d '[:space:]')"
        if command -v "$pref" >/dev/null 2>&1; then
            echo "$pref"
            return
        fi
    fi
    # Fallback: detect at runtime
    if command -v claude >/dev/null 2>&1; then
        echo "claude"
    elif command -v codex >/dev/null 2>&1; then
        echo "codex"
    else
        echo ""
    fi
}

# Return the detected CLI name (uses cached $LLM_CLI)
llm_cli_name() {
    echo "${LLM_CLI:-}"
}

# Start an interactive session with the given prompt
# claude: claude "$prompt"
# codex:  codex "$prompt"
llm_interactive() {
    local prompt="$1"
    case "${LLM_CLI:-}" in
        claude) claude "$prompt" ;;
        codex)  codex "$prompt" ;;
        *) echo "llm-cli: no LLM CLI found (need claude or codex)" >&2; return 1 ;;
    esac
}

# Non-interactive execution with a prompt string
# claude: claude -p "$prompt"
# codex:  codex exec "$prompt"
llm_exec() {
    local prompt="$1"
    case "${LLM_CLI:-}" in
        claude) claude -p "$prompt" ;;
        codex)  codex exec "$prompt" ;;
        *) echo "llm-cli: no LLM CLI found (need claude or codex)" >&2; return 1 ;;
    esac
}

# Non-interactive execution with model override
# For claude: model is passed as-is (e.g., "haiku", "sonnet")
# For codex: "haiku" maps to "gpt-4.1-mini"; others passed as-is
# If called with only model arg (no prompt), reads prompt from stdin.
llm_exec_model() {
    local model="$1"
    local prompt="${2:-}"
    local codex_model
    local session_flag
    session_flag="$(llm_session_flag)"
    case "${LLM_CLI:-}" in
        claude)
            if [ -n "$prompt" ]; then
                claude -p --model "$model" ${session_flag:+$session_flag} "$prompt"
            else
                claude -p --model "$model" ${session_flag:+$session_flag}
            fi
            ;;
        codex)
            # Map claude model names to codex equivalents
            case "$model" in
                haiku*)  codex_model="gpt-4.1-mini" ;;
                sonnet*) codex_model="gpt-4.1" ;;
                opus*)   codex_model="o4-mini" ;;
                *)       codex_model="$model" ;;
            esac
            if [ -n "$prompt" ]; then
                codex exec --model "$codex_model" "$prompt"
            else
                codex exec --model "$codex_model" -
            fi
            ;;
        *) echo "llm-cli: no LLM CLI found (need claude or codex)" >&2; return 1 ;;
    esac
}

# Non-interactive execution reading prompt from stdin
# claude: claude -p
# codex:  codex exec -
llm_exec_stdin() {
    case "${LLM_CLI:-}" in
        claude) claude -p ;;
        codex)  codex exec - ;;
        *) echo "llm-cli: no LLM CLI found (need claude or codex)" >&2; return 1 ;;
    esac
}

# Return the no-persistence session flag for the detected CLI
# claude: --no-session-persistence
# codex:  --ephemeral
llm_session_flag() {
    case "${LLM_CLI:-}" in
        claude) echo "--no-session-persistence" ;;
        codex)  echo "--ephemeral" ;;
        *)      echo "" ;;
    esac
}

# Initialize: detect and cache the CLI
LLM_CLI="$(detect_llm_cli)"
