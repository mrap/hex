#!/usr/bin/env bash
# x-twitter probe — verify X API is reachable with the current OAuth2 bearer token.
set -uo pipefail

ENV_FILE="$HOME/github.com/xdevplatform/xmcp/.env"

if [ ! -f "$ENV_FILE" ]; then
    echo "[x-twitter/probe] No env file at $ENV_FILE — skipping bearer check" >&2
    exit 0
fi

ACCESS_TOKEN=$(grep '^X_OAUTH2_ACCESS_TOKEN=' "$ENV_FILE" 2>/dev/null | cut -d= -f2-)

if [ -z "${ACCESS_TOKEN:-}" ]; then
    echo "[x-twitter/probe] X_OAUTH2_ACCESS_TOKEN not set in $ENV_FILE" >&2
    exit 1
fi

HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
    --max-time 30 \
    -H "Authorization: Bearer $ACCESS_TOKEN" \
    "https://api.twitter.com/2/users/me")

if [ "$HTTP_CODE" = "200" ]; then
    echo "[x-twitter/probe] OK (HTTP $HTTP_CODE)"
    exit 0
else
    echo "[x-twitter/probe] FAIL (HTTP $HTTP_CODE)" >&2
    exit 1
fi
