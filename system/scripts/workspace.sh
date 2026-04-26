#!/bin/bash
# workspace.sh — Launch hex workspace: LLM CLI + landings dashboard
#
# Usage:
#   bash workspace.sh           # Launch workspace
#   alias hex='bash /path/to/workspace.sh'   # Add to .bashrc/.zshrc
#
# Behavior:
#   Not in tmux  → Creates tmux session "hex", splits panes, launches both
#   In tmux      → Splits current window, launches dashboard in right pane
#   Session exists → Attaches to existing "hex" session

# ─── Resolve agent directory ────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
candidate="$SCRIPT_DIR"
while [ "$candidate" != "/" ]; do
  if [ -f "$candidate/CLAUDE.md" ]; then
    AGENT_DIR="$candidate"
    break
  fi
  candidate="$(dirname "$candidate")"
done

if [ -z "${AGENT_DIR:-}" ]; then
  echo "Error: Could not find CLAUDE.md. Run this from your hex directory."
  exit 1
fi

# ─── Load LLM CLI abstraction ────────────────────────────────────────────────
source "$AGENT_DIR/.hex/scripts/llm-cli.sh"

DASHBOARD="$AGENT_DIR/.hex/scripts/landings-dashboard.sh"
CAPTURE_PANE="$AGENT_DIR/.hex/scripts/capture-pane.sh"
HEX_WATCHER="$AGENT_DIR/.hex/scripts/hex-watcher"
HEX_BOT="$AGENT_DIR/.hex/scripts/hex-bot"
HEX_PICKER="$AGENT_DIR/.hex/scripts/hex-picker.sh"
HEX_CONTEXT_STATUS="$AGENT_DIR/.hex/scripts/hex-context-status.sh"
SESSION_NAME="hex"
DASH_WIDTH="10%"
BOI_BIN="$HOME/.local/bin/boi"

# ─── Start BOI watcher (idempotent, survives tmux restarts) ──────────────
if [ -x "$HEX_WATCHER" ]; then
  "$HEX_WATCHER" start
fi

# ─── Start Telegram bot (idempotent, survives tmux restarts) ─────────────
if [ -x "$HEX_BOT" ]; then
  "$HEX_BOT" start
fi

# Helper: get the first window index (respects base-index setting)
first_win() { tmux list-windows -t "$SESSION_NAME" -F '#{window_index}' | head -1; }

# ─── Already in the hex session? ────────────────────────────────────────────
if [ -n "${TMUX:-}" ]; then
  CURRENT_SESSION=$(tmux display-message -p '#S')
  if [ "$CURRENT_SESSION" = "$SESSION_NAME" ]; then
    # Already in hex session. Check if dashboard pane exists.
    PANE_COUNT=$(tmux list-panes | wc -l | tr -d ' ')
    if [ "$PANE_COUNT" -eq 1 ]; then
      # Split and launch dashboard
      tmux split-window -h -l "$DASH_WIDTH" "AGENT_DIR='$AGENT_DIR' bash '$DASHBOARD' --watch"
      # Split dashboard pane to create BOI status pane below it (if boi is installed)
      W=$(first_win)
      DASH_PANE=$(tmux list-panes -t "$SESSION_NAME:$W" -F '#{pane_index}' | tail -1)
      if [ -x "$BOI_BIN" ]; then
        tmux split-window -t "$SESSION_NAME:$W.$DASH_PANE" -v -l 35% -c "$AGENT_DIR" \
          "'$BOI_BIN' status --compact"
      fi
      MAIN_PANE=$(tmux list-panes -t "$SESSION_NAME:$W" -F '#{pane_index}' | head -1)
      tmux select-pane -t "$SESSION_NAME:$W.$MAIN_PANE"
    elif [ "$PANE_COUNT" -eq 2 ] && [ -x "$BOI_BIN" ]; then
      # Dashboard exists but no BOI pane — add it
      W=$(first_win)
      DASH_PANE=$(tmux list-panes -t "$SESSION_NAME:$W" -F '#{pane_index}' | tail -1)
      tmux split-window -t "$SESSION_NAME:$W.$DASH_PANE" -v -l 35% -c "$AGENT_DIR" \
        "'$BOI_BIN' status --compact"
    fi
    # If claude isn't running in the main pane, start it
    exit 0
  fi
fi

# ─── Tmux session already exists? Attach to it. ────────────────────────────
if tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
  if [ -n "${TMUX:-}" ]; then
    tmux switch-client -t "$SESSION_NAME"
  else
    exec tmux attach-session -t "$SESSION_NAME"
  fi
  exit 0
fi

# ─── Create new tmux session ────────────────────────────────────────────────
# Start session with LLM CLI in the main pane.
# Detect user's shell (zsh or bash) and load their rc file for aliases/functions.
USER_SHELL="$(basename "${SHELL:-bash}")"
case "$USER_SHELL" in
  zsh)  SHELL_CMD="zsh -ic" ;;
  *)    SHELL_CMD="bash -ic" ;;
esac

# Build the LLM launch command.
# Claude Code: pass '/hex-startup' as initial prompt (triggers slash command)
# Codex: pass as initial prompt (Codex reads AGENTS.md for startup instructions)
CLI_NAME="$(llm_cli_name)"
tmux new-session -d -s "$SESSION_NAME" -c "$AGENT_DIR" "$SHELL_CMD \"$CLI_NAME '/hex-startup'\""

# Split right pane for dashboard
# Note: sleep between splits to let tmux stabilize dimensions before
# programs start rendering. Without this, content renders at pre-split
# widths and appears cut off until the first refresh cycle.
tmux split-window -h -t "$SESSION_NAME" -l "$DASH_WIDTH" -c "$AGENT_DIR" \
  "sleep 0.5 && AGENT_DIR='$AGENT_DIR' bash '$DASHBOARD' --watch"

# Split dashboard pane to create BOI status pane below it (if boi is installed)
W=$(first_win)
DASH_PANE=$(tmux list-panes -t "$SESSION_NAME:$W" -F '#{pane_index}' | tail -1)
if [ -x "$BOI_BIN" ]; then
  tmux split-window -t "$SESSION_NAME:$W.$DASH_PANE" -v -l 35% -c "$AGENT_DIR" \
    "sleep 0.5 && '$BOI_BIN' status --compact"
fi

# Focus the main (left) pane
MAIN_PANE=$(tmux list-panes -t "$SESSION_NAME:$W" -F '#{pane_index}' | head -1)
tmux select-pane -t "$SESSION_NAME:$W.$MAIN_PANE"

# ─── Register "main" context and bind workspace picker hotkey ────────────────
(source "$AGENT_DIR/.hex/scripts/hex-context-lib.sh" && ctx_register "main") || true
if [ -f "$HEX_PICKER" ]; then
  tmux bind-key -T root "C-\\" run-shell "bash '$HEX_PICKER'"
fi

# ─── Set tmux status-right to show workspace indicator ───────────────────────
if [ -f "$HEX_CONTEXT_STATUS" ]; then
  tmux set-option -t "$SESSION_NAME" status-right "#(bash '$HEX_CONTEXT_STATUS') %H:%M"
  tmux set-option -t "$SESSION_NAME" status-interval 5
fi

# Attach
if [ -n "${TMUX:-}" ]; then
  tmux switch-client -t "$SESSION_NAME"
else
  exec tmux attach-session -t "$SESSION_NAME"
fi
