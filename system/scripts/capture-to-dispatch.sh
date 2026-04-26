#!/usr/bin/env bash
# capture-to-dispatch: Scan triaged captures, generate BOI specs, and dispatch.
#
# Closes the "dispatch gap": captures get triaged but never dispatched to BOI.
# This script finds actionable captures listed in the latest TRIAGE report,
# generates a minimal BOI spec for each, validates it, and dispatches.
#
# Usage:
#   capture-to-dispatch.sh                  # dispatch up to 3 captures
#   capture-to-dispatch.sh --dry-run        # show what would be dispatched
#   capture-to-dispatch.sh --max 5          # override rate limit (default: 3)
#   capture-to-dispatch.sh --triage FILE    # use a specific triage report

set -euo pipefail

# --- Config ---
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="$(cd "$SCRIPT_DIR/../.." && pwd)"
CAPTURES_DIR="$WORKSPACE/raw/captures"
SPEC_STAGING_DIR="$WORKSPACE/raw/captures/.dispatch-staging"
VALIDATOR="$WORKSPACE/.hex/scripts/validate-boi-spec.py"
BOI="$HOME/.boi/boi"
MAX_DISPATCHES=3
DRY_RUN=false
TRIAGE_FILE=""

# Timezone from .hex/timezone (SO #17)
if [ -z "${TZ:-}" ] && [ -f "$WORKSPACE/.hex/timezone" ]; then
  TZ="$(tr -d '[:space:]' < "$WORKSPACE/.hex/timezone")"; export TZ
fi
TODAY="$(date '+%Y-%m-%d')"

# --- Parse args ---
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)  DRY_RUN=true; shift ;;
    --max)      MAX_DISPATCHES="$2"; shift 2 ;;
    --triage)   TRIAGE_FILE="$2"; shift 2 ;;
    *)          echo "Unknown arg: $1"; exit 1 ;;
  esac
done

# --- Find triage report ---
if [ -z "$TRIAGE_FILE" ]; then
  # Find the most recent TRIAGE-*.md file
  TRIAGE_FILE="$(ls -t "$CAPTURES_DIR"/TRIAGE-*.md 2>/dev/null | head -1)"
  if [ -z "$TRIAGE_FILE" ]; then
    echo "No TRIAGE report found in $CAPTURES_DIR"
    exit 0
  fi
fi

if [ ! -f "$TRIAGE_FILE" ]; then
  echo "Triage file not found: $TRIAGE_FILE"
  exit 1
fi

echo "=== capture-to-dispatch ==="
echo "Triage report: $(basename "$TRIAGE_FILE")"
echo "Max dispatches: $MAX_DISPATCHES"
echo "Dry run: $DRY_RUN"
echo ""

