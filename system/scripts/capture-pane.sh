#!/usr/bin/env bash
# capture-pane.sh — Persistent quick capture input for the hex tmux workspace.
# Runs in a tiny tmux pane. Type a thought, hit Enter, it's saved instantly.
#
# Captures are saved to {workspace}/raw/captures/ with YAML frontmatter,
# same format as capture.sh (CLI capture tool).

set -uo pipefail

# ─── Resolve workspace path ──────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
candidate="$SCRIPT_DIR"
while [ "$candidate" != "/" ]; do
  if [ -f "$candidate/CLAUDE.md" ]; then
    WORKSPACE="$candidate"
    break
  fi
  candidate="$(dirname "$candidate")"
done

if [ -z "${WORKSPACE:-}" ]; then
  echo "Error: Could not find CLAUDE.md. Run from hex workspace."
  exit 1
fi

CAPTURES_DIR="$WORKSPACE/raw/captures"
mkdir -p "$CAPTURES_DIR"

# Use configured timezone (SO #17)
if [ -z "${TZ:-}" ] && [ -f "$WORKSPACE/.hex/timezone" ]; then
  TZ="$(tr -d '[:space:]' < "$WORKSPACE/.hex/timezone")"; export TZ
fi

# ─── Colors ───────────────────────────────────────────────────────────────────
DIM='\033[2m'
CYAN='\033[36m'
GREEN='\033[32m'
RESET='\033[0m'

# ─── Helpers ──────────────────────────────────────────────────────────────────
today_count() {
  local today
  today="$(date '+%Y-%m-%d')"
  local count=0
  for f in "$CAPTURES_DIR/${today}"_*.md; do
    [ -e "$f" ] && count=$((count + 1))
  done
  echo "$count"
}

draw_header() {
  local count
  count="$(today_count)"
  tput cup 0 0
  tput el
  printf "${DIM}── 💡 Capture ─── today: %d ──${RESET}" "$count"
}

clear_confirmation() {
  tput cup 1 0
  tput el
}

show_confirmation() {
  tput cup 1 0
  tput el
  printf "  ${GREEN}✓ captured${RESET}"
  (
    sleep 1.5
    tput cup 1 0
    tput el
  ) &
}

# ─── Cleanup on exit ─────────────────────────────────────────────────────────
cleanup() {
  printf "\n${DIM}Capture pane closed.${RESET}\n"
  exit 0
}
trap cleanup INT TERM

# ─── Main loop ────────────────────────────────────────────────────────────────
tput clear
draw_header
clear_confirmation

while true; do
  # Position cursor on the prompt line
  tput cup 2 0
  tput el
  printf "${CYAN}›${RESET} "

  # Read input
  INPUT=""
  if ! read -r INPUT; then
    # EOF (Ctrl+D)
    break
  fi

  # Skip empty input
  if [ -z "${INPUT// /}" ]; then
    continue
  fi

  # Save capture (atomic write)
  TIMESTAMP="$(date '+%Y-%m-%dT%H:%M:%S')"
  FILENAME="$(date '+%Y-%m-%d_%H-%M-%S').md"
  OUTFILE="$CAPTURES_DIR/$FILENAME"
  TMPOUT="$OUTFILE.tmp"

  cat > "$TMPOUT" <<CAPTURE_EOF
---
captured: $TIMESTAMP
source: tmux-pane
---

$INPUT
CAPTURE_EOF

  mv "$TMPOUT" "$OUTFILE"

  # Update header with new count and show confirmation
  draw_header
  show_confirmation
done

cleanup
