#!/usr/bin/env bash
# Verify gate for a cleanup batch: build + test are the pass/fail gate;
# lint is reported but does NOT fail the gate. Ecosystem-detected; runs
# EVERY detected ecosystem in a polyglot repo, not just the first manifest.
# Run after EVERY batch (HARD RULE 3) — before moving to the next one.
# A fixer's exit 0 is not success; THIS script's overall exit is the gate.
#
# Why lint is report-only: build+test are regression signals (green->red means
# THIS batch broke something -> must gate). Lint on a codebase mid-cleanup is
# a TARGET, not a regression signal -- it is red at baseline (that's why the
# campaign exists), and the safe-tier autofix pass only clears part of it, so
# residual lint debt is expected to stay red for many batches. Gating on it
# would halt the campaign on a false signal on batch one. The lint delta is
# what Phase 5's re-measurement is for, not this gate.
#
# --check-batch additionally enforces HARD RULE 6 mechanically: the last
# commit must stay within the batch cap (default 250 changed lines; override
# with CLEANUP_BATCH_CAP for repos with a deliberate different cap — justify
# any override in CLEANUP-PROGRESS.md). Baseline runs (Phase 0) omit the flag.
#
# Prints every command it runs and its exit code — never swallows a failure,
# never pipes a command through tail/head before capturing its exit status
# (that loses the real exit code — a documented footgun).
#
# Meant to be EXECUTED (bash scripts/verify.sh), not just read.
#
# Usage: scripts/verify.sh [path-to-repo] [--check-batch]
# Exit: 0 if every gated step passed. Nonzero and loud otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SYSTEM_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd -P)"
MANAGED_CARGO_GATE="$SYSTEM_DIR/scripts/managed-cargo-gate.py"
[ -f "$MANAGED_CARGO_GATE" ] && [ ! -L "$MANAGED_CARGO_GATE" ] || {
  echo "ERROR: managed Cargo gate is unavailable beside this installed skill." >&2
  exit 1
}

REPO="."
CHECK_BATCH=0
for arg in "$@"; do
  case "$arg" in
    --check-batch) CHECK_BATCH=1 ;;
    *) REPO="$arg" ;;
  esac
done
cd "$REPO" || { echo "ERROR: cannot cd into $REPO" >&2; exit 1; }
REPO="$(pwd -P)"

overall_status=0
LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

run_step() {
  # $1 = label, $2 = "gate" | "report", rest = command.
  local label="$1" mode="$2"; shift 2
  echo "--- $label ---"
  echo "\$ $*"
  "$@" >"$LOG" 2>&1
  local code=$?
  # Show output AFTER capturing exit code — never pipe through tail first.
  tail -n 40 "$LOG"
  echo "exit: $code"
  if [ "$mode" = "report" ] && [ "$code" -ne 0 ]; then
    echo "(report-only: does not fail the gate — lint is a target, not a regression signal; see header)"
  fi
  echo
  if [ "$mode" = "gate" ] && [ "$code" -ne 0 ]; then
    overall_status=1
  fi
  return 0  # keep going; overall_status is the real signal
}

source_revision_context() {
  SOURCE_REVISION="unavailable"
  SOURCE_STATE="unavailable"
  if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    if SOURCE_REVISION=$(git rev-parse HEAD 2>/dev/null); then
      if [ -z "$(git status --porcelain 2>/dev/null)" ]; then
        SOURCE_STATE="clean"
      else
        SOURCE_STATE="dirty"
      fi
    fi
  fi
}

