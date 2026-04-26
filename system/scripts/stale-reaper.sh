#!/bin/bash
# stale-reaper.sh — Scans todo.md for stale items and generates a weekly report.
#
# Categories:
#   Kill candidates:   Items with "stale since" dates > 21 days old
#   Revive candidates: Items with dates 14-21 days old that are still open
#   Active:            Items under 14 days old (skipped)
#
# Usage: bash /Users/admin/mrap-hex/.hex/scripts/stale-reaper.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT_DIR="${AGENT_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

# Use configured timezone (SO #17)
if [ -f "$AGENT_DIR/.hex/timezone" ]; then
  TZ="$(tr -d '[:space:]' < "$AGENT_DIR/.hex/timezone")"; export TZ
fi

TODAY=$(date +%Y-%m-%d)
TODAY_EPOCH=$(date -j -f "%Y-%m-%d" "$TODAY" "+%s" 2>/dev/null || date -d "$TODAY" "+%s")

TODO_FILE="$AGENT_DIR/todo.md"
REPORT_DIR="$AGENT_DIR/raw/research/stale-reaper"
REPORT_FILE="$REPORT_DIR/$TODAY.md"

mkdir -p "$REPORT_DIR"

if [ ! -f "$TODO_FILE" ]; then
  echo "Error: $TODO_FILE not found" >&2
  exit 1
fi

# Convert a YYYY-MM-DD date to epoch seconds (macOS compatible)
date_to_epoch() {
  date -j -f "%Y-%m-%d" "$1" "+%s" 2>/dev/null || date -d "$1" "+%s"
}

# Calculate days between a date string and today
days_ago() {
  local d_epoch
  d_epoch=$(date_to_epoch "$1")
  echo $(( (TODAY_EPOCH - d_epoch) / 86400 ))
}

# Safe grep that returns empty string instead of failing
sgrep() {
  grep "$@" || true
}

# Arrays for results
kill_items=()
revive_items=()
total_scanned=0

# Process todo.md line by line
while IFS= read -r line; do
  # Only look at unchecked items (open tasks)
  case "$line" in
    *'- [ ] '*) ;;
    *) continue ;;
  esac

  total_scanned=$((total_scanned + 1))

  # Extract item text (strip leading "- [ ] " and bold markers)
  item_text=$(echo "$line" | sed 's/^[[:space:]]*- \[ \] //' | sed 's/\*\*//g')
  # Truncate to first meaningful identifier (up to first em-dash or parenthesized block)
  short_name=$(echo "$item_text" | sed 's/ — .*//' | sed 's/ — .*//')

  # Check for "stale since YYYY-MM-DD"
  stale_date=$(echo "$line" | sgrep -oE 'stale since [0-9]{4}-[0-9]{2}-[0-9]{2}' | sgrep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}' | head -1)

  if [ -n "$stale_date" ]; then
    age=$(days_ago "$stale_date")
    if [ "$age" -gt 21 ]; then
      kill_items+=("- ${short_name} — stale since $stale_date, $age days ago")
    elif [ "$age" -ge 14 ]; then
      revive_items+=("- ${short_name} — last activity $stale_date, $age days ago")
    fi
    continue
  fi

  # Check for "captured YYYY-MM-DD" pattern
  captured_date=$(echo "$line" | sgrep -oE 'captured [0-9]{4}-[0-9]{2}-[0-9]{2}' | sgrep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}' | head -1)

  if [ -n "$captured_date" ]; then
    age=$(days_ago "$captured_date")
    if [ "$age" -ge 14 ] && [ "$age" -le 21 ]; then
      revive_items+=("- ${short_name} — last activity $captured_date, $age days ago")
    fi
    continue
  fi

  # Check for inline parenthesized dates like (YYYY-MM-DD)
  inline_date=$(echo "$line" | sgrep -oE '\([0-9]{4}-[0-9]{2}-[0-9]{2}\)' | tail -1 | tr -d '()')

  if [ -n "$inline_date" ]; then
    age=$(days_ago "$inline_date")
    if [ "$age" -ge 14 ] && [ "$age" -le 21 ]; then
      revive_items+=("- ${short_name} — last activity $inline_date, $age days ago")
    fi
    continue
  fi

  # Check for any remaining YYYY-MM-DD dates in the line
  any_date=$(echo "$line" | sgrep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}' | tail -1)

  if [ -n "$any_date" ]; then
    age=$(days_ago "$any_date")
    if [ "$age" -ge 14 ] && [ "$age" -le 21 ]; then
      revive_items+=("- ${short_name} — last activity $any_date, $age days ago")
    fi
    continue
  fi

done < "$TODO_FILE"

# Write report
cat > "$REPORT_FILE" << EOF
# Stale Work Report — $TODAY

## Kill Candidates (>21 days, marked stale)
EOF

if [ ${#kill_items[@]} -eq 0 ]; then
  echo "_None_" >> "$REPORT_FILE"
else
  for item in "${kill_items[@]}"; do
    echo "$item" >> "$REPORT_FILE"
  done
fi

cat >> "$REPORT_FILE" << EOF

## Revive Candidates (14-21 days)
EOF

if [ ${#revive_items[@]} -eq 0 ]; then
  echo "_None_" >> "$REPORT_FILE"
else
  for item in "${revive_items[@]}"; do
    echo "$item" >> "$REPORT_FILE"
  done
fi

cat >> "$REPORT_FILE" << EOF

## Stats
- Total items scanned: $total_scanned
- Kill candidates: ${#kill_items[@]}
- Revive candidates: ${#revive_items[@]}
EOF

echo "Report written to: $REPORT_FILE"
cat "$REPORT_FILE"
