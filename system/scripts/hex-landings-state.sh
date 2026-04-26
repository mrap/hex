#!/usr/bin/env bash
set -uo pipefail

AGENT_DIR="${AGENT_DIR:-/Users/mrap/mrap-hex}"
LANDINGS_FILE="${1:-${AGENT_DIR}/landings/$(date +%Y-%m-%d).md}"
BOI_DB="${HOME}/.boi/boi.db"

if [[ ! -f "$LANDINGS_FILE" ]]; then
  echo "Note: no landings file found at $LANDINGS_FILE"
  exit 0
fi

# Extract landing item boundaries (line numbers of ### Ln. headings)
landing_lines=$(grep -n "^### L[0-9]*\." "$LANDINGS_FILE" | cut -d: -f1)
total_lines=$(wc -l < "$LANDINGS_FILE")

process_block() {
  local block_text="$1"

  # Extract heading: L1. Title
  local heading
  heading=$(echo "$block_text" | grep "^### L" | head -1)
  local lnum
  lnum=$(echo "$heading" | sed 's/^### \(L[0-9]*\)\..*/\1/')
  local title
  title=$(echo "$heading" | sed 's/^### L[0-9]*\. *//')

  # Extract Status
  local status
  status=$(echo "$block_text" | grep "^\*\*Status:\*\*" | head -1 | sed 's/^\*\*Status:\*\*[[:space:]]*//')

  # Extract Weekly target
  local weekly
  weekly=$(echo "$block_text" | grep "^\*\*Weekly target:\*\*" | head -1 | sed 's/^\*\*Weekly target:\*\*[[:space:]]*//')

  # Extract table rows: pick column 2 (owner) and column 4 (status)
  # Table rows look like: | sub-item | owner | action | status |
  local owners
  owners=$(echo "$block_text" | grep "^|" | grep -v "^|[-| ]*|$" | grep -iv "sub-item" | \
    awk -F'|' 'NF>=5 { gsub(/^[[:space:]]+|[[:space:]]+$/, "", $3); print $3 }')

  # Derive State + Holder
  local status_lower
  status_lower=$(echo "$status" | tr '[:upper:]' '[:lower:]')

  local state="Unknown"
  local holder="🧑"

  case "$status_lower" in
    done*)
      state="Done"; holder="✅" ;;
    blocked*)
      state="Blocked"; holder="⚠" ;;
    *)
      # Rule 3: real person owner (not hex/boi/mike/mrap)
      local real_owners
      real_owners=$(echo "$owners" | grep -viE "^\s*$|^\s*(hex|boi|mike|mrap)\s*$" || true)
      if [[ -n "$real_owners" ]] && ! echo "$status_lower" | grep -q "^done"; then
        state="Waiting"; holder="👥"
      elif echo "$status_lower" | grep -q "^in progress"; then
        state="In Progress"; holder="🧑"
      elif echo "$status_lower" | grep -q "^not started"; then
        state="Not Started"; holder="🧑"
      fi
      ;;
  esac

  # BOI enrichment
  if [[ -f "$BOI_DB" ]] && command -v sqlite3 &>/dev/null; then
    local keyword
    keyword=$(echo "$title" | awk '{print $1, $2}' | sed "s/'//g" | sed 's/[^a-zA-Z0-9 ]//g')
    local boi_hit
    boi_hit=$(sqlite3 "$BOI_DB" "SELECT id FROM specs WHERE status IN ('running','queued') AND LOWER(title) LIKE LOWER('%${keyword}%') LIMIT 1" 2>/dev/null || true)
    if [[ -n "$boi_hit" ]]; then
      holder="🤖"
    fi
  fi

  # Output
  echo "${lnum}. ${title}"
  if [[ -n "$weekly" ]]; then
    printf "  State: %-14s Holder: %s   Weekly: %s\n" "$state" "$holder" "$weekly"
  else
    printf "  State: %-14s Holder: %s\n" "$state" "$holder"
  fi
  echo ""
}

# Process each landing block
prev_line=""
prev_num=0
while IFS= read -r line_num; do
  if [[ -n "$prev_line" ]]; then
    end_line=$((line_num - 1))
    block=$(sed -n "${prev_num},${end_line}p" "$LANDINGS_FILE")
    process_block "$block"
  fi
  prev_line="$line_num"
  prev_num="$line_num"
done <<< "$landing_lines"

# Process last block
if [[ -n "$prev_num" && "$prev_num" -gt 0 ]]; then
  block=$(sed -n "${prev_num},${total_lines}p" "$LANDINGS_FILE")
  process_block "$block"
fi

exit 0
