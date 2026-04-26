#!/bin/bash
# Launch hex ui server
# Usage: bash ui.sh [port]

set -uo pipefail

# ─── Resolve agent directory ────────────────────────────────────────────────
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
  echo "Error: Could not find CLAUDE.md. Run this from your hexagon directory."
  exit 1
fi

UI_DIR="$WORKSPACE/.hex/ui"

if [ ! -f "$UI_DIR/app.py" ]; then
  echo "Error: UI not found at $UI_DIR/app.py. Run /hex-upgrade to install."
  exit 1
fi

# ─── Install dependencies if needed ─────────────────────────────────────────
if ! python3 -c "import fastapi, uvicorn, jinja2" 2>/dev/null; then
  echo "Installing UI dependencies..."
  pip install -q -r "$UI_DIR/requirements.txt" 2>/dev/null
fi

# ─── Launch ──────────────────────────────────────────────────────────────────
PORT="${1:-3141}"
echo "Hexagon UI running at http://localhost:$PORT"

# Detect remote environment and print tunnel instructions
if [ -n "${SSH_CONNECTION:-}" ] || [ -n "${SSH_TTY:-}" ]; then
  TUNNEL_SCRIPT="$SCRIPT_DIR/ui-tunnel.sh"
  if [ -f "$TUNNEL_SCRIPT" ]; then
    bash "$TUNNEL_SCRIPT" "$PORT"
  fi
fi

python3 "$UI_DIR/app.py" --workspace "$WORKSPACE" --port "$PORT"
