#!/bin/bash
# sync-safe
# startup.sh — Automated session startup checklist
#
# Runs the full startup sequence for the hex agent.
#
# Usage:
#   startup.sh              # Full startup
#   startup.sh --quick      # Skip integration pulls
#   startup.sh --step NAME  # Run a single step
#   startup.sh --status     # Show what's been done today
#
# Exit codes:
#   0 = all steps passed (or warnings only)
#   1 = failures (something broke)
#
# Companion integrations (graceful degradation when absent):
#   ~/.hex-events/   hex-events reactive automation system
#   ~/.boi/          BOI parallel worker dispatch

set -uo pipefail

# ─── Resolve paths ───────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# HEX_DIR may be injected by the caller (e.g. in tests); fall back to the
# directory two levels above this script (.hex/scripts/ → .hex/ → HEX_DIR/).
HEX_DIR="${HEX_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
HEX_SYSTEM_DIR="$HEX_DIR/.hex"
SCRIPTS_DIR="$HEX_SYSTEM_DIR/scripts"
SKILLS_DIR="$HEX_SYSTEM_DIR/skills"
MEMORY_SCRIPTS="$SKILLS_DIR/memory/scripts"

# Colors
GREEN='\033[32m'
YELLOW='\033[33m'
RED='\033[31m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

# State
WARNINGS=0
FAILURES=0
SESSION_ID=""
IS_SOLO=true
PRIVACY_MODE=false

# Use configured timezone from .hex/timezone (if set)
if [ -z "${TZ:-}" ] && [ -f "$HEX_SYSTEM_DIR/timezone" ]; then
  export TZ="$(cat "$HEX_SYSTEM_DIR/timezone" | tr -d '[:space:]')"
fi
TODAY=$(date +%Y-%m-%d)

# Privacy mode check
if [[ "${HEX_PRIVACY:-}" == "1" ]]; then
    PRIVACY_MODE=true
fi

# ─── Helpers ─────────────────────────────────────────────────────────────────
pass()   { echo -e "  [${GREEN}PASS${RESET}] $1"; }
warn()   { echo -e "  [${YELLOW}WARN${RESET}] $1"; WARNINGS=$((WARNINGS + 1)); }
fail()   { echo -e "  [${RED}FAIL${RESET}] $1"; FAILURES=$((FAILURES + 1)); }
info()   { echo -e "  ${DIM}→${RESET} $1"; }
header() { echo -e "\n${BOLD}$1${RESET}"; }

# ─── Step: Environment Detection ────────────────────────────────────────────
step_env() {
    header "1. Environment Detection"

    if [[ "$OSTYPE" == darwin* ]]; then
        pass "macOS"
    elif [[ "$OSTYPE" == linux* ]]; then
        pass "Linux"
    else
        warn "Unknown environment: $OSTYPE"
    fi

    info "HEX_DIR=$HEX_DIR"
}

# ─── Step: Session Management ───────────────────────────────────────────────
step_session() {
    header "2. Session Management"

    SESSION_SH="$SCRIPTS_DIR/session.sh"
    if [[ ! -f "$SESSION_SH" ]]; then
        info "session.sh not found — multi-session tracking unavailable"
        info "Install session.sh to $SCRIPTS_DIR/ to enable this feature"
        return
    fi

    # Cleanup stale sessions first
    CLEANUP_OUT=$(bash "$SESSION_SH" cleanup 2>&1)
    info "$CLEANUP_OUT"

    # Check for other sessions
    CHECK_OUT=$(bash "$SESSION_SH" check 2>&1) || true
    if echo "$CHECK_OUT" | grep -q "No active sessions"; then
        IS_SOLO=true
        pass "No other sessions. Solo mode."
    else
        IS_SOLO=false
        ACTIVE_COUNT=$(echo "$CHECK_OUT" | grep -c "^SESSION" || echo "0")
        warn "$ACTIVE_COUNT other session(s) active. Limited mode."
        echo "$CHECK_OUT" | grep "^SESSION" | while read -r line; do
            info "$line"
        done
    fi

    # Register this session
    SESSION_ID=$(bash "$SESSION_SH" start "startup-script" 2>&1)
    pass "Registered session: $SESSION_ID"
}

# ─── Step: Parse Transcripts ───────────────────────────────────────────────
step_transcripts() {
    header "3. Parse Transcripts"

    PARSER="$SCRIPTS_DIR/parse_transcripts.py"
    if [[ ! -f "$PARSER" ]]; then
        info "parse_transcripts.py not found — transcript parsing unavailable"
        info "Install parse_transcripts.py to $SCRIPTS_DIR/ to enable this feature"
        return
    fi

    PARSE_OUT=$(python3 "$PARSER" 2>&1)
    if echo "$PARSE_OUT" | grep -q "No new transcripts"; then
        pass "All transcripts already parsed"
    elif echo "$PARSE_OUT" | grep -q "No .jsonl files"; then
        pass "No transcripts to parse"
    else
        echo "$PARSE_OUT" | while read -r line; do
            [[ -n "$line" ]] && info "$line"
        done
        pass "Transcripts parsed"
    fi
}

