#!/usr/bin/env bash
# BOI integration hook — run after completing a web-producing spec.
# Usage: boi-hook.sh <spec_file> [<url>] [<selectors>]
# Exit 0: not a web spec, or verification passed.
# Exit 1: web spec detected but verification failed.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERIFY="$SCRIPT_DIR/verify.py"

SPEC_FILE="${1:-}"
URL="${2:-}"
SELECTORS="${3:-}"

if [[ -z "$SPEC_FILE" ]]; then
  echo "[boi-hook] ERROR: spec file argument required" >&2
  exit 2
fi

if [[ ! -f "$SPEC_FILE" ]]; then
  echo "[boi-hook] ERROR: spec file not found: $SPEC_FILE" >&2
  exit 2
fi

# ── 1. Detect web patterns in files touched by this spec ──────────────────────
WEB_FILES=()

# Collect file paths mentioned in the spec
while IFS= read -r line; do
  # Match lines referencing .py, .html, .js, .ts files
  if echo "$line" | grep -qE '\b[A-Za-z0-9_./-]+(\.py|\.html|\.js|\.ts)\b'; then
    WEB_FILES+=("$line")
  fi
done < "$SPEC_FILE"

# Also check spec content for web framework keywords
HAS_WEB_KEYWORDS=false
if grep -qE 'HTTPServer|uvicorn|flask|FastAPI|express|server\.py|index\.html' "$SPEC_FILE" 2>/dev/null; then
  HAS_WEB_KEYWORDS=true
fi

if [[ ${#WEB_FILES[@]} -eq 0 && "$HAS_WEB_KEYWORDS" == "false" ]]; then
  echo "[boi-hook] No web patterns detected — skipping E2E verification."
  exit 0
fi

echo "[boi-hook] Web patterns detected in spec."

# ── 2. Resolve URL ─────────────────────────────────────────────────────────────
if [[ -z "$URL" ]]; then
  # Try extracting URL from spec (look for --url or https:// patterns)
  URL=$(grep -oE 'https?://[A-Za-z0-9._/:?=&%-]+' "$SPEC_FILE" | head -1)
fi

if [[ -z "$URL" ]]; then
  # Try known hex-router routes
  HEX_ROUTER="${SCRIPT_DIR}/../../config/hex-router.conf"
  if [[ -f "$HEX_ROUTER" ]]; then
    URL=$(grep -oE 'https?://[A-Za-z0-9._/:?=&%-]+' "$HEX_ROUTER" | head -1)
  fi
fi

if [[ -z "$URL" ]]; then
  echo "[boi-hook] WARNING: Could not determine service URL — skipping E2E verification." >&2
  exit 0
fi

echo "[boi-hook] Testing URL: $URL"

# ── 3. Run verify.py ───────────────────────────────────────────────────────────
VERIFY_ARGS=("--url" "$URL")
[[ -n "$SELECTORS" ]] && VERIFY_ARGS+=("--selectors" "$SELECTORS")

OUTPUT_FILE="/tmp/boi-hook-e2e-report-$$.json"
VERIFY_ARGS+=("--output" "$OUTPUT_FILE")

python3 "$VERIFY" "${VERIFY_ARGS[@]}"
EXIT_CODE=$?

# ── 4. Report results ──────────────────────────────────────────────────────────
if [[ -f "$OUTPUT_FILE" ]]; then
  python3 - "$OUTPUT_FILE" <<'PYEOF'
import sys, json
with open(sys.argv[1]) as f:
    d = json.load(f)
print(f"[boi-hook] Verdict: {d['verdict']}  ({d['passed']} passed, {d['failed']} failed)")
for t in d.get("tests", []):
    status = t.get("status", "?")
    name = t.get("name", "?")
    detail = t.get("detail", "")
    screenshot = t.get("screenshot", "")
    if status == "FAIL":
        msg = f"  FAIL  {name}"
        if detail:
            msg += f" — {detail}"
        if screenshot:
            msg += f" [screenshot: {screenshot}]"
        print(msg)
PYEOF
  rm -f "$OUTPUT_FILE"
fi

if [[ $EXIT_CODE -ne 0 ]]; then
  echo "[boi-hook] E2E verification FAILED — spec cannot be marked DONE." >&2
  exit 1
fi

echo "[boi-hook] E2E verification PASSED — spec may be marked DONE."
exit 0
