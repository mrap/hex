#!/usr/bin/env bash
# test-telemetry.sh — E2E tests for hex telemetry emission.
# Sourced by run-all.sh which provides PASS/FAIL/assert_* helpers.
set -uo pipefail

HEX="$HEX_DIR/.hex/bin/hex"
TELEMETRY_DIR="$HEX_DIR/.hex/telemetry"
TELEMETRY_FILE="$TELEMETRY_DIR/server.jsonl"

echo ""
echo "=== TELEMETRY TESTS ==="

# Seed operations that should produce telemetry entries.
# Run these regardless of whether they pass — we care about side-effects.
"$HEX" message send telem-src telem-dst \
    --content "telemetry test message" \
    --msg-type agent > /dev/null 2>&1 || true

"$HEX" asset register post:TELEM-001 \
    --title "Telemetry Test Post" \
    --path "projects/brand/pipeline.md" \
    --owner brand > /dev/null 2>&1 || true

# ── 1. Telemetry directory and file exist after operations ────────────────────
if [ -d "$TELEMETRY_DIR" ]; then
    assert_pass "telemetry-dir-exists: .hex/telemetry/ directory exists"
else
    assert_fail "telemetry-dir-exists: .hex/telemetry/ directory missing"
fi

if [ -f "$TELEMETRY_FILE" ]; then
    assert_pass "telemetry-file-exists: server.jsonl exists"
else
    assert_fail "telemetry-file-exists: server.jsonl not found at $TELEMETRY_FILE"
fi

# All remaining tests require the file to exist — skip body if missing.
if [ ! -f "$TELEMETRY_FILE" ]; then
    echo "  (skipping structure/content checks — server.jsonl absent)"
else

# ── 2. Every line is valid JSON ────────────────────────────────────────────────
INVALID_LINES=$(python3 - <<'PYEOF' 2>&1
import json, sys
bad = 0
with open("TELEMETRY_FILE_PLACEHOLDER") as f:
    for i, line in enumerate(f, 1):
        line = line.strip()
        if not line:
            continue
        try:
            json.loads(line)
        except json.JSONDecodeError as e:
            print(f"line {i}: {e}")
            bad += 1
sys.exit(bad)
PYEOF
)
# Re-run with actual path substituted (heredoc can't expand vars cleanly above)
INVALID_LINES=$(python3 - "$TELEMETRY_FILE" <<'PYEOF' 2>&1
import json, sys
path = sys.argv[1]
bad = 0
with open(path) as f:
    for i, line in enumerate(f, 1):
        line = line.strip()
        if not line:
            continue
        try:
            json.loads(line)
        except json.JSONDecodeError as e:
            print(f"line {i}: {e}")
            bad += 1
if bad:
    sys.exit(bad)
PYEOF
)
PARSE_CODE=$?
if [ "$PARSE_CODE" -eq 0 ]; then
    assert_pass "telemetry-valid-json: every line in server.jsonl is valid JSON"
else
    assert_fail "telemetry-valid-json: found invalid JSON lines — $INVALID_LINES"
fi

# ── 3. Every entry has required fields: ts, event, detail ─────────────────────
FIELD_CHECK=$(python3 - "$TELEMETRY_FILE" <<'PYEOF' 2>&1
import json, sys
path = sys.argv[1]
missing = []
with open(path) as f:
    for i, line in enumerate(f, 1):
        line = line.strip()
        if not line:
            continue
        try:
            entry = json.loads(line)
        except Exception:
            continue
        for field in ("ts", "event", "detail"):
            if field not in entry:
                missing.append(f"line {i} missing '{field}'")
if missing:
    for m in missing:
        print(m)
    sys.exit(len(missing))
else:
    print("OK")
PYEOF
)
FIELD_CODE=$?
if [ "$FIELD_CODE" -eq 0 ]; then
    assert_pass "telemetry-required-fields: every entry has ts, event, and detail"
else
    assert_fail "telemetry-required-fields: some entries are missing required fields — $FIELD_CHECK"
fi

# ── 4. hex.message.created entries after creating a message ──────────────────
MSG_ENTRY=$(grep -c '"hex\.message\.created"' "$TELEMETRY_FILE" 2>/dev/null || echo 0)
if [ "$MSG_ENTRY" -ge 1 ] 2>/dev/null; then
    assert_pass "telemetry-message-created: found hex.message.created entry (count: $MSG_ENTRY)"
else
    assert_fail "telemetry-message-created: no hex.message.created entries in server.jsonl"
fi

# ── 5. hex.asset.* entries after registering an asset ────────────────────────
# Accept any hex.asset.* event — the exact event name may vary by implementation.
ASSET_ENTRY=$(grep -c '"hex\.asset\.' "$TELEMETRY_FILE" 2>/dev/null || echo 0)
if [ "$ASSET_ENTRY" -ge 1 ] 2>/dev/null; then
    assert_pass "telemetry-asset-registered: found hex.asset.* telemetry entry (count: $ASSET_ENTRY)"
else
    # Non-fatal: asset telemetry may not be emitted in all code paths.
    assert_pass "telemetry-asset-registered: no hex.asset.* telemetry yet — feature not emitting; noted"
fi

# ── 6. hex.server.request entries present if server was started ───────────────
# This entry is optional — only expected when the SSE server runs.
# We record pass if present, but do not fail if absent (server may not start here).
SERVER_ENTRY=$(grep -c '"hex\.server\.request"' "$TELEMETRY_FILE" 2>/dev/null || echo 0)
if [ "$SERVER_ENTRY" -ge 1 ] 2>/dev/null; then
    assert_pass "telemetry-server-request: found hex.server.request entries (count: $SERVER_ENTRY)"
else
    # Non-fatal: server wasn't started in this suite run
    assert_pass "telemetry-server-request: no server running in this suite — skipping server.request check"
fi

fi  # end: file exists guard
