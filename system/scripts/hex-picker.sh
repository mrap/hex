#!/usr/bin/env bash
# hex-picker.sh — fzf workspace picker for hex contexts
# Usage: hex-picker.sh
#   Shows an fzf popup listing all workspaces.
#   Select one to switch, or type a new name to create it.
set -uo pipefail

SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ---------------------------------------------------------------------------
# fzf availability and version check
# ---------------------------------------------------------------------------
if ! command -v fzf &>/dev/null; then
    echo "fzf is required for the workspace picker. Install: brew install fzf" >&2
    exit 1
fi

# --tmux flag requires fzf >= 0.49.0; fall back to --height for older versions
_fzf_version="$(fzf --version 2>/dev/null | awk '{print $1}')"
_fzf_use_tmux=1
if [[ -n "$_fzf_version" ]]; then
    IFS='.' read -r _fzf_major _fzf_minor _fzf_patch <<< "$_fzf_version"
    _fzf_major="${_fzf_major:-0}"; _fzf_minor="${_fzf_minor:-0}"; _fzf_patch="${_fzf_patch:-0}"
    if (( _fzf_major < 0 || ( _fzf_major == 0 && _fzf_minor < 49 ) )); then
        _fzf_use_tmux=0
    fi
fi

# shellcheck source=hex-context-lib.sh
source "$SCRIPTS_DIR/hex-context-lib.sh"

_ctx_ensure_registry

# ---------------------------------------------------------------------------
# Build display lines: "<marker> <name>  <relative-time>"
# Marker: "▶" for active, " " for others
# ---------------------------------------------------------------------------
display_lines() {
  python3 - "$HEX_CONTEXTS_JSON" <<'PYEOF'
import json, sys
from datetime import datetime, timezone

path = sys.argv[1]
with open(path) as f:
    d = json.load(f)

active = d.get("active", "")
contexts = d.get("contexts", {})

def rel_time(ts_str):
    if not ts_str:
        return "never"
    try:
        ts = datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
        delta = datetime.now(timezone.utc) - ts
        s = int(delta.total_seconds())
        if s < 60:
            return f"{s}s ago"
        elif s < 3600:
            return f"{s // 60}m ago"
        elif s < 86400:
            return f"{s // 3600}h ago"
        else:
            return f"{s // 86400}d ago"
    except Exception:
        return "?"

# Active context first, then others sorted by last_active desc (MRU first)
active_names = [n for n in contexts.keys() if n == active]
other_names = [n for n in contexts.keys() if n != active]
other_names.sort(key=lambda n: contexts.get(n, {}).get("last_active", ""), reverse=True)
names = active_names + other_names

# Also include active if not yet in contexts dict
if active and active not in contexts:
    names = [active] + names

for name in names:
    entry = contexts.get(name, {})
    marker = "▶" if name == active else " "
    last = rel_time(entry.get("last_active", ""))
    display_name = entry.get("display_name", name)
    print(f"{marker} {display_name}\t{last}")
PYEOF
}

# ---------------------------------------------------------------------------
# Run fzf
# ---------------------------------------------------------------------------
_fzf_opts=(
  --header "Workspaces  |  Enter=switch  Ctrl-N=new  Ctrl-X=back"
  --prompt "  "
  --pointer "▶"
  --marker "●"
  --ansi
  --no-multi
  --bind "ctrl-n:print(NEW WORKSPACE)+abort"
  --bind "ctrl-x:print(__BACK__)+abort"
  --delimiter "\t"
  --with-nth 1
  --tabstop 4
  --height "40%"
)
if (( _fzf_use_tmux )); then
  _fzf_opts=(--tmux "center,60%,40%" "${_fzf_opts[@]}")
fi

lines="$(display_lines)"

# Add "+ New workspace..." entry at the bottom
lines_with_new="${lines}
  + New workspace..."

selected="$(printf '%s\n' "$lines_with_new" | fzf "${_fzf_opts[@]}" 2>/dev/null || true)"

if [[ -z "$selected" ]]; then
  exit 0
fi

# ---------------------------------------------------------------------------
# Handle special actions
# ---------------------------------------------------------------------------
if [[ "$selected" == "__BACK__" ]]; then
  exec "$SCRIPTS_DIR/hex-context-switch.sh" --back
fi

if [[ "$selected" == "NEW WORKSPACE" || "$selected" == *"+ New workspace..."* ]]; then
  # Prompt for new workspace name
  _new_fzf_opts=(
    --header "New workspace name (type and press Enter):"
    --prompt "  Name: "
    --print-query
    --no-sort
    --query ""
    --height "20%"
  )
  if (( _fzf_use_tmux )); then
    _new_fzf_opts=(--tmux "center,60%,20%" "${_new_fzf_opts[@]}")
  fi
  new_name="$(printf '' | fzf "${_new_fzf_opts[@]}" 2>/dev/null | head -1 || true)"
  new_name="$(printf '%s' "$new_name" | tr -d '[:space:]')"
  if [[ -z "$new_name" ]]; then
    exit 0
  fi
  exec "$SCRIPTS_DIR/hex-context-switch.sh" "$new_name"
fi

# Extract context name from selected line (strip marker and trim)
# Format is: "▶ name\ttime" or "  name\ttime"
context_name="$(printf '%s' "$selected" | sed 's/^[▶ ] //' | cut -f1 | sed 's/[[:space:]]*$//')"

if [[ -z "$context_name" ]]; then
  exit 0
fi

exec "$SCRIPTS_DIR/hex-context-switch.sh" "$context_name"
