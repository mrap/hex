#!/bin/bash
# Daily Landings Dashboard — narrow-width view for tmux pane
# Optimized for 60-char wide displays. No wrapping.
# Usage: bash landings-dashboard.sh --watch
#    or: watch -t -n 2 -c bash landings-dashboard.sh

# Auto-detect AGENT_DIR: walk up from script location to find CLAUDE.md
# Resolve before any cd changes the working directory
if [ -z "${AGENT_DIR:-}" ]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  candidate="$SCRIPT_DIR"
  while [ "$candidate" != "/" ]; do
    if [ -f "$candidate/CLAUDE.md" ]; then
      AGENT_DIR="$candidate"
      break
    fi
    candidate="$(dirname "$candidate")"
  done
  AGENT_DIR="${AGENT_DIR:-$HOME/hex}"
fi

# Use configured timezone from .hex/timezone (if set)
if [ -z "${TZ:-}" ] && [ -f "$AGENT_DIR/.hex/timezone" ]; then
  TZ="$(cat "$AGENT_DIR/.hex/timezone" | tr -d '[:space:]')"; export TZ
fi

TODAY=$(date +%Y-%m-%d)
FILE="$AGENT_DIR/landings/$TODAY.md"
MAX=50

# Colors — ANSI codes
BOLD="\033[1m"
DIM="\033[2m"
GREEN="\033[32m"
YELLOW="\033[33m"
RED="\033[31m"
CYAN="\033[36m"
MAUVE="\033[35m"
LAVENDER="\033[94m"
TEAL="\033[36m"
TEXT="\033[90m"
RESET="\033[0m"

if [[ "${1:-}" == "--watch" ]]; then
  exec watch -t -n 2 -c "AGENT_DIR='$AGENT_DIR' bash '$0'"
fi

# Truncate helper
trunc() {
  local text="$1" max="$2"
  if [[ ${#text} -gt $max ]]; then
    echo "${text:0:$((max-1))}…"
  else
    echo "$text"
  fi
}

# Toggle: set to "true" to show weekly targets
SHOW_WEEKLY="${SHOW_WEEKLY:-false}"

# Header
echo -e "${BOLD}${MAUVE}═══ LANDINGS $(date +%a\ %b\ %d) ═══${RESET}"

# Current mission
MISSION_FILE="$AGENT_DIR/.hex/mission"
if [[ -f "$MISSION_FILE" ]]; then
  MISSION=$(head -1 "$MISSION_FILE")
  if [[ -n "$MISSION" ]]; then
    echo -e "${BOLD}${CYAN}► ${MISSION}${RESET}"
  fi
fi
echo ""

# --- Weekly Targets ---
if [[ "$SHOW_WEEKLY" == "true" ]]; then
  WEEK_NUM=$(date +%V)
  YEAR=$(date +%G)
  WEEKLY_FILE="$AGENT_DIR/landings/weekly/${YEAR}-W${WEEK_NUM}.md"

  if [[ -f "$WEEKLY_FILE" ]]; then
    echo -e "${LAVENDER}── W${WEEK_NUM} Targets ──${RESET}"

    w_name=""
    while IFS= read -r line; do
      if [[ "$line" =~ ^###\ W([0-9]+)\. ]]; then
        w_id="W${BASH_REMATCH[1]}"
        w_name=$(echo "$line" | sed 's/^### W[0-9]*\. //')
        w_status=""
      fi
      if [[ "$line" =~ ^\*\*Status:\*\* ]]; then
        w_status=$(echo "$line" | sed 's/\*\*Status:\*\* //')
        case "$w_status" in
          *Done*|*done*|*Code-Complete*) w_color=$GREEN; w_icon="✓" ;;
          *Risk*|*risk*|*Stalled*|*stalled*) w_color=$RED; w_icon="!" ;;
          *Progress*|*progress*) w_color=$TEAL; w_icon="▸" ;;
          *Deprioritized*|*deprioritized*) w_color=$DIM; w_icon="–" ;;
          *) w_color=$DIM; w_icon="○" ;;
        esac
        tag=""
        case "$w_status" in
          *Code-Complete*) tag="done" ;;
          *In\ Progress*)  tag="prog" ;;
          *Stalled*)       tag="stall" ;;
          *Deprioritized*) tag="depri" ;;
          *Not\ Started*)  tag="" ;;
        esac
        w_short=$(trunc "${w_name:-}" 40)
        if [[ -n "$tag" ]]; then
          printf " ${w_color}${w_icon}${RESET} ${DIM}%s${RESET} %s ${DIM}%s${RESET}\n" "$w_id" "$w_short" "$tag"
        else
          printf " ${w_color}${w_icon}${RESET} ${DIM}%s${RESET} %s\n" "$w_id" "$w_short"
        fi
      fi
    done < "$WEEKLY_FILE"

    echo ""
  fi
