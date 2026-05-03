#!/usr/bin/env bash
# sync-secrets.sh — load every secret in .hex/secrets/*.env into the hex runtime surfaces.
#
# Purpose: one canonical place (~/<hex-workspace>/.hex/secrets/*.env) holds every API key.
# Adding a new key is a two-step: (1) drop a new .env file in that dir, (2) run this.
#
# What this does:
#   1. `launchctl setenv KEY VAL` for every KEY=VAL line in .hex/secrets/*.env
#      → makes them visible to user-launched processes + new launchd services
#
# Idempotent. Run anytime a key is added, updated, or removed.
# Hex-core — do not auto-mutate (Red tier).

set -euo pipefail

HEX_DIR="${CLAUDE_PROJECT_DIR:-${HEX_DIR:-$HOME/hex}}"
SECRETS_DIR="$HEX_DIR/.hex/secrets"

if [ ! -d "$SECRETS_DIR" ]; then
  echo "ERR: $SECRETS_DIR does not exist" >&2
  exit 1
fi

# --- 1. Collect every KEY=VAL from secrets/*.env (skip comments + blank) ---
declare -a KEYS VALS
while IFS= read -r line; do
  [[ -z "$line" || "$line" =~ ^[[:space:]]*# ]] && continue
  if [[ "$line" =~ ^[[:space:]]*([A-Z_][A-Z0-9_]*)=(.*)$ ]]; then
    KEYS+=("${BASH_REMATCH[1]}")
    VALS+=("${BASH_REMATCH[2]}")
  fi
done < <(cat "$SECRETS_DIR"/*.env 2>/dev/null || true)

if [ ${#KEYS[@]} -eq 0 ]; then
  echo "No secrets found in $SECRETS_DIR/*.env"
  exit 0
fi

echo "Loaded ${#KEYS[@]} secret(s) from $SECRETS_DIR:"
for k in "${KEYS[@]}"; do echo "  - $k"; done

# --- 2. Export into user launchctl env ---
for i in "${!KEYS[@]}"; do
  launchctl setenv "${KEYS[$i]}" "${VALS[$i]}"
done
echo "✓ launchctl setenv applied ($(launchctl getenv FAL_KEY > /dev/null 2>&1 && echo 'FAL_KEY visible' || echo 'FAL_KEY not visible'))"

echo ""
echo "Done. Secrets from $SECRETS_DIR exported to launchctl user env."
