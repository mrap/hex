#!/usr/bin/env bash
# env.sh — Thin shim (Phase 5). Delegates env composition to `hex env`.
# Usage: source "$HEX_DIR/.hex/scripts/env.sh"

# Bootstrap HEX_DIR without invoking hex binary (chicken-and-egg)
if [[ -z "${HEX_DIR:-}" ]]; then
  if [[ -n "${AGENT_DIR:-}" ]]; then
    HEX_DIR="$AGENT_DIR"
  else
    HEX_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  fi
fi
export HEX_DIR AGENT_DIR="$HEX_DIR" HEX_ROOT="$HEX_DIR"

# Bootstrap PATH just enough to find hex, then let Rust compose full PATH
export PATH="$HEX_DIR/.hex/bin:$PATH"
export PATH="$(hex env path)"
_tz="$(hex env tz 2>/dev/null)"
[[ -n "${_tz:-}" && -z "${TZ:-}" ]] && export TZ="$_tz"
unset _tz

# Use the file keyring backend for noninteractive Google Workspace CLI sessions.
# Preserve an explicit caller choice, including an explicitly empty value.
if [[ -z "${GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND+x}" ]]; then
  export GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND=file
fi

# Load only private *.env secret files. Do not disclose paths or values when rejecting
# a file so logs cannot reveal secret-file identity.
if [[ -d "$HEX_DIR/.hex/secrets" ]]; then
  for _sf in "$HEX_DIR"/.hex/secrets/*.env; do
    [[ -e "$_sf" ]] || continue
    # Do not combine these probes with `||` in one command substitution.
    # Some GNU stat variants write output for the unsupported form before
    # returning failure, which corrupts the fallback mode value.
    if ! _mode="$(stat -f '%Lp' "$_sf" 2>/dev/null)" || ! [[ "$_mode" =~ ^[0-7]{3,4}$ ]]; then
      _mode="$(stat -c '%a' "$_sf" 2>/dev/null)" || _mode=""
    fi
    if [[ -z "$_mode" ]] || (( (8#$_mode & 0077) != 0 )); then
      echo "ERROR: refusing to load secret file: group or other permissions are set" >&2
      continue
    fi
    [[ -r "$_sf" ]] && source "$_sf"
  done
  unset _mode _sf
fi

# claude() must live in shell namespace — cannot move to Rust
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

# Lean-by-default headless invocation (spec Sf5bj7y1d). Prepends resolved
# flags from `hex claude-flags <profile>` plus --dangerously-skip-permissions
# so daemon/cron scripts get a clean, plugin/MCP/CLAUDE.md-free claude run.
# Usage: claude_lean <profile> -p "..."
claude_lean() {
  local profile="${1:?claude_lean: profile name required (e.g. harness_worker, eval)}"
  shift
  local claude_bin
  claude_bin="$(type -P claude 2>/dev/null)" || {
    echo "ERROR: claude not found on PATH" >&2
    return 127
  }
  local lean_flags=""
  if command -v hex >/dev/null 2>&1; then
    lean_flags="$(hex claude-flags "$profile" 2>/dev/null || true)"
  fi
  # shellcheck disable=SC2086
  "$claude_bin" --dangerously-skip-permissions $lean_flags "$@"
}
export -f claude_lean

# Circuit breaker must stay shell (process substitution, shell builtins)
agent_check_circuit_breaker() {
  local agent_id="$1" log_file="$2" threshold="${3:-3}"
  local halt_file="$HOME/.hex-${agent_id}-HALT"
  [ -f "$log_file" ] || return 0
  local fail_streak=0
  while IFS= read -r line; do
    echo "$line" | grep -q '"status":"failed"' && fail_streak=$((fail_streak+1)) || fail_streak=0
  done < <(tail -"$threshold" "$log_file")
  if [ "$fail_streak" -ge "$threshold" ]; then
    touch "$halt_file"
    local err_file last_err=""
    err_file="$(dirname "$log_file")/last-error.txt"
    [ -f "$err_file" ] && last_err=$(tail -1 "$err_file" 2>/dev/null | head -c 200)
    local msg="Agent *${agent_id}* circuit-tripped: ${fail_streak} consecutive failures. Auto-halted. ${last_err:+Last error: \`$last_err\`}"
    cc-connect send --message "$msg" 2>/dev/null || echo "[WARN] cc-connect unavailable" >&2
    echo "$msg" >&2
    return 1
  fi
  return 0
}
export -f agent_check_circuit_breaker