fi

HAS_LANDINGS=false
if [[ -f "$FILE" ]]; then
  HAS_LANDINGS=true
fi

if [[ "$HAS_LANDINGS" == false ]]; then
  echo -e "${DIM}No landings set yet.${RESET}"
fi

if [[ "$HAS_LANDINGS" == true ]]; then
# --- Landings ---
echo -e "${LAVENDER}── Landings ──${RESET}"

in_landing=false
landing_name=""
landing_id=""
status_text=""
weekly_target=""
holder_text=""
sub_done=0
sub_total=0

print_landing() {
  if [[ -z "$landing_name" ]]; then return; fi

  progress=""
  if [[ $sub_total -gt 0 ]]; then
    progress="$sub_done/$sub_total"
  fi

  case "$status_text" in
    *Done*|*done*|*Complete*|*complete*)
      color=$GREEN; icon="✓" ;;
    *Waiting*|*waiting*)
      color=$YELLOW; icon="◷" ;;
    *Blocked*)
      color=$RED; icon="✗" ;;
    *Not\ Started*|*not\ started*)
      color=$DIM; icon="○" ;;
    *)
      color=$TEAL; icon="▸" ;;
  esac

  name_display=$(trunc "$landing_name" 40)
  if [[ -n "$progress" ]]; then
    printf " ${color}${BOLD}${icon}${RESET} ${BOLD}%s${RESET} %s ${DIM}[%s]${RESET}\n" "$landing_id" "$name_display" "$progress"
  else
    printf " ${color}${BOLD}${icon}${RESET} ${BOLD}%s${RESET} %s\n" "$landing_id" "$name_display"
  fi

  if [[ -n "$status_text" ]]; then
    detail=$(echo "$status_text" | sed 's/^[^—]*— //')
    if [[ "$detail" == "$status_text" ]]; then
      detail=$(echo "$status_text" | sed 's/^Not Started$//' | sed 's/^Done$//')
    fi
    if [[ -n "$detail" ]]; then
      echo "$detail" | sed 's/\. /.\n/g' | while IFS= read -r sentence; do
        sentence=$(echo "$sentence" | sed 's/^ *//;s/ *$//')
        [[ -z "$sentence" ]] && continue
        sentence=$(trunc "$sentence" 52)
        printf "   ${DIM}· %s${RESET}\n" "$sentence"
      done
    fi
  fi

  if [[ -n "$weekly_target" || -n "$holder_text" ]]; then
    dim_row="  ↳"
    [[ -n "$weekly_target" ]] && dim_row="$dim_row $weekly_target"
    [[ -n "$holder_text" ]] && dim_row="$dim_row  $holder_text"
    printf "${DIM}%s${RESET}\n" "$dim_row"
  fi
}

