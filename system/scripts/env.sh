#!/usr/bin/env bash
# env.sh — Shared environment for hex agents, workers, and scripts.
#
# Every agent wake script and BOI worker sources this file. It guarantees:
#   1. HEX_DIR and AGENT_DIR point to the hex instance
#   2. PATH includes locations where claude/codex and user tools live
#   3. A claude() wrapper bakes in --dangerously-skip-permissions
#   4. TZ is set from the instance's timezone file
#
# Usage (from wake scripts):
#   source "$HEX_DIR/.hex/scripts/env.sh"
#
# Usage (standalone — auto-detects HEX_DIR from script location):
#   source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

# ── Resolve HEX_DIR ─────────────────────────────────────────────────────────
# Priority: HEX_DIR env var > AGENT_DIR env var (deprecated) > auto-detect from script location
# DEPRECATED: AGENT_DIR is a legacy alias. Use HEX_DIR.
if [[ -z "${HEX_DIR:-}" ]]; then
  if [[ -n "${AGENT_DIR:-}" ]]; then
    HEX_DIR="$AGENT_DIR"
  else
    # Auto-detect: env.sh lives at $HEX_DIR/.hex/scripts/env.sh
    HEX_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  fi
fi
export HEX_DIR
export AGENT_DIR="$HEX_DIR"  # deprecated alias — use HEX_DIR
export HEX_ROOT="$HEX_DIR"

# ── PATH: ensure user tools are reachable ────────────────────────────────────
# Agents and workers run in clean environments (launchd, tmux, cron) where the
# user's interactive PATH is not inherited. We add every common install location
# so claude, codex, boi, hex-events, python3, node, and npm are discoverable.
_add_to_path() {
  [[ -d "$1" ]] && [[ ":${PATH}:" != *":$1:"* ]] && export PATH="$1:$PATH"
}

# User-local binaries (pip install --user, cargo install, go install)
_add_to_path "$HOME/.local/bin"
_add_to_path "$HOME/bin"
_add_to_path "$HOME/.cargo/bin"
_add_to_path "$HOME/go/bin"

# Homebrew (macOS)
_add_to_path "/opt/homebrew/bin"
_add_to_path "/usr/local/bin"

# Node.js / npm global (where claude CLI lives after npm install -g)
if [[ -d "$HOME/.npm-global/bin" ]]; then
  _add_to_path "$HOME/.npm-global/bin"
fi
# fnm / nvm managed node
for _ndir in "$HOME/.fnm/aliases/default/bin" "$HOME/.nvm/versions/node"/*/bin; do
  _add_to_path "$_ndir" 2>/dev/null
done

# Python (uv, pyenv)
_add_to_path "$HOME/.local/share/uv/python"
if [[ -d "$HOME/.pyenv/shims" ]]; then
  _add_to_path "$HOME/.pyenv/shims"
fi

# hex binary
_add_to_path "$HEX_DIR/.hex/bin"

# BOI and hex-events bin directories
_add_to_path "$HOME/.boi/bin"
if [[ -d "$HOME/.hex-events/venv/bin" ]]; then
  _add_to_path "$HOME/.hex-events/venv/bin"
fi

unset -f _add_to_path

# ── Timezone ─────────────────────────────────────────────────────────────────
if [[ -z "${TZ:-}" && -f "$HEX_DIR/.hex/timezone" ]]; then
  export TZ
  TZ="$(cat "$HEX_DIR/.hex/timezone")"
fi

# ── claude() wrapper ─────────────────────────────────────────────────────────
# All agent invocations of claude go through this function. It bakes in
# --dangerously-skip-permissions so individual scripts don't need to.
claude() {
  local claude_bin
  claude_bin="$(type -P claude 2>/dev/null)" || {
    echo "ERROR: claude not found on PATH after sourcing env.sh" >&2
    echo "  PATH=$PATH" >&2
    return 127
  }
  "$claude_bin" --dangerously-skip-permissions "$@"
}
export -f claude

# ── Circuit breaker ──────────────────────────────────────────────────────────
# Halts an agent after N consecutive failed wake cycles. Call from wake scripts.
# Usage: agent_check_circuit_breaker "agent-id" "/path/to/log.jsonl" 3
# Returns: 0 = ok, 1 = circuit tripped (HALT file created, alert sent)
agent_check_circuit_breaker() {
  local agent_id="$1"
  local log_file="$2"
  local threshold="${3:-3}"
  local halt_file="$HOME/.hex-${agent_id}-HALT"

  [ -f "$log_file" ] || return 0

  local fail_streak=0
  while IFS= read -r line; do
    if echo "$line" | grep -q '"status":"failed"'; then
      fail_streak=$((fail_streak + 1))
    else
      fail_streak=0
    fi
  done < <(tail -"$threshold" "$log_file")

  if [ "$fail_streak" -ge "$threshold" ]; then
    touch "$halt_file"
    local last_err=""
    local err_file
    err_file="$(dirname "$log_file")/last-error.txt"
    [ -f "$err_file" ] && last_err=$(tail -1 "$err_file" 2>/dev/null | head -c 200)
    local msg="Agent *${agent_id}* circuit-tripped: ${fail_streak} consecutive failures. Auto-halted. ${last_err:+Last error: \`$last_err\`}"
    if ! cc-connect send --message "$msg" 2>/dev/null; then
      echo "[WARN] cc-connect notification failed (cc-connect unavailable?)" >&2
    fi
    echo "$msg" >&2
    return 1
  fi
  return 0
}
export -f agent_check_circuit_breaker
