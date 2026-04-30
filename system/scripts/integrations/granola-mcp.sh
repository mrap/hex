#!/usr/bin/env bash
set -uo pipefail

INTEGRATION="granola-mcp"

# Step 1 — Verify supabase.json exists (auth source)
CREDS="$HOME/Library/Application Support/Granola/supabase.json"
if [ ! -f "$CREDS" ]; then
  echo "$INTEGRATION: credentials file not found at $CREDS" >&2
  exit 1
fi

# Step 2 — Verify MCP server binary exists and is runnable
SERVER="$HOME/.hex/integrations/granola-mcp/dist/index.js"
if [ ! -f "$SERVER" ]; then
  echo "$INTEGRATION: MCP server not found at $SERVER" >&2
  exit 1
fi

if ! command -v node >/dev/null 2>&1; then
  echo "$INTEGRATION: node not found" >&2
  exit 1
fi

# Step 3 — Verify registered in claude config
if ! grep -q "granola" "$HOME/.claude.json" 2>/dev/null; then
  echo "$INTEGRATION: not registered in ~/.claude.json" >&2
  exit 1
fi

exit 0
