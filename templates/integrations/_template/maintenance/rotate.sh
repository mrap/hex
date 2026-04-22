#!/usr/bin/env bash
# maintenance/rotate.sh — REPLACE_ME key/secret rotation skeleton
set -uo pipefail

HEX_ROOT="${HEX_ROOT:-/Users/mrap/mrap-hex}"
SECRETS_DIR="$HEX_ROOT/.hex/secrets"
ENV_FILE="$SECRETS_DIR/REPLACE_ME.env"
BUNDLE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "[REPLACE_ME/rotate] TODO: implement rotation logic"

# Example steps:
# 1. Generate new credentials
# 2. Register with provider
# 3. Update ENV_FILE atomically (write to .tmp, then mv)
# 4. Verify via probe
bash "$BUNDLE_DIR/probe.sh" || {
  echo "[REPLACE_ME/rotate] FAIL: probe failed after rotation" >&2
  exit 1
}

# 5. Sync secrets
SYNC_SCRIPT="$HEX_ROOT/.hex/scripts/sync-secrets.sh"
[[ -f "$SYNC_SCRIPT" ]] && bash "$SYNC_SCRIPT" REPLACE_ME

printf '{"event":"hex.integration.REPLACE_ME.rotated","status":"ok","ts":"%s"}\n' \
  "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
