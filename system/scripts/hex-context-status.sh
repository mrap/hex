#!/usr/bin/env bash
# hex-context-status.sh — Outputs workspace list for tmux status-right
# Format: [main*] [pitch-deck] [research]
# Active context gets asterisk. Must run in <100ms.
set -uo pipefail

if [ -z "${AGENT_DIR:-}" ]; then
  _ctx_status_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  _ctx_candidate="$_ctx_status_dir"
  while [ "$_ctx_candidate" != "/" ]; do
    if [ -f "$_ctx_candidate/CLAUDE.md" ]; then
      AGENT_DIR="$_ctx_candidate"
      break
    fi
    _ctx_candidate="$(dirname "$_ctx_candidate")"
  done
  AGENT_DIR="${AGENT_DIR:-$HOME/hex}"
  unset _ctx_status_dir _ctx_candidate
fi
HEX_CONTEXTS_JSON="${HEX_CONTEXTS_JSON:-$AGENT_DIR/.hex/hex-contexts.json}"

if [[ ! -f "$HEX_CONTEXTS_JSON" ]]; then
  echo "[main*]"
  exit 0
fi

python3 - "$HEX_CONTEXTS_JSON" <<'PYEOF'
import json, sys

try:
    with open(sys.argv[1]) as f:
        d = json.load(f)
except Exception:
    print("[main*]")
    sys.exit(0)

active = d.get("active", "main")
contexts = d.get("contexts", {})

if not contexts:
    print(f"[{active}*]")
    sys.exit(0)

parts = []
for name in contexts:
    marker = "*" if name == active else ""
    parts.append(f"[{name}{marker}]")

# If active context isn't in registry yet, prepend it
if active not in contexts:
    parts.insert(0, f"[{active}*]")

print(" ".join(parts))
PYEOF