# --- Extract actionable capture filenames + titles from triage report ---
# The triage report lists captures like:
#   1. **BOI status sortable + last updated** (`2026-03-13_22-25-23.md`)
# We extract filenames and bold titles from the Actionable section.
# Store filename->title mappings in a temp file (macOS bash 3 lacks associative arrays)
TRIAGE_TITLES_FILE="$(mktemp)"
trap 'rm -f "$TRIAGE_TITLES_FILE"' EXIT
ACTIONABLE_FILES=()
in_actionable=false
while IFS= read -r line; do
  # Detect start of actionable section
  if echo "$line" | grep -qE '## Actionable Items'; then
    in_actionable=true
    continue
  fi
  # Detect end of actionable section (next ## heading that isn't a sub-section)
  if $in_actionable && echo "$line" | grep -qE '^## [^A]'; then
    in_actionable=false
    continue
  fi
  # Extract capture filenames and bold titles
  if $in_actionable; then
    # Try to extract a bold title: **Some Title** before the filename
    triage_title=""
    if [[ "$line" =~ \*\*([^*]+)\*\* ]]; then
      triage_title="${BASH_REMATCH[1]}"
    fi
    # Match patterns like (`2026-03-13_22-25-23.md`) or (`filename.md` + `other.md`)
    remaining="$line"
    while [[ "$remaining" =~ \`([0-9]{4}-[0-9]{2}-[0-9]{2}_[0-9]{2}-[0-9]{2}-[0-9]{2}[^\.]*\.md)\` ]]; do
      fname="${BASH_REMATCH[1]}"
      remaining="${remaining#*"${BASH_REMATCH[0]}"}"
      ACTIONABLE_FILES+=("$fname")
      if [ -n "$triage_title" ]; then
        echo "${fname}	${triage_title}" >> "$TRIAGE_TITLES_FILE"
      fi
    done
  fi
done < "$TRIAGE_FILE"

# Deduplicate
ACTIONABLE_FILES=($(printf '%s\n' "${ACTIONABLE_FILES[@]}" | sort -u))

echo "Found ${#ACTIONABLE_FILES[@]} actionable captures in triage report"
echo ""

# --- Filter: skip already-dispatched captures ---
PENDING_FILES=()
for fname in "${ACTIONABLE_FILES[@]}"; do
  filepath="$CAPTURES_DIR/$fname"
  if [ ! -f "$filepath" ]; then
    echo "  SKIP (not found): $fname"
    continue
  fi
  if grep -q '^dispatched:' "$filepath" 2>/dev/null; then
    echo "  SKIP (already dispatched via tag): $fname"
    continue
  fi
  # Check BOI DB for any active spec referencing this capture (authoritative source of truth)
  if sqlite3 "$HOME/.boi/boi.db" \
    "SELECT 1 FROM specs WHERE spec_path LIKE '%${fname}%' AND status NOT IN ('canceled','failed') LIMIT 1;" 2>/dev/null | grep -q 1; then
    echo "  SKIP (active BOI spec exists in DB): $fname"
    echo "dispatched: $(date '+%Y-%m-%d')" >> "$filepath"
    continue
  fi
  # Skip captures older than 7 days (stale captures burn tokens on irrelevant work)
  capture_date="${fname:0:10}"  # extract YYYY-MM-DD from filename
  if [ -n "$capture_date" ]; then
    capture_epoch=$(date -j -f "%Y-%m-%d" "$capture_date" "+%s" 2>/dev/null || echo "0")
    now_epoch=$(date "+%s")
    age_days=$(( (now_epoch - capture_epoch) / 86400 ))
    if [ "$age_days" -gt 7 ]; then
      echo "  SKIP (stale, ${age_days}d old): $fname"
      continue
    fi
  fi
  PENDING_FILES+=("$fname")
done

echo ""
echo "${#PENDING_FILES[@]} captures pending dispatch"

if [ ${#PENDING_FILES[@]} -eq 0 ]; then
  echo "Nothing to dispatch."
  exit 0
fi

# --- Create staging dir ---
mkdir -p "$SPEC_STAGING_DIR"

# --- Generate and dispatch specs ---
dispatched=0
for fname in "${PENDING_FILES[@]}"; do
  if [ "$dispatched" -ge "$MAX_DISPATCHES" ]; then
    echo ""
    echo "Rate limit reached ($MAX_DISPATCHES). Remaining captures deferred to next run."
    break
  fi

  filepath="$CAPTURES_DIR/$fname"

  # Extract the capture content (everything after the YAML frontmatter)
  content=""
  in_frontmatter=false
  past_frontmatter=false
  while IFS= read -r line; do
    if [ "$past_frontmatter" = true ]; then
      content+="$line"$'\n'
    elif [ "$line" = "---" ] && [ "$in_frontmatter" = false ]; then
      in_frontmatter=true
    elif [ "$line" = "---" ] && [ "$in_frontmatter" = true ]; then
      past_frontmatter=true
    fi
  done < "$filepath"

  # Trim leading/trailing whitespace
  content="$(echo "$content" | sed -e 's/^[[:space:]]*//' -e '/^$/d')"

  if [ -z "$content" ]; then
    echo "  SKIP (empty content): $fname"
    continue
  fi

  # Derive title: prefer triage report title > routed_to > first line of content
  title=""

  # 1. Title from triage report (best: human-curated during triage)
  triage_match="$(grep "^${fname}	" "$TRIAGE_TITLES_FILE" 2>/dev/null | head -1 | cut -f2-)"
  if [ -n "$triage_match" ]; then
    title="$triage_match"
  fi

  # 2. Fallback: routed_to from capture frontmatter
  if [ -z "$title" ] && grep -q '^routed_to:' "$filepath" 2>/dev/null; then
    title="$(grep '^routed_to:' "$filepath" | sed 's/^routed_to:[[:space:]]*//' | sed 's/^"//;s/"$//' | sed 's/^todo\.md (//' | sed 's/)$//')"
  fi

  # 3. Last resort: first line of content (truncated)
  if [ -z "$title" ]; then
    title="$(echo "$content" | head -1 | cut -c1-80)"
  fi

  # Clean title for use as filename
  spec_basename="$(echo "$title" | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9]/-/g' | sed 's/--*/-/g' | sed 's/^-//;s/-$//' | cut -c1-60)"
  spec_path="$SPEC_STAGING_DIR/${spec_basename}.spec.md"

  # Generate BOI spec
  cat > "$spec_path" <<SPEC_EOF
# ${title}

**Mode:** execute
**Workspace:** worktree

## Context

Source capture: \`${filepath}\`
Captured content:
> ${content}

## Tasks

### t-1: Implement ${title}
PENDING

**Spec:** ${content}

Use absolute paths. Target repository should be determined from the task context.

**Verify:**
\`\`\`bash
echo "Task completed: ${title}"
\`\`\`

**Self-evolution:** If the task requires multiple steps, decompose into additional tasks before proceeding. If the task is research-only, produce a written report at \`${WORKSPACE}/raw/research/${spec_basename}.md\` and verify with \`test -f ${WORKSPACE}/raw/research/${spec_basename}.md\`.
SPEC_EOF

  echo ""
  echo "--- [$((dispatched + 1))/$MAX_DISPATCHES] $fname ---"
  echo "  Title: $title"
  echo "  Spec:  $(basename "$spec_path")"

  # Validate spec
  if [ -f "$VALIDATOR" ]; then
    if ! python3 "$VALIDATOR" "$spec_path" 2>&1; then
      echo "  VALIDATION FAILED -- skipping"
      rm -f "$spec_path"
      continue
    fi
  fi

  # Dispatch or dry-run
  if [ "$DRY_RUN" = true ]; then
    echo "  [DRY RUN] Would dispatch: $spec_path"
  else
    echo "  Dispatching..."
    if bash "$BOI" dispatch "$spec_path" --no-critic 2>&1; then
      echo "  Dispatched successfully."
      # Mark capture as dispatched
      echo "dispatched: $TODAY" >> "$filepath"
      # Emit telemetry: capture dispatched to BOI spec
      spec_id="$(basename "$spec_path" .spec.md)"
      "$WORKSPACE/.hex/bin/hex-emit.sh" "capture.dispatched" \
        "{\"capture_path\":\"$filepath\",\"spec_id\":\"$spec_id\"}" \
        "capture-to-dispatch" || true
    else
      echo "  DISPATCH FAILED"
      continue
    fi
  fi

  dispatched=$((dispatched + 1))
done

echo ""
echo "=== Done: $dispatched dispatched (of ${#PENDING_FILES[@]} pending) ==="
