#!/usr/bin/env bash
# Rewrites MCP OAuth auth URLs so the redirect_uri points through hex-router.
#
# Usage: mcp-oauth-rewrite.sh "<auth_url>"
#
# Claude Code generates auth URLs with redirect_uri=http://localhost:{port}/callback.
# When Mike is on a non-Mac-Mini device this callback fails because the local port
# has no server. This script replaces redirect_uri with the hex-router equivalent
# so any device can complete the OAuth flow.
#
# Example:
#   Input:  https://vercel.com/oauth?client_id=...&redirect_uri=http%3A%2F%2Flocalhost%3A49386%2Fcallback&...
#   Output: https://vercel.com/oauth?client_id=...&redirect_uri=https%3A%2F%2F${HEX_HOST:-localhost}%2Fauth%2Fcallback%2F49386&...

set -uo pipefail

HEX_ROUTER_BASE="https://${HEX_HOST:-localhost}"

if [[ $# -lt 1 ]]; then
    echo "Usage: mcp-oauth-rewrite.sh \"<auth_url>\"" >&2
    echo "" >&2
    echo "Paste the auth URL Claude Code gives you, and this script outputs" >&2
    echo "a rewritten URL whose redirect_uri routes through hex-router." >&2
    exit 1
fi

input_url="$1"

python3 - "$input_url" "$HEX_ROUTER_BASE" <<'PYEOF'
import sys
import urllib.parse

input_url = sys.argv[1]
router_base = sys.argv[2]

parsed = urllib.parse.urlparse(input_url)
params = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)

if "redirect_uri" not in params:
    print(f"ERROR: No redirect_uri parameter found in URL", file=sys.stderr)
    print(f"URL: {input_url}", file=sys.stderr)
    sys.exit(1)

original_redirect = params["redirect_uri"][0]
rp = urllib.parse.urlparse(original_redirect)

# Only rewrite localhost redirects
if rp.hostname not in ("localhost", "127.0.0.1", "::1"):
    print(f"ERROR: redirect_uri is not a localhost URL: {original_redirect}", file=sys.stderr)
    print("Only localhost redirect URIs can be rewritten.", file=sys.stderr)
    sys.exit(1)

port = rp.port
if not port:
    print(f"ERROR: No port found in redirect_uri: {original_redirect}", file=sys.stderr)
    sys.exit(1)

new_redirect = f"{router_base}/auth/callback/{port}"
params["redirect_uri"] = [new_redirect]

new_query = urllib.parse.urlencode(params, doseq=True)
new_parsed = parsed._replace(query=new_query)
new_url = urllib.parse.urlunparse(new_parsed)

print(new_url)
print(f"\n[mcp-oauth-rewrite] redirect_uri rewritten:", file=sys.stderr)
print(f"  Before: {original_redirect}", file=sys.stderr)
print(f"  After:  {new_redirect}", file=sys.stderr)
print(f"  Port:   {port}", file=sys.stderr)
PYEOF
