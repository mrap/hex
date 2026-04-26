#!/bin/bash
# check-evolution.sh — Check evolution DB for items ready for promotion
#
# Reads evolution_db.py for items with 3+ occurrences (still open).
# If found, formats them as proposed standing orders and appends
# to evolution/suggestions.md.
#
# Usage:
#   check-evolution.sh          # Check and append suggestions
#   check-evolution.sh --dry    # Preview only, don't write

set -uo pipefail

# ─── Resolve paths ──────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
EVOLUTION_DB="$AGENT_DIR/.hex/skills/memory/scripts/evolution_db.py"
SUGGESTIONS="$AGENT_DIR/evolution/suggestions.md"

# Use configured timezone (SO #17)
if [ -z "${TZ:-}" ] && [ -f "$AGENT_DIR/.hex/timezone" ]; then
  TZ="$(tr -d '[:space:]' < "$AGENT_DIR/.hex/timezone")"; export TZ
fi

DRY_RUN=false
if [[ "${1:-}" == "--dry" ]]; then
    DRY_RUN=true
fi

# ─── Check prerequisites ───────────────────────────────────────────────────
if [[ ! -f "$EVOLUTION_DB" ]]; then
    echo "evolution_db.py not found at $EVOLUTION_DB"
    exit 1
fi

# ─── Query ready items ─────────────────────────────────────────────────────
READY_OUTPUT=$(AGENT_DIR="$AGENT_DIR" python3 "$EVOLUTION_DB" list --ready --sort occurrences 2>&1)

if echo "$READY_OUTPUT" | grep -q "No items found"; then
    echo "No items ready for promotion."
    exit 0
fi

# Parse the table output (skip header lines, extract ID and title)
ITEMS=()
while IFS= read -r line; do
    # Skip header/separator lines
    [[ "$line" =~ ^[[:space:]]*$ ]] && continue
    [[ "$line" =~ ^[[:space:]]*ID ]] && continue
    [[ "$line" =~ ^---- ]] && continue

    # Extract fields: ID, Occ, Status, Category, Title
    ID=$(echo "$line" | awk '{print $1}')
    OCC=$(echo "$line" | awk '{print $2}')
    TITLE=$(echo "$line" | sed 's/^[[:space:]]*[0-9]*[[:space:]]*[0-9]*[[:space:]]*[a-z-]*[[:space:]]*[a-z-]*[[:space:]]*//')

    # Validate we got a numeric ID
    [[ "$ID" =~ ^[0-9]+$ ]] || continue

    ITEMS+=("$ID|$OCC|$TITLE")
done <<< "$READY_OUTPUT"

if [[ ${#ITEMS[@]} -eq 0 ]]; then
    echo "No items ready for promotion."
    exit 0
fi

# ─── Format as proposed standing orders ─────────────────────────────────────
DATE=$(date +%Y-%m-%d)
NEW_SUGGESTIONS=""

for ITEM in "${ITEMS[@]}"; do
    IFS='|' read -r ID OCC TITLE <<< "$ITEM"

    # Get full details for this item
    DETAILS=$(AGENT_DIR="$AGENT_DIR" python3 "$EVOLUTION_DB" get "$ID" 2>&1)

    # Extract category and impact from details
    CATEGORY=$(echo "$DETAILS" | grep "category:" | head -1 | sed 's/.*category: //')
    IMPACT=$(echo "$DETAILS" | grep "impact:" | head -1 | sed 's/.*impact: //')

    # Check if this item is already in suggestions.md
    if grep -q "Evolution Item #${ID}" "$SUGGESTIONS" 2>/dev/null; then
        continue
    fi

    NEW_SUGGESTIONS+="
## Proposed: ${TITLE}
**Status:** proposed
**Source:** Evolution Item #${ID} (${OCC} occurrences)
**Category:** ${CATEGORY:-unknown}
**Date:** ${DATE}

**Impact:** ${IMPACT:-Not specified}

**Proposed standing order:**
> When encountering \"${TITLE}\", apply the established pattern to resolve it.

**Action required:** Review and approve, modify, or reject this suggestion.

"
done

if [[ -z "$NEW_SUGGESTIONS" ]]; then
    echo "All ready items already in suggestions.md."
    exit 0
fi

# ─── Output ─────────────────────────────────────────────────────────────────
if $DRY_RUN; then
    echo "=== Would append to $SUGGESTIONS ==="
    echo "$NEW_SUGGESTIONS"
else
    # Ensure parent directory and suggestions file exist
    mkdir -p "$(dirname "$SUGGESTIONS")"
    if [[ ! -f "$SUGGESTIONS" ]]; then
        cat > "$SUGGESTIONS" <<'HEADER'
# Suggestions

_Part of the improvement engine._
HEADER
    fi

    # Atomic write: append via temp file
    TMP=$(mktemp "${SUGGESTIONS}.tmp.XXXXXX")
    cat "$SUGGESTIONS" > "$TMP"
    echo "$NEW_SUGGESTIONS" >> "$TMP"
    mv "$TMP" "$SUGGESTIONS"
    echo "Appended ${#ITEMS[@]} suggestion(s) to suggestions.md"
fi
