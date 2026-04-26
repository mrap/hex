#!/usr/bin/env bash
# weekly-synthesis-digest.sh — Summarize the week's input compounding pipeline output.
#
# Scans captures, dispatched BOI specs, synthesis outputs, and agent trails from
# the past 7 days, then emits a signal-focused digest to landings/weekly/.
#
# Usage:
#   weekly-synthesis-digest.sh              # only runs on Fridays (by default)
#   weekly-synthesis-digest.sh --force      # run regardless of day
#   weekly-synthesis-digest.sh --dry-run    # print digest to stdout, don't write file

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="$(cd "$SCRIPT_DIR/../.." && pwd)"
CAPTURES_DIR="$WORKSPACE/raw/captures"
RESEARCH_DIR="$WORKSPACE/raw/research"
PROJECTS_DIR="$WORKSPACE/projects"
LANDINGS_DIR="$WORKSPACE/landings/weekly"
BOI_QUEUE="$HOME/.boi/queue"
SLACK_SCRIPT="$SCRIPT_DIR/slack-post.sh"
SLACK_CHANNEL="${HEX_MAIN_CHANNEL:-C0AQZR31EET}"

FORCE=false
DRY_RUN=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --force)    FORCE=true;   shift ;;
    --dry-run)  DRY_RUN=true; shift ;;
    *) echo "Unknown arg: $1"; exit 1 ;;
  esac
done

# Only run on Fridays unless forced
DOW=$(date +%u)  # 1=Mon … 5=Fri … 7=Sun
if [ "$FORCE" = false ] && [ "$DOW" != "5" ]; then
  echo "[weekly-synthesis-digest] Not Friday (DOW=$DOW) — skipping. Use --force to override."
  exit 0
fi

# Date math
TODAY=$(date '+%Y-%m-%d')
WEEK_NUM=$(date '+%V')
YEAR=$(date '+%Y')
WEEK_AGO=$(date -v-7d '+%Y-%m-%d' 2>/dev/null || date -d '7 days ago' '+%Y-%m-%d')

mkdir -p "$LANDINGS_DIR"

echo "=== weekly-synthesis-digest ==="
echo "Period: $WEEK_AGO → $TODAY  (W$WEEK_NUM)"
echo ""

# ── 1. Count captures received this week ─────────────────────────────────────
captures_total=0
captures_dispatched=0
captures_list=""
if [ -d "$CAPTURES_DIR" ]; then
  while IFS= read -r f; do
    fname="$(basename "$f")"
    # filename date is the first 10 chars: YYYY-MM-DD
    fdate="${fname:0:10}"
    # skip non-dated files
    if ! echo "$fdate" | grep -qE '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'; then
      continue
    fi
    if [[ "$fdate" > "$WEEK_AGO" || "$fdate" = "$WEEK_AGO" ]]; then
      captures_total=$((captures_total + 1))
      title="$(grep '^title:' "$f" 2>/dev/null | head -1 | sed 's/^title:[[:space:]]*//' || echo "$fname")"
      [ -z "$title" ] && title="$fname"
      if grep -q '^dispatched:' "$f" 2>/dev/null; then
        captures_dispatched=$((captures_dispatched + 1))
        captures_list+="  - [dispatched] $title"$'\n'
      else
        captures_list+="  - [queued]     $title"$'\n'
      fi
    fi
  done < <(find "$CAPTURES_DIR" -maxdepth 1 -name "*.md" ! -name "TRIAGE-*" 2>/dev/null | sort)
fi

# ── 2. BOI specs that originated from captures (have "Source capture:" line) ──
boi_from_captures=0
boi_completed=0
boi_list=""
if [ -d "$BOI_QUEUE" ]; then
  while IFS= read -r f; do
    if grep -q 'Source capture:' "$f" 2>/dev/null; then
      boi_from_captures=$((boi_from_captures + 1))
      spec_title="$(head -1 "$f" | sed 's/^# //')"
      status_line="$(grep -E '^(DONE|PENDING|FAILED)' "$f" 2>/dev/null | head -1 || echo "PENDING")"
      if echo "$status_line" | grep -q 'DONE'; then
        boi_completed=$((boi_completed + 1))
        boi_list+="  - [done]    $spec_title"$'\n'
      else
        boi_list+="  - [active]  $spec_title"$'\n'
      fi
    fi
  done < <(find "$BOI_QUEUE" -maxdepth 1 -name "*.spec.md" -newer "$CAPTURES_DIR" 2>/dev/null | sort)
