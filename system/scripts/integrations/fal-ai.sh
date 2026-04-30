#!/usr/bin/env bash
set -uo pipefail

SECRETS_FILE="$(dirname "$(dirname "$(dirname "$0")")")/secrets/fal.env"

if [[ ! -f "$SECRETS_FILE" ]]; then
  echo "ERROR: $SECRETS_FILE not found" >&2
  exit 1
fi

# shellcheck disable=SC1090
source "$SECRETS_FILE"

if [[ -z "${FAL_KEY:-}" ]]; then
  echo "ERROR: FAL_KEY is empty in $SECRETS_FILE" >&2
  exit 1
fi

HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 "https://fal.run/" 2>/dev/null)
CURL_EXIT=$?

if [[ $CURL_EXIT -ne 0 ]]; then
  echo "ERROR: curl failed with exit $CURL_EXIT — fal.run unreachable" >&2
  exit 1
fi

# Accept 2xx, 3xx, or 4xx — all mean endpoint is alive; only 5xx or curl fail = unhealthy
if [[ "$HTTP_CODE" =~ ^[234] ]]; then
  echo "OK: FAL_KEY present; fal.run returned $HTTP_CODE" >&2
  exit 0
else
  echo "ERROR: fal.run returned HTTP $HTTP_CODE" >&2
  exit 1
fi