# ─── Step: Rebuild Memory Index ────────────────────────────────────────────
step_index() {
    header "4. Memory Index"

    INDEXER="$MEMORY_SCRIPTS/memory_index.py"
    if [[ ! -f "$INDEXER" ]]; then
        warn "memory_index.py not found at $MEMORY_SCRIPTS"
        return
    fi

    INDEX_OUT=$(python3 "$INDEXER" 2>&1)
    INDEXED=$(echo "$INDEX_OUT" | grep "^Done:" || echo "$INDEX_OUT" | tail -1)
    if [[ -n "$INDEXED" ]]; then
        info "$INDEXED"
    fi
    pass "Memory index rebuilt"
}

# ─── Step: Memory Health Check ─────────────────────────────────────────────
step_health() {
    header "5. Memory Health"

    HEALTH="$MEMORY_SCRIPTS/memory_health.py"
    if [[ ! -f "$HEALTH" ]]; then
        info "memory_health.py not found (optional)"
        return
    fi

    HEALTH_OUT=$(python3 "$HEALTH" --quiet 2>&1)
    if echo "$HEALTH_OUT" | grep -q "FAIL"; then
        echo "$HEALTH_OUT" | grep "FAIL" | while read -r line; do
            fail "$(echo "$line" | sed 's/.*FAIL.*\] //')"
        done
    elif echo "$HEALTH_OUT" | grep -q "WARN"; then
        echo "$HEALTH_OUT" | grep "WARN" | while read -r line; do
            warn "$(echo "$line" | sed 's/.*WARN.*\] //')"
        done
    else
        pass "All health checks passed"
    fi
}

