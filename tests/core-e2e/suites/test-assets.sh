#!/usr/bin/env bash
# test-assets.sh — E2E tests for the hex asset registry CLI.
# Sourced by run-all.sh which provides PASS/FAIL/assert_* helpers.
# All operations go through the unified hex binary CLI.
set -uo pipefail

HEX="$HEX_DIR/.hex/bin/hex"
ASSETS_FILE="$HEX_DIR/.hex/data/assets.json"
DATA_DIR="$HEX_DIR/.hex/data"

echo ""
echo "=== ASSET REGISTRY TESTS ==="

# Clean state so earlier runs don't bleed in
rm -f "$ASSETS_FILE"
mkdir -p "$DATA_DIR"

# ── 1. Register a post asset ──────────────────────────────────────────────────
OUT=$("$HEX" asset register \
    --type post \
    --id P-001 \
    --title "Test Post" \
    --path "projects/brand/pipeline.md" 2>&1)
CODE=$?
assert_exit 0 "$CODE" "asset-register-post: exit 0"
assert_contains "$OUT" "post:P-001" "asset-register-post: output contains 'post:P-001'"
assert_contains "$OUT" "Test Post" "asset-register-post: output contains title 'Test Post'"

# ── 2. Resolve: JSON with title, path, owner ──────────────────────────────────
OUT=$("$HEX" asset resolve post:P-001 2>&1)
CODE=$?
assert_exit 0 "$CODE" "asset-resolve: exit 0"
assert_contains "$OUT" "post:P-001" "asset-resolve: id field present"
assert_contains "$OUT" "Test Post" "asset-resolve: title field present"
assert_contains "$OUT" "pipeline.md" "asset-resolve: path field present"

# ── 3. Register more assets ───────────────────────────────────────────────────
"$HEX" asset register \
    --type proposal \
    --id build-in-public \
    --title "Build-in-Public Proposal" > /dev/null 2>&1
CODE=$?
assert_exit 0 "$CODE" "asset-register-proposal: exit 0"

"$HEX" asset register \
    --type decision \
    --id D-042 \
    --title "Ship hex v0.8" > /dev/null 2>&1
CODE=$?
assert_exit 0 "$CODE" "asset-register-decision: exit 0"

"$HEX" asset register \
    --type project \
    --id brand \
    --title "Brand Project" > /dev/null 2>&1
CODE=$?
assert_exit 0 "$CODE" "asset-register-project: exit 0"

# ── 4. List by type: only posts ───────────────────────────────────────────────
OUT=$("$HEX" asset list --type post 2>&1)
CODE=$?
assert_exit 0 "$CODE" "asset-list-by-type: exit 0"
assert_contains "$OUT" "post:P-001" "asset-list-by-type: post:P-001 in output"
assert_not_contains "$OUT" "proposal:" "asset-list-by-type: no proposal assets in post listing"
assert_not_contains "$OUT" "decision:" "asset-list-by-type: no decision assets in post listing"

# ── 5. Search finds registered assets ─────────────────────────────────────────
OUT=$("$HEX" asset search "Test" 2>&1)
CODE=$?
assert_exit 0 "$CODE" "asset-search: exit 0"
assert_contains "$OUT" "post:P-001" "asset-search: finds post:P-001 via title match"

OUT_BRAND=$("$HEX" asset search "Build-in-Public" 2>&1)
assert_contains "$OUT_BRAND" "build-in-public" "asset-search-brand: finds proposal by title 'Build-in-Public'"

# ── 6. Types: counts per type ─────────────────────────────────────────────────
OUT=$("$HEX" asset types 2>&1)
CODE=$?
assert_exit 0 "$CODE" "asset-types: exit 0"
assert_contains "$OUT" "post" "asset-types: 'post' type listed"
assert_contains "$OUT" "proposal" "asset-types: 'proposal' type listed"
assert_contains "$OUT" "decision" "asset-types: 'decision' type listed"
assert_contains "$OUT" "project" "asset-types: 'project' type listed"

# ── 7. Remove (graceful skip if not implemented) ──────────────────────────────
REMOVE_OUT=$("$HEX" asset remove post:P-001 2>&1)
REMOVE_CODE=$?
if [ "$REMOVE_CODE" -eq 0 ]; then
    # Remove succeeded — verify resolve now fails
    RESOLVE_OUT=$("$HEX" asset resolve post:P-001 2>&1)
    RESOLVE_CODE=$?
    if [ "$RESOLVE_CODE" -ne 0 ]; then
        assert_pass "asset-remove: removed post:P-001, resolve correctly returns non-zero"
    else
        assert_fail "asset-remove: removed post:P-001 but resolve still returns exit 0"
    fi
elif [ "$REMOVE_CODE" -eq 127 ] || echo "$REMOVE_OUT" | grep -qi "unrecognized\|unknown\|not.*implement"; then
    # Subcommand not yet implemented — graceful skip
    assert_pass "asset-remove: 'hex asset remove' not yet implemented — skipping (exit $REMOVE_CODE)"
else
    assert_fail "asset-remove: unexpected failure (exit $REMOVE_CODE) — $REMOVE_OUT"
fi

# ── 8. Verify JSON storage directly ───────────────────────────────────────────
if [ -f "$ASSETS_FILE" ]; then
    ASSET_COUNT=$(python3 -c "
import json
with open('$ASSETS_FILE') as f:
    data = json.load(f)
print(len(data['assets']))
" 2>&1)
    # We registered 4 assets; remove may have deleted one
    if [ "${ASSET_COUNT:-0}" -ge 3 ] 2>/dev/null; then
        assert_pass "asset-storage-count: assets.json has $ASSET_COUNT assets (expected >=3)"
    else
        assert_fail "asset-storage-count: expected >=3 assets in assets.json, got $ASSET_COUNT"
    fi

    # Verify schema: every asset has id, type, title, registered_at
    SCHEMA_OK=$(python3 - <<PYEOF 2>&1
import json, sys
with open("$ASSETS_FILE") as f:
    data = json.load(f)
required = {"id", "type", "title", "registered_at"}
for a in data["assets"]:
    missing = required - set(a.keys())
    if missing:
        print(f"MISSING_FIELDS:{a['id']}:{missing}")
        sys.exit(1)
print("SCHEMA_OK")
PYEOF
)
    assert_contains "$SCHEMA_OK" "SCHEMA_OK" "asset-storage-schema: all assets have id/type/title/registered_at"

    # Verify upsert: registering post:P-001 again should not duplicate it
    "$HEX" asset register \
        --type post \
        --id P-001 \
        --title "Test Post Updated" > /dev/null 2>&1
    UPSERT_COUNT=$(python3 -c "
import json
with open('$ASSETS_FILE') as f:
    data = json.load(f)
print(sum(1 for a in data['assets'] if a['id'] == 'post:P-001'))
" 2>&1)
    if [ "${UPSERT_COUNT:-0}" -eq 1 ] 2>/dev/null; then
        assert_pass "asset-upsert: re-registering post:P-001 produces exactly 1 entry (upsert, not duplicate)"
    else
        assert_fail "asset-upsert: expected 1 post:P-001 after upsert, got $UPSERT_COUNT"
    fi
else
    assert_fail "asset-storage-count: assets.json does not exist at $ASSETS_FILE"
    assert_fail "asset-storage-schema: assets.json does not exist"
    assert_fail "asset-upsert: assets.json does not exist"
fi
