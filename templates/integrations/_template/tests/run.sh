#!/usr/bin/env bash
# tests/run.sh — REPLACE_ME bundle test entry point
set -uo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PASS=0
FAIL=0

for test_script in "$TESTS_DIR"/*.test.sh; do
  [[ -f "$test_script" ]] || continue
  echo "--- running $(basename "$test_script") ---"
  if bash "$test_script"; then
    ((PASS++))
  else
    ((FAIL++))
  fi
done

echo ""
echo "=== REPLACE_ME bundle tests: $PASS passed, $FAIL failed ==="
[[ $FAIL -eq 0 ]] && exit 0 || exit 1