while IFS= read -r line; do
  if [[ "$line" =~ ^###\ L([0-9]+)\. ]]; then
    print_landing

    landing_id="L${BASH_REMATCH[1]}"
    landing_name=$(echo "$line" | sed 's/^### L[0-9]*\. //')
    status_text=""
    weekly_target=""
    holder_text=""
    sub_done=0
    sub_total=0
    in_landing=true
    continue
  fi

  if [[ "$in_landing" == true && "$line" =~ ^\*\*Status:\*\* ]]; then
    status_text=$(echo "$line" | sed 's/\*\*Status:\*\* //')
    continue
  fi

  if [[ "$in_landing" == true && "$line" =~ ^\*\*Weekly\ target:\*\* ]]; then
    weekly_target=$(echo "$line" | sed 's/\*\*Weekly target:\*\* //')
    continue
  fi

  if [[ "$in_landing" == true && "$line" =~ ^\*\*Holder:\*\* ]]; then
    holder_text=$(echo "$line" | sed 's/\*\*Holder:\*\* //')
    continue
  fi

  if [[ "$in_landing" == true && "$line" =~ ^\|[^-] && ! "$line" =~ "Sub-item" ]]; then
    sub_total=$((sub_total + 1))
    if [[ "$line" =~ "Done ✓" ]]; then
      sub_done=$((sub_done + 1))
    fi
  fi

  if [[ "$line" =~ ^##\  && ! "$line" =~ ^###\  ]]; then
    in_landing=false
  fi

done < "$FILE"

print_landing
echo ""
fi  # end HAS_LANDINGS

# --- Open Threads ---
if [[ "$HAS_LANDINGS" == true ]]; then
in_threads=false
thread_id=""
thread_name=""
thread_state=""
thread_next=""

print_thread() {
  if [[ -z "$thread_name" ]]; then return; fi
  case "$thread_state" in
    *complete*|*done*|*Done*) t_color=$GREEN; t_icon="✓" ;;
    *build*|*Build*|*implement*|*Active*) t_color=$TEAL; t_icon="▸" ;;
    *review*|*Review*|*pending*|*Pending*) t_color=$YELLOW; t_icon="◷" ;;
    *) t_color=$DIM; t_icon="○" ;;
  esac
  t_display=$(trunc "$thread_name" 44)
  printf " ${t_color}${BOLD}${t_icon}${RESET} ${BOLD}%s${RESET} %s\n" "$thread_id" "$t_display"
  if [[ -n "$thread_state" ]]; then
    t_state_short=$(trunc "$thread_state" 48)
    printf "   ${DIM}State: %s${RESET}\n" "$t_state_short"
  fi
  if [[ -n "$thread_next" ]]; then
    t_next_short=$(trunc "$thread_next" 48)
    printf "   ${TEAL}Next: %s${RESET}\n" "$t_next_short"
  fi
}

while IFS= read -r line; do
  if [[ "$line" == "## Open Threads"* ]]; then
    in_threads=true
    echo -e "${LAVENDER}── Threads ──${RESET}"
    continue
  fi
  if [[ "$in_threads" == true && "$line" =~ ^##\  && ! "$line" =~ ^###\  ]]; then
    print_thread
    in_threads=false
    continue
  fi
  if [[ "$in_threads" == true && "$line" =~ ^###\ T([0-9]+)\. ]]; then
    print_thread
    thread_id="T${BASH_REMATCH[1]}"
    thread_name=$(echo "$line" | sed 's/^### T[0-9]*\. //' | sed 's/ — .*//')
    thread_state=""
    thread_next=""
    continue
  fi
  if [[ "$in_threads" == true && "$line" =~ ^\*\*State:\*\* ]]; then
    thread_state=$(echo "$line" | sed 's/\*\*State:\*\* //')
    continue
  fi
  if [[ "$in_threads" == true && "$line" =~ ^\*\*Next\ action:\*\* ]]; then
    thread_next=$(echo "$line" | sed 's/\*\*Next action:\*\* //')
    continue
  fi
done < "$FILE"
print_thread
if [[ "$in_threads" == true ]]; then echo ""; fi

# --- Last 3 changelog entries (compact) ---
if grep -q "^- " "$FILE" 2>/dev/null; then
  echo -e "${LAVENDER}── Recent ──${RESET}"
  grep "^- " "$FILE" | tail -3 | while IFS= read -r entry; do
    time=$(echo "$entry" | sed -n 's/^- \([0-9:]*\).*/\1/p')
    body=$(echo "$entry" | sed 's/^- [0-9:]* — //')
    body=$(trunc "$body" $MAX)
    if [[ -n "$time" ]]; then
      printf " ${DIM}%s${RESET} %s\n" "$time" "$body"
    else
      printf " %s\n" "$body"
    fi
  done
fi

fi  # end HAS_LANDINGS (Threads, Recent)

echo ""
echo -e "${TEXT}$(date +%H:%M:%S)${RESET}"
