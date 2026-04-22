#!/usr/bin/env bash
# hex-integration-check-all.sh — Run integration checks for a given tier in parallel.
#
# Usage: hex-integration-check-all.sh [--tier critical|standard|slow|all]
# Exit:  0 = all ok, 1 = any fail

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HEX_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
MANIFEST="$HEX_ROOT/projects/integrations/manifest.yaml"
HARNESS="$SCRIPT_DIR/hex-integration-check.sh"

TIER="all"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tier)
      TIER="${2:-all}"
      shift 2
      ;;
    *)
      echo "Unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

if [[ ! -f "$MANIFEST" ]]; then
  echo "[ERROR] manifest not found: $MANIFEST" >&2
  exit 1
fi

# Parse simple YAML — format: `name: tier` lines (no nested keys)
INTEGRATIONS_FILE="$(mktemp /tmp/hex-integrations.XXXXXX)"
trap 'rm -f "$INTEGRATIONS_FILE"' EXIT

python3 - "$MANIFEST" "$TIER" >"$INTEGRATIONS_FILE" <<'PYEOF'
import sys

manifest_path = sys.argv[1]
tier_filter = sys.argv[2]

with open(manifest_path) as f:
    for line in f:
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        if ':' not in line:
            continue
        name, _, tier = line.partition(':')
        name = name.strip()
        tier = tier.strip()
        if tier_filter == 'all' or tier == tier_filter:
            print(name)
PYEOF

if [[ ! -s "$INTEGRATIONS_FILE" ]]; then
  echo "No integrations found for tier: $TIER"
  exit 0
fi

# Run checks in parallel via xargs; capture overall result
set +e
xargs -P 8 -n 1 bash "$HARNESS" < "$INTEGRATIONS_FILE"
OVERALL_RC=$?
set -e

[[ $OVERALL_RC -ne 0 ]] && exit 1
exit 0
