#!/usr/bin/env bash
# Deterministic consolidation — no LLM, runs nightly.
# Dedup detection, stale reference pruning, memory reindex, date normalization.
# Exit 0 on clean, exit 1 on issues found (logged to stdout for policy chaining).

set -euo pipefail

AGENT_DIR="${AGENT_DIR:-${AGENT_DIR:-$HOME/hex}}"
ISSUES=0
LOG=""

log() { LOG="$LOG\n$1"; }

# 1. Rebuild memory index (incremental)
if [ -f "$AGENT_DIR/.hex/skills/memory/scripts/memory_index.py" ]; then
  python3 "$AGENT_DIR/.hex/skills/memory/scripts/memory_index.py" 2>/dev/null && \
    log "OK: memory index rebuilt" || \
    { log "ISSUE: memory index rebuild failed"; ISSUES=$((ISSUES+1)); }
fi

# 2. Check MEMORY.md line count (cap at 200)
MEMORY_MD="$HOME/.claude/projects/-Users-mrap-hex/memory/MEMORY.md"
if [ -f "$MEMORY_MD" ]; then
  LINE_COUNT=$(wc -l < "$MEMORY_MD" | tr -d ' ')
  if [ "$LINE_COUNT" -gt 180 ]; then
    log "ISSUE: MEMORY.md at $LINE_COUNT lines (cap 200) — needs pruning"
    ISSUES=$((ISSUES+1))
  else
    log "OK: MEMORY.md at $LINE_COUNT lines"
  fi
fi

# 3. Check for stale file references in CLAUDE.md
if [ -f "$AGENT_DIR/CLAUDE.md" ]; then
  STALE=0
  while IFS= read -r ref; do
    # Extract file paths that look like relative refs
    path="$AGENT_DIR/$ref"
    if [ ! -e "$path" ]; then
      log "STALE: CLAUDE.md references missing file: $ref"
      STALE=$((STALE+1))
    fi
  done < <(grep -oP '(?<=\()(?!http)[a-zA-Z0-9_./-]+\.(?:md|yaml|py|sh|json)\)' "$AGENT_DIR/CLAUDE.md" 2>/dev/null | sed 's/)$//' || true)
  if [ "$STALE" -gt 0 ]; then
    ISSUES=$((ISSUES+STALE))
  else
    log "OK: no stale file references in CLAUDE.md"
  fi
fi

# 4. Check for duplicate memory files
if [ -d "$HOME/.claude/projects/-Users-mrap-hex/memory" ]; then
  MEM_DIR="$HOME/.claude/projects/-Users-mrap-hex/memory"
  DUPES=$(find "$MEM_DIR" -name "*.md" ! -name "MEMORY.md" -exec md5 -q {} + 2>/dev/null | sort | uniq -d | wc -l | tr -d ' ')
  if [ "$DUPES" -gt 0 ]; then
    log "ISSUE: $DUPES duplicate memory files detected"
    ISSUES=$((ISSUES+1))
  else
    log "OK: no duplicate memory files"
  fi
fi

# 5. Check todo.md for items older than 14 days without update
if [ -f "$AGENT_DIR/todo.md" ]; then
  STALE_TODOS=$(grep -cP '\d{4}-\d{2}-\d{2}' "$AGENT_DIR/todo.md" 2>/dev/null || echo 0)
  log "OK: todo.md has $STALE_TODOS dated items (age check deferred to dream agent)"
fi

# 6. Check evolution pipeline health
for f in observations.md suggestions.md changelog.md; do
  if [ ! -f "$AGENT_DIR/evolution/$f" ]; then
    log "ISSUE: missing evolution/$f"
    ISSUES=$((ISSUES+1))
  fi
done

# 7. Check for orphaned project directories (no context.md or charter.yaml)
if [ -d "$AGENT_DIR/projects" ]; then
  ORPHANS=0
  for dir in "$AGENT_DIR/projects"/*/; do
    [ -d "$dir" ] || continue
    dirname=$(basename "$dir")
    [ "$dirname" = "_archive" ] && continue
    if [ ! -f "$dir/context.md" ] && [ ! -f "$dir/charter.yaml" ] && [ ! -f "$dir/checkpoint.md" ]; then
      log "ORPHAN: projects/$dirname has no context.md, charter.yaml, or checkpoint.md"
      ORPHANS=$((ORPHANS+1))
    fi
  done
  if [ "$ORPHANS" -gt 0 ]; then
    ISSUES=$((ISSUES+ORPHANS))
  fi
fi

# 8. Run dream-cycle.py for activity summary
if [ -f "$AGENT_DIR/evolution/scripts/dream-cycle.py" ]; then
  DREAM_OUTPUT=$(python3 "$AGENT_DIR/evolution/scripts/dream-cycle.py" 2>/dev/null || echo "dream-cycle.py failed")
  log "DREAM: $DREAM_OUTPUT"
fi

# Output
NOW=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
echo "=== Consolidation Report — $NOW ==="
echo -e "$LOG"
echo ""
echo "Issues found: $ISSUES"

# Write to consolidation log
CONSOLIDATION_LOG="$AGENT_DIR/evolution/consolidation-latest.log"
{
  echo "=== Consolidation Report — $NOW ==="
  echo -e "$LOG"
  echo ""
  echo "Issues found: $ISSUES"
} > "$CONSOLIDATION_LOG"

exit $( [ "$ISSUES" -eq 0 ] && echo 0 || echo 1 )
