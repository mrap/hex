#!/usr/bin/env bash
# hex-context-switch.sh — Switch between hex workspace contexts
# Usage: hex-context-switch.sh <context-name>
#        hex-context-switch.sh --back
set -uo pipefail

SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=hex-context-lib.sh
source "$SCRIPTS_DIR/hex-context-lib.sh"

# ---------------------------------------------------------------------------
# Environment checks
# ---------------------------------------------------------------------------

if ! command -v tmux &>/dev/null; then
  echo "Error: tmux is not installed. Install with: brew install tmux" >&2
  exit 1
fi

if [[ -z "${TMUX:-}" ]]; then
  echo "Error: not inside a tmux session. Run from inside the hex workspace." >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

_checkpoint_current() {
  local current="$1"
  local handoff_dir
  handoff_dir="$(dirname "$HEX_CONTEXTS_JSON")/handoffs"
  mkdir -p "$handoff_dir"

  local handoff_file="$handoff_dir/${current}.md"
  local ts
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  {
    echo "# Handoff: $current"
    echo "**Checkpointed:** $ts"
    echo ""
    echo "## Active tmux pane capture"
    # Capture visible pane content if inside tmux
    if [[ -n "${TMUX:-}" ]]; then
      tmux capture-pane -p -t "$current" 2>/dev/null || true
    fi
  } > "${handoff_file}.tmp"
  mv "${handoff_file}.tmp" "$handoff_file"
}

_tmux_window_exists() {
  local name="$1"
  tmux list-windows -F "#{window_name}" 2>/dev/null | grep -qx "$name"
}

_switch_to_window() {
  local name="$1"
  tmux select-window -t "$name" 2>/dev/null
}

_create_window() {
  local name="$1"
  # New window named after context; starts a shell (user can then launch Claude)
  tmux new-window -n "$name" 2>/dev/null
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

if [[ $# -eq 0 ]]; then
  echo "Usage: hex-context-switch.sh <context-name>" >&2
  echo "       hex-context-switch.sh --back" >&2
  exit 1
fi

_ctx_ensure_registry

current="$(ctx_get_active)"

# --back: pop context stack and return to previous workspace
if [[ "${1:-}" == "--back" ]]; then
  previous="$(ctx_pop)"
  if [[ -z "$previous" ]]; then
    echo "Already at root context." >&2
    exit 0
  fi
  echo "Returning to: $previous"
  if _tmux_window_exists "$previous"; then
    _switch_to_window "$previous"
  else
    _create_window "$previous"
    ctx_register "$previous"
  fi
  ctx_set_active "$previous"
  exit 0
fi

target="$1"

# Checkpoint current context before switching
if [[ "$current" != "$target" ]]; then
  _checkpoint_current "$current"
fi

# Register target if not known
ctx_register "$target"

# Switch tmux window
if _tmux_window_exists "$target"; then
  _switch_to_window "$target"
else
  _create_window "$target"
fi

# Update registry: push new context onto stack
ctx_push "$target"

echo "Switched to context: $target"
