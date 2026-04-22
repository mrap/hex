#!/usr/bin/env bash
# tests/probe.test.sh — REPLACE_ME probe smoke tests (mock data only, no live API)
set -uo pipefail

BUNDLE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PASS=0
FAIL=0

ok() { echo "  PASS: $1"; ((PASS++)); }
fail() { echo "  FAIL: $1"; ((FAIL++)); }

echo "=== REPLACE_ME/probe smoke tests ==="

bash -n "$BUNDLE_DIR/probe.sh" && ok "probe.sh syntax clean" || fail "probe.sh syntax error"
bash -n "$BUNDLE_DIR/maintenance/rotate.sh" && ok "rotate.sh syntax clean" || fail "rotate.sh syntax error"

for field in name description owner tier probe secrets maintenance; do
  grep -q "^$field:" "$BUNDLE_DIR/integration.yaml" \
    && ok "integration.yaml has: $field" \
    || fail "integration.yaml missing: $field"
done

echo ""
echo "Results: $PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]] && exit 0 || exit 1
