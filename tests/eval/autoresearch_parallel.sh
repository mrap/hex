#!/usr/bin/env bash
set -euo pipefail

# hex autoresearch — parallel multi-branch optimization
#
# Runs 3 focused autoresearch loops simultaneously, each in its own git worktree:
#   - Branch "events": fixes hex-events routing failures
#   - Branch "boi": fixes BOI delegation failures
#   - Branch "core": fixes persistence/onboarding/memory failures
#
# Each branch runs tournament-style mutations (--candidates N) for faster convergence.
#
# Usage:
#   bash autoresearch_parallel.sh                    # Default: 5 iterations, 3 candidates each
#   bash autoresearch_parallel.sh --iterations 10    # More iterations per branch
#   bash autoresearch_parallel.sh --candidates 5     # More candidates per tournament
#   bash autoresearch_parallel.sh --budget 20        # Budget cap per branch

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

ITERATIONS="${ITERATIONS:-5}"
CANDIDATES="${CANDIDATES:-3}"
BUDGET="${BUDGET:-15}"
MODEL="${MODEL:-sonnet}"
TIMEOUT="${TIMEOUT:-180}"

# Parse args
for arg in "$@"; do
    case "$arg" in
        --iterations=*) ITERATIONS="${arg#*=}" ;;
        --candidates=*) CANDIDATES="${arg#*=}" ;;
        --budget=*)     BUDGET="${arg#*=}" ;;
        --model=*)      MODEL="${arg#*=}" ;;
        --timeout=*)    TIMEOUT="${arg#*=}" ;;
        --dry-run)      DRY_RUN="--dry-run" ;;
    esac
done
DRY_RUN="${DRY_RUN:-}"

TIMESTAMP=$(date +%Y%m%d-%H%M%S)

echo "============================================="
echo " hex autoresearch — parallel optimization"
echo "============================================="
echo "  Branches    : events, boi, core"
echo "  Iterations  : $ITERATIONS per branch"
echo "  Candidates  : $CANDIDATES per tournament"
echo "  Budget      : \$$BUDGET per branch (\$$(( BUDGET * 3 )) total)"
echo "  Model       : $MODEL"
echo ""

# Define focus categories
# Each maps to specific eval cases
FOCUS_EVENTS="route_schedule_to_events,route_monitoring_to_events,route_reactive_to_events,hex_events_routing"
FOCUS_BOI="delegation,route_research_to_boi,route_build_to_boi"
FOCUS_CORE="persistence,onboarding,memory_search,startup_loads_context"

# Create worktrees
echo "Creating worktrees..."
cd "$REPO_DIR"

for branch in events boi core; do
    WT_PATH="/tmp/hex-ar-$branch-$TIMESTAMP"
    BRANCH_NAME="autoresearch/$branch-$TIMESTAMP"

    # Create branch from main
    git branch "$BRANCH_NAME" main 2>/dev/null || true
    git worktree add "$WT_PATH" "$BRANCH_NAME" 2>/dev/null

    echo "  $branch → $WT_PATH ($BRANCH_NAME)"
done
echo ""

# Launch parallel loops
echo "Launching 3 parallel autoresearch loops..."
echo ""

PIDS=()
LOGS=()

for branch in events boi core; do
    WT_PATH="/tmp/hex-ar-$branch-$TIMESTAMP"
    BRANCH_NAME="autoresearch/$branch-$TIMESTAMP"
    LOG="/tmp/hex-ar-$branch-$TIMESTAMP.log"

    case "$branch" in
        events) FOCUS="$FOCUS_EVENTS" ;;
        boi)    FOCUS="$FOCUS_BOI" ;;
        core)   FOCUS="$FOCUS_CORE" ;;
    esac

    echo "  Starting $branch (log: $LOG)..."

    # Run autoresearch in the worktree
    (
        cd "$WT_PATH"
        python3 tests/eval/autoresearch.py \
            --iterations "$ITERATIONS" \
            --budget "$BUDGET" \
            --model "$MODEL" \
            --timeout "$TIMEOUT" \
            --focus "$FOCUS" \
            --candidates "$CANDIDATES" \
            $DRY_RUN \
            2>&1
    ) > "$LOG" 2>&1 &

    PIDS+=($!)
    LOGS+=("$LOG")
done

echo ""
echo "All 3 branches running. PIDs: ${PIDS[*]}"
echo "Monitor:"
echo "  tail -f /tmp/hex-ar-events-$TIMESTAMP.log"
echo "  tail -f /tmp/hex-ar-boi-$TIMESTAMP.log"
echo "  tail -f /tmp/hex-ar-core-$TIMESTAMP.log"
echo ""
echo "Waiting for completion..."

# Wait for all to finish
RESULTS=()
for i in 0 1 2; do
    branch=("events" "boi" "core")
    wait "${PIDS[$i]}" 2>/dev/null
    EXIT_CODE=$?

    # Extract final score from log
    FINAL=$(grep "Final best" "${LOGS[$i]}" 2>/dev/null | tail -1 || echo "unknown")
    BASELINE=$(grep "Baseline" "${LOGS[$i]}" 2>/dev/null | head -1 || echo "unknown")

    echo "  ${branch[$i]}: exit=$EXIT_CODE | $FINAL"
    RESULTS+=("${branch[$i]}:$EXIT_CODE")
done

echo ""
echo "============================================="
echo " PARALLEL AUTORESEARCH COMPLETE"
echo "============================================="
echo ""

# Show results from each branch
for branch in events boi core; do
    LOG="/tmp/hex-ar-$branch-$TIMESTAMP.log"
    BRANCH_NAME="autoresearch/$branch-$TIMESTAMP"

    echo "--- $branch ($BRANCH_NAME) ---"
    # Show the AUTORESEARCH COMPLETE section
    sed -n '/AUTORESEARCH COMPLETE/,/^$/p' "$LOG" 2>/dev/null || echo "  (no results)"
    echo ""
done

echo "To merge improvements:"
echo "  cd $REPO_DIR"
for branch in events boi core; do
    BRANCH_NAME="autoresearch/$branch-$TIMESTAMP"
    echo "  git merge $BRANCH_NAME  # if it improved"
done

echo ""
echo "To clean up worktrees:"
for branch in events boi core; do
    WT_PATH="/tmp/hex-ar-$branch-$TIMESTAMP"
    echo "  git worktree remove $WT_PATH"
done
