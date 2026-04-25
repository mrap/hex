#!/usr/bin/env bash
# run-all.sh — Run all user-outcome metrics scripts and report PASS/FAIL.
set -uo pipefail

METRICS_DIR="$(cd "$(dirname "$0")" && pwd)"
OVERALL=0

red()   { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
bold()  { printf '\033[1m%s\033[0m\n' "$*"; }

run_metric() {
  local name="$1"
  local script="$2"
  if [ ! -f "$script" ]; then
    red "  MISSING: $name — script not found at $script"
    OVERALL=1
    return
  fi
  local out
  out=$(python3 "$script" 2>&1)
  local rc=$?
  if [ $rc -eq 0 ]; then
    green "  PASS: $name — $out"
  elif [ $rc -eq 2 ]; then
    red "  FAIL (threshold breached): $name — $out"
    OVERALL=1
  else
    red "  FAIL (script error rc=$rc): $name — $out"
    OVERALL=1
  fi
}

bold "══ User-Outcome Metrics ══"

run_metric "frustration-signals"      "$METRICS_DIR/frustration-signals.py"
run_metric "feedback-recurrence"      "$METRICS_DIR/feedback-recurrence.py"
run_metric "loop-waste-detection"     "$METRICS_DIR/loop-waste-detection.py"
run_metric "done-claim-verification"  "$METRICS_DIR/done-claim-verification.py"
run_metric "context-continuity"       "$METRICS_DIR/context-continuity.py"

echo ""
if [ $OVERALL -eq 0 ]; then
  green "Overall: PASS"
else
  red "Overall: FAIL"
fi

exit $OVERALL