fi

# ── 3. Synthesis outputs produced this week ───────────────────────────────────
synthesis_count=0
synthesis_list=""
if [ -d "$RESEARCH_DIR" ]; then
  while IFS= read -r f; do
    synthesis_count=$((synthesis_count + 1))
    synthesis_title="$(basename "$f" .md)"
    synthesis_list+="  - $synthesis_title"$'\n'
  done < <(find "$RESEARCH_DIR" -maxdepth 1 -name "*-synthesis-*.md" -newer "$WORKSPACE/raw/captures" 2>/dev/null | sort)
fi

# ── 4. Agent trail entries referencing input-derived work ─────────────────────
agent_trail_count=0
trail_list=""
while IFS= read -r trail_f; do
  while IFS= read -r line; do
    if echo "$line" | grep -qiE 'capture|synthesis|dispatch|source_capture'; then
      agent_trail_count=$((agent_trail_count + 1))
      project="$(basename "$(dirname "$trail_f")")"
      trail_list+="  - [$project] $line"$'\n'
      break  # one match per trail file
    fi
  done < "$trail_f"
done < <(find "$PROJECTS_DIR" -name "trail.md" -newer "$WORKSPACE/raw/captures" 2>/dev/null 2>&1)

# ── 5. Open threads (captures not yet dispatched) ─────────────────────────────
open_count=$((captures_total - captures_dispatched))

# ── 6. Build digest ───────────────────────────────────────────────────────────
DIGEST=$(cat <<DIGEST_EOF
---
generated: $TODAY
week: ${YEAR}-W${WEEK_NUM}
period: $WEEK_AGO to $TODAY
---

# Input Compounding Digest — W${WEEK_NUM}

> This week you fed hex **${captures_total}** input(s).
> ${captures_dispatched} triggered BOI specs. ${synthesis_count} led to synthesis outputs.
> ${open_count} remain open threads.

## Inputs Received (${captures_total})

${captures_list:-  _(none this week)_}

## BOI Specs Dispatched from Captures (${boi_from_captures})

${boi_list:-  _(none this week)_}

## Synthesis Outputs Produced (${synthesis_count})

${synthesis_list:-  _(none this week)_}

## Agent Trail — Input-Derived Mentions (${agent_trail_count})

${trail_list:-  _(none this week)_}

## Open Threads (${open_count})

$(if [ "$open_count" -gt 0 ]; then
  echo "$(echo "$captures_list" | grep '\[queued\]')"
else
  echo "  _(all captures actioned)_"
fi)
DIGEST_EOF
)

# ── 7. Write or print ─────────────────────────────────────────────────────────
DIGEST_FILE="$LANDINGS_DIR/synthesis-${YEAR}-W${WEEK_NUM}.md"

if [ "$DRY_RUN" = true ]; then
  echo "$DIGEST"
  echo ""
  echo "[dry-run] Would write to: $DIGEST_FILE"
  exit 0
fi

# Atomic write
TMP_FILE="$(mktemp)"
echo "$DIGEST" > "$TMP_FILE"
mv "$TMP_FILE" "$DIGEST_FILE"
echo "Digest written: $DIGEST_FILE"

# ── 8. Slack summary ──────────────────────────────────────────────────────────
SLACK_MSG="*Hex W${WEEK_NUM} Synthesis Digest*
You fed hex ${captures_total} input(s) this week. ${captures_dispatched} triggered BOI specs. ${synthesis_count} led to synthesis outputs. ${open_count} open threads remain.
Full digest: \`landings/weekly/synthesis-${YEAR}-W${WEEK_NUM}.md\`"

if [ -f "$SLACK_SCRIPT" ]; then
  echo "[weekly-synthesis-digest] Posting summary to Slack..."
  if bash "$SLACK_SCRIPT" --channel "$SLACK_CHANNEL" --text "$SLACK_MSG" 2>&1; then
    echo "[weekly-synthesis-digest] Slack post ok"
  else
    echo "[weekly-synthesis-digest] Slack post failed (non-fatal)"
  fi
else
  echo "[weekly-synthesis-digest] slack-post.sh not found — skipping Slack"
fi

echo ""
echo "=== Done ==="
echo "  Captures:   $captures_total received, $captures_dispatched dispatched"
echo "  BOI specs:  $boi_from_captures from captures ($boi_completed completed)"
echo "  Synthesis:  $synthesis_count outputs"
echo "  Open:       $open_count threads"