# ─── Step: Integrations Check ──────────────────────────────────────────────
step_integrations() {
    header "6. Integrations"

    # Check for integrations.json (user-configured external tools)
    INTEGRATIONS="$HEX_DIR/integrations.json"
    if [[ ! -f "$INTEGRATIONS" ]]; then
        info "No integrations configured. The agent works without them."
        info "Create integrations.json to connect external tools (calendar, messaging, etc.)"
        return
    fi

    # Parse and report configured integrations
    while IFS= read -r line; do
        NAME=$(echo "$line" | grep -o '"name": "[^"]*"' | cut -d'"' -f4)
        ENABLED=$(echo "$line" | grep -o '"enabled": [a-z]*' | cut -d: -f2 | xargs)
        if [[ "$ENABLED" == "true" ]]; then
            pass "$NAME connected"
        else
            info "$NAME configured but disabled"
        fi
    done < <(python3 -c "
import json, sys
try:
    with open('$INTEGRATIONS') as f:
        data = json.load(f)
    for name, cfg in data.get('integrations', {}).items():
        print(json.dumps({'name': name, 'enabled': cfg.get('enabled', False)}))
except Exception as e:
    print(f'Error reading integrations: {e}', file=sys.stderr)
" 2>&1) || true
}

# ─── Step: Evolution Check ─────────────────────────────────────────────────
step_evolution() {
    header "7. Improvement Engine"

    SUGGESTIONS="$HEX_DIR/evolution/suggestions.md"
    if [[ ! -f "$SUGGESTIONS" ]]; then
        info "No improvement suggestions yet"
        return
    fi

    # Check evolution DB for items ready for promotion (3+ occurrences)
    CHECK_EVO="$SCRIPTS_DIR/check-evolution.sh"
    if [[ -f "$CHECK_EVO" ]]; then
        CHECK_OUT=$(bash "$CHECK_EVO" 2>&1) || true
        if echo "$CHECK_OUT" | grep -q "Appended"; then
            info "$CHECK_OUT"
        fi
    fi

    # Count pending suggestions (lines with "Status: proposed")
    PENDING=$(grep -c "Status: proposed" "$SUGGESTIONS" 2>/dev/null) || true
    if [[ "${PENDING:-0}" -gt 0 ]]; then
        warn "$PENDING pending improvement suggestion(s). Review at session start."
    else
        pass "No pending suggestions"
    fi

    # Generate performance context from eval_records (optional)
    PERF_CTX_SCRIPT="$HEX_DIR/evolution/eval/generate-performance-context.py"
    PERF_CTX_OUTPUT="$HEX_DIR/evolution/eval/latest-performance-context.md"
    MEMORY_DB="$HEX_SYSTEM_DIR/memory.db"
    if [[ -f "$PERF_CTX_SCRIPT" && -f "$MEMORY_DB" ]]; then
        if python3 "$PERF_CTX_SCRIPT" --db "$MEMORY_DB" --output "$PERF_CTX_OUTPUT" 2>/dev/null; then
            pass "Performance context generated"
        else
            info "Performance context generation failed (non-fatal)"
        fi
    fi
}

# ─── Step: Priority Scoring ────────────────────────────────────────────────
step_priorities() {
    header "8. Priority Scoring"

    PRIORITY_SCORE="$HEX_DIR/evolution/priority-score.py"
    if [[ ! -f "$PRIORITY_SCORE" ]]; then
        info "priority-score.py not found (optional)"
        return
    fi

    if python3 "$PRIORITY_SCORE" --top 3 --output "$HEX_DIR/evolution/priority-ranked.yaml" 2>/dev/null; then
        pass "Top priorities scored"
    else
        warn "Priority scoring failed (non-fatal)"
    fi
}

# ─── Step: Daemon Status ──────────────────────────────────────────────────
step_daemon_status() {
    header "9. Daemon Status"

    local daemons_script="$SCRIPTS_DIR/hex-daemons.sh"
    if [[ ! -f "$daemons_script" ]]; then
        info "hex-daemons.sh not found — install hex-daemons to enable daemon management"
        info "Start daemons manually or run: hex-daemons setup"
        return
    fi

    # Check-only. Never start daemons from within the agent (sandbox restrictions).
    local status_out
    status_out=$(bash "$daemons_script" status 2>&1) || true

    local stripped
    stripped=$(echo "$status_out" | sed 's/\x1b\[[0-9;]*m//g')

    local any_down=false
    while IFS= read -r line; do
        if echo "$line" | grep -qE '\[OK\]'; then
            local daemon_info
            daemon_info=$(echo "$line" | sed 's/.*\[OK\][[:space:]]*//')
            pass "$daemon_info"
        elif echo "$line" | grep -qE '\[WARN\]|\[FAIL\]'; then
            local daemon_msg
            daemon_msg=$(echo "$line" | sed 's/.*\(WARN\|FAIL\)\][[:space:]]*//')
            warn "$daemon_msg"
            any_down=true
        elif echo "$line" | grep -q "not installed"; then
            local daemon_msg
            daemon_msg=$(echo "$line" | sed 's/.*>[[:space:]]*//')
            info "$daemon_msg"
        fi
    done <<< "$stripped"

    if $any_down; then
        info "Start daemons outside the agent: hex-daemons start <name>"
        info "Or install all: hex-daemons setup"
    fi
}

# ─── Step: hex-events Telemetry ───────────────────────────────────────────
step_hex_events() {
    header "10. hex-events Telemetry"

    HEX_EVENTS_CLI="$HOME/.hex-events/hex_events_cli.py"
    HEX_EVENTS_PYTHON="$HOME/.hex-events/venv/bin/python"

    if [[ ! -f "$HEX_EVENTS_CLI" ]]; then
        info "hex-events not installed (optional companion)"
        info "Install hex-events to enable reactive automation policies"
        return
    fi

    # hex-events telemetry check
    ERRORS=$(${HEX_EVENTS_PYTHON} "${HEX_EVENTS_CLI}" telemetry --json 2>/dev/null \
        | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('actions_failed',0))" \
        2>/dev/null || echo "0")
    if [ "${ERRORS:-0}" -gt 0 ]; then
        warn "$ERRORS hex-events action failures in last 24h. Run 'hex-events telemetry' for details."
    else
        pass "hex-events OK"
    fi
}

# ─── Step: Status ──────────────────────────────────────────────────────────
step_status() {
    header "Startup Status — $TODAY"

    # Active sessions
    if ls "$HEX_DIR/.sessions"/session_*.json 1>/dev/null 2>&1; then
        SESSION_COUNT=$(ls "$HEX_DIR/.sessions"/session_*.json 2>/dev/null | wc -l | xargs)
        info "Active sessions: $SESSION_COUNT"
    else
        info "No active sessions"
    fi

    # Transcripts
    if ls "$HEX_DIR/raw/transcripts"/*.md 1>/dev/null 2>&1; then
        MD_COUNT=$(ls "$HEX_DIR/raw/transcripts"/*.md 2>/dev/null | wc -l | xargs)
        pass "Transcripts: $MD_COUNT parsed files"
    else
        info "No parsed transcripts"
    fi

    # Memory DB freshness
    DB="$HEX_SYSTEM_DIR/memory.db"
    if [[ -f "$DB" ]]; then
        if [[ "$OSTYPE" == darwin* ]]; then
            DB_MOD=$(stat -f %m "$DB")
        else
            DB_MOD=$(stat -c %Y "$DB")
        fi
        DB_AGE=$(( ($(date +%s) - DB_MOD) / 60 ))
        if [[ $DB_AGE -lt 60 ]]; then
            pass "Memory index fresh (${DB_AGE}min ago)"
        else
            warn "Memory index stale (${DB_AGE}min ago)"
        fi
    else
        warn "No memory.db"
    fi
}

# ─── Main ──────────────────────────────────────────────────────────────────
main() {
    local QUICK=false
    local SINGLE_STEP=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --quick)  QUICK=true; shift ;;
            --step)   SINGLE_STEP="${2:-}"; shift 2 ;;
            --status) step_status; exit 0 ;;
            --help|-h)
                echo "Usage: startup.sh [--quick] [--step NAME] [--status]"
                echo ""
                echo "Steps: env, session, transcripts, index, health, integrations, evolution, priorities, daemons, hex-events"
                echo ""
                echo "Options:"
                echo "  --quick    Skip integration checks"
                echo "  --step X   Run only step X"
                echo "  --status   Show what's been done today"
                echo ""
                exit 0
                ;;
            *) echo "Unknown option: $1"; exit 1 ;;
        esac
    done

    # Single step mode
    if [[ -n "$SINGLE_STEP" ]]; then
        case "$SINGLE_STEP" in
            env)          step_env ;;
            session)      step_session ;;
            transcripts)  step_transcripts ;;
            index)        step_index ;;
            health)       step_health ;;
            integrations) step_integrations ;;
            evolution)    step_evolution ;;
            priorities)   step_priorities ;;
            daemons)      step_daemon_status ;;
            hex-events)   step_hex_events ;;
            *) echo "Unknown step: $SINGLE_STEP"; exit 1 ;;
        esac
        exit 0
    fi

    # Full startup
    echo ""
    echo "============================================================"
    echo " Hex Startup — $(date '+%Y-%m-%d %H:%M')"
    echo "============================================================"

    # Launch update check in background (non-blocking)
    CHECK_UPDATE_SH="$SCRIPTS_DIR/check-update.sh"
    if [[ -f "$CHECK_UPDATE_SH" ]]; then
        bash "$CHECK_UPDATE_SH" &
    fi

    if $PRIVACY_MODE; then
        echo ""
        echo -e "  ${YELLOW}${BOLD}[PRIVACY MODE]${RESET} Sensitive context loading disabled."
        echo ""
    fi

    step_env
    step_session

    # Surface any background health check alerts
    if [ -f "$HEX_DIR/.hex/doctor-alert" ]; then
        warn "Background health check found issues (run /hex-doctor to review):"
        head -20 "$HEX_DIR/.hex/doctor-alert"
        rm -f "$HEX_DIR/.hex/doctor-alert"
    fi

    step_transcripts

    if $IS_SOLO && ! $QUICK; then
        step_index
        step_health
        step_integrations
        step_evolution
        step_priorities
        step_daemon_status
    elif $QUICK; then
        step_index
        step_health
        step_daemon_status
        info ""
        info "Quick mode. Skipped integrations and evolution check."
    else
        info ""
        info "Multi-session mode. Skipped index rebuild and data pulls."
        info "Read todo.md and latest context to get started."
    fi

    step_hex_events

    # Emit session.started for hex-events policies
    HEX_EMIT="$HOME/.hex-events/hex_emit.py"
    HEX_EMIT_PYTHON="$HOME/.hex-events/venv/bin/python"
    if [[ -f "$HEX_EMIT" && -f "$HEX_EMIT_PYTHON" ]]; then
        "$HEX_EMIT_PYTHON" "$HEX_EMIT" session.started \
            "{\"hex_dir\":\"$HEX_DIR\",\"today\":\"$TODAY\"}" \
            startup.sh 2>/dev/null || true
    fi

    # Update notification
    if [[ -f "$HEX_SYSTEM_DIR/.update-available" ]]; then
        echo ""
        echo -e "  ${YELLOW}[UPDATE]${RESET} A new version of hex is available. Run /hex-upgrade to update."
    fi

    # Summary
    echo ""
    echo "────────────────────────────────────────────────────────────"
    if [[ $FAILURES -gt 0 ]]; then
        echo -e "  ${RED}${FAILURES} failure(s)${RESET}, ${YELLOW}${WARNINGS} warning(s)${RESET}"
        exit 1
    elif [[ $WARNINGS -gt 0 ]]; then
        echo -e "  ${GREEN}Startup complete${RESET} with ${YELLOW}${WARNINGS} warning(s)${RESET}"
        exit 0
    else
        echo -e "  ${GREEN}Startup complete. All checks passed.${RESET}"
        exit 0
    fi
}

main "$@"