private_receipt_dir() {
  local base="$REPO/.cleanup" directory="$REPO/.cleanup/managed-cargo-receipts"
  local current mode owner previous_umask failed=0
  previous_umask=$(umask)
  umask 077
  for current in "$base" "$directory"; do
    if [ -L "$current" ]; then
      echo "ERROR: managed Cargo receipt directory must not be a symlink: $current" >&2
      failed=1
      break
    fi
    if [ ! -e "$current" ]; then
      if ! mkdir "$current"; then
        failed=1
        break
      fi
    fi
    if [ ! -d "$current" ]; then
      echo "ERROR: managed Cargo receipt path is not a directory: $current" >&2
      failed=1
      break
    fi
    if ! mode=$(stat -f '%Lp' "$current" 2>/dev/null || stat -c '%a' "$current" 2>/dev/null) || \
       ! owner=$(stat -f '%u' "$current" 2>/dev/null || stat -c '%u' "$current" 2>/dev/null); then
      failed=1
      break
    fi
    [ "$owner" = "$(id -u)" ] && (( (8#$mode & 0022) == 0 )) || {
      echo "ERROR: managed Cargo receipt directory is not private and owned by this user: $current" >&2
      failed=1
      break
    }
  done
  umask "$previous_umask"
  [ "$failed" -eq 0 ] || return 1
  MANAGED_CARGO_RECEIPTS="$directory"
}

run_managed_cargo() {
  local label="$1" mode="$2" operation="$3"; shift 3
  run_step "$label" "$mode" python3 "$MANAGED_CARGO_GATE" \
    --caller repo-cleanup --source-revision "$SOURCE_REVISION" --source-state "$SOURCE_STATE" \
    --receipt-dir "$MANAGED_CARGO_RECEIPTS" "$operation" "$@"
}

# --- HARD RULE 6 mechanical check (only with --check-batch) ---
if [ "$CHECK_BATCH" -eq 1 ]; then
  echo "--- batch-size check (HARD RULE 6) ---"
  cap="${CLEANUP_BATCH_CAP:-250}"
  if git rev-parse HEAD~1 >/dev/null 2>&1; then
    changed=$(git diff --numstat HEAD~1..HEAD | awk '{a+=$1+$2} END {print a+0}')
    echo "last commit: $changed changed lines (cap: $cap)"
    if [ "$changed" -gt "$cap" ]; then
      echo "FAIL: last commit exceeds the batch cap — split it before continuing."
      overall_status=1
    fi
  else
    echo "no parent commit to diff against — skipping (first commit in history)"
  fi
  echo
fi

ran_anything=0

if [ -f Cargo.toml ]; then
  ran_anything=1
  source_revision_context
  if private_receipt_dir; then
    run_managed_cargo "cargo build" gate build --all-targets
    run_managed_cargo "cargo test" gate test
    run_managed_cargo "cargo clippy (report-only)" report clippy --all-targets
  else
    overall_status=1
  fi
fi

if [ -f package.json ]; then
  ran_anything=1
  if command -v npm >/dev/null 2>&1; then
    run_step "npm run build (if defined)" gate npm run --if-present build
    run_step "npm test" gate npm test
    run_step "npm run lint (report-only)" report npm run --if-present lint
  else
    echo "ERROR: package.json found but npm is unavailable; tests did not run." >&2
    overall_status=1
  fi
fi

if [ -f pyproject.toml ] || [ -f setup.py ]; then
  ran_anything=1
  command -v ruff >/dev/null 2>&1 && run_step "ruff check (report-only)" report ruff check .
  if command -v pytest >/dev/null 2>&1; then
    run_step "pytest" gate pytest -q
  else
    echo "ERROR: Python project found but pytest is unavailable; tests did not run." >&2
    overall_status=1
  fi
fi

if [ -f go.mod ]; then
  ran_anything=1
  run_step "go build" gate go build ./...
  run_step "go test" gate go test ./...
  command -v golangci-lint >/dev/null 2>&1 && run_step "golangci-lint (report-only)" report golangci-lint run
fi

if [ "$ran_anything" -eq 0 ]; then
  echo "No recognized manifest (Cargo.toml/package.json/pyproject.toml/setup.py/go.mod) found."
  echo "This is a loud unknown, not a silent pass — supply the repo's real build/test commands"
  echo "manually; an unrecognized ecosystem is never a green gate."
  exit 1
fi

echo "=== Verify gate: $([ "$overall_status" -eq 0 ] && echo PASS || echo FAIL) ==="
exit "$overall_status"
