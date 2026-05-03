#!/bin/bash
# sync-safe
# doctor.sh — Scriptable health checks for hex agent installation
#
# Usage:
#   doctor.sh              # Run all checks, report results
#   doctor.sh --fix        # Auto-fix safe issues
#   doctor.sh --json       # Output full results as JSON
#   doctor.sh --quiet      # Only show errors and warnings (no PASS lines)
#
# Exit codes:
#   0 = all pass
#   1 = errors found
#   2 = warnings only (no errors)

set -uo pipefail

# ─── Resolve HEX_DIR ─────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# HEX_DIR may be injected by the caller (e.g. in tests); fall back to the
# directory two levels above this script (.hex/scripts/ → .hex/ → HEX_DIR/).
HEX_DIR="${HEX_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

if [[ ! -f "$HEX_DIR/CLAUDE.md" ]] && [[ ! -f "$HEX_DIR/AGENTS.md" ]]; then
  echo "ERROR: HEX_DIR=$HEX_DIR does not contain CLAUDE.md — not a valid hex workspace" >&2
  echo "  Fix: export HEX_DIR=\$HOME/hex  (or wherever hex is installed)" >&2
  exit 1
fi

if ! grep -q 'export HEX_DIR=' "$HOME/.zshrc" 2>/dev/null && \
   ! grep -q 'export HEX_DIR=' "$HOME/.bashrc" 2>/dev/null; then
  echo "WARN: HEX_DIR not exported in shell rc — hex may not start in new terminals" >&2
  echo "  Fix: echo 'export HEX_DIR=\"$HEX_DIR\"' >> ~/.zshrc" >&2
fi

HEX_SYSTEM_DIR="$HEX_DIR/.hex"
SCRIPTS_DIR="$HEX_SYSTEM_DIR/scripts"
# shellcheck source=./env.sh
[[ -f "$SCRIPTS_DIR/env.sh" ]] && source "$SCRIPTS_DIR/env.sh" 2>/dev/null || true

# Colors
GREEN='\033[32m'
YELLOW='\033[33m'
RED='\033[31m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

# ─── Flags ────────────────────────────────────────────────────────────────────
FIX=false
JSON_MODE=false
QUIET=false

if [[ "${DOCTOR_SOURCE_ONLY:-}" != "1" ]]; then
  for arg in "$@"; do
    case "$arg" in
      --fix)   FIX=true ;;
      --json)  JSON_MODE=true ;;
      --quiet) QUIET=true ;;
      --help|-h)
        echo "Usage: doctor.sh [--fix] [--json] [--quiet]"
        echo ""
        echo "  --fix    Auto-fix safe issues (checks 1,2,5,8,10,11,12,13,14,15)"
        echo "  --json   Output full results as JSON (always includes all checks)"
        echo "  --quiet  Only show errors and warnings (skip passing checks)"
        exit 0
        ;;
      *) echo "Unknown option: $arg" >&2; exit 1 ;;
    esac
  done
fi

# ─── State ────────────────────────────────────────────────────────────────────
PASS_COUNT=0
WARN_COUNT=0
ERROR_COUNT=0
FIXED_COUNT=0
HAS_ERRORS=false
HAS_WARNINGS=false

# Temp file for check records (tab-separated: id<TAB>name<TAB>status<TAB>message)
CHECKS_FILE=$(mktemp /tmp/doctor-checks.XXXXXX)
trap 'rm -f "$CHECKS_FILE"' EXIT

# ─── Output helpers ───────────────────────────────────────────────────────────
_pass() {
  PASS_COUNT=$((PASS_COUNT + 1))
  if ! $JSON_MODE && ! $QUIET; then
    echo -e "  [${GREEN}PASS${RESET}] $1"
  fi
}

_warn() {
  WARN_COUNT=$((WARN_COUNT + 1))
  HAS_WARNINGS=true
  if ! $JSON_MODE; then
    echo -e "  [${YELLOW}WARN${RESET}] $1"
  fi
}

_error() {
  ERROR_COUNT=$((ERROR_COUNT + 1))
  HAS_ERRORS=true
  if ! $JSON_MODE; then
    echo -e "  [${RED}ERROR${RESET}] $1"
  fi
}

_fixed() {
  FIXED_COUNT=$((FIXED_COUNT + 1))
  if ! $JSON_MODE; then
    echo -e "  [${GREEN}FIXED${RESET}] $1"
  fi
}

_info() {
  if ! $JSON_MODE && ! $QUIET; then
    echo -e "  ${DIM}→${RESET} $1"
  fi
}

# Record a check result: id, name, status (pass|warn|error|fixed), message
# Note: no tab characters allowed in name or message fields
_rec() {
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" >> "$CHECKS_FILE"
}

# ─── Checks ───────────────────────────────────────────────────────────────────

# 1: .hex/ exists (error, fix: mkdir)
check_1() {
  if [ -d "$HEX_SYSTEM_DIR" ]; then
    _pass ".hex/ exists"
    _rec 1 ".hex exists" "pass" ".hex/ directory found"
    return
  fi
  if $FIX; then
    mkdir -p "$HEX_SYSTEM_DIR"
    _fixed ".hex/ created"
    _rec 1 ".hex exists" "fixed" ".hex/ created"
    return
  fi
  _error ".hex/ missing"
  _rec 1 ".hex exists" "error" ".hex/ directory not found"
}

# 2: .git/ initialized (error, fix: git init + initial commit)
check_2() {
  if [ -d "$HEX_DIR/.git" ]; then
    _pass ".git/ initialized"
    _rec 2 ".git initialized" "pass" "git repository found"
    return
  fi
  if $FIX; then
    (cd "$HEX_DIR" && git init -q && git commit --allow-empty -q -m "Initial commit") 2>/dev/null || true
    if [ -d "$HEX_DIR/.git" ]; then
      _fixed ".git/ initialized"
      _rec 2 ".git initialized" "fixed" "git init + initial commit"
      return
    fi
    _error ".git/ init failed"
    _rec 2 ".git initialized" "error" "git init failed"
    return
  fi
  _error ".git/ not initialized"
  _rec 2 ".git initialized" "error" "git repository not found"
}

# 3: .hex/ directory structure — skills subdirectory (error, no fix)
check_3() {
  if [ -d "$HEX_SYSTEM_DIR/skills" ]; then
    _pass ".hex/skills/ exists"
    _rec 3 ".hex/skills/ exists" "pass" ".hex/skills/ directory found"
    return
  fi
  _error ".hex/skills/ missing — re-run bootstrap to fix"
  _rec 3 ".hex/skills/ exists" "error" ".hex/skills/ not found — re-run bootstrap"
}

# 4: .hex/skills/ has skill dirs (error, no fix)
check_4() {
  if [ -d "$HEX_SYSTEM_DIR/skills" ]; then
    local count
    count=$(ls -d "$HEX_SYSTEM_DIR/skills"/*/ 2>/dev/null | wc -l | tr -d ' ')
    if [ "${count:-0}" -gt 0 ]; then
      _pass ".hex/skills/ has ${count} skill directories"
      _rec 4 ".hex/skills/ has skills" "pass" "${count} skill directories found"
      return
    fi
  fi
  _error ".hex/skills/ empty or missing — re-run bootstrap"
  _rec 4 ".hex/skills/ has skills" "error" "no skill directories found — re-run bootstrap"
}

# 5: .agents/skills/ linked to .hex/skills/ (error, fix: create symlink)
check_5() {
  local agents_skills="$HEX_DIR/.agents/skills"
  local hex_skills="$HEX_SYSTEM_DIR/skills"

  if [ -L "$agents_skills" ]; then
    _pass ".agents/skills/ linked to .hex/skills/"
    _rec 5 ".agents/skills/ symlinked" "pass" "symlink exists"
    return
  fi

  if [ -d "$agents_skills" ]; then
    local count
    count=$(ls "$agents_skills" 2>/dev/null | wc -l | tr -d ' ')
    if [ "${count:-0}" -gt 0 ]; then
      _warn ".agents/skills/ is a real non-empty directory — skipping symlink creation"
      _rec 5 ".agents/skills/ symlinked" "warn" ".agents/skills/ is a non-empty real directory"
      return
    fi
    if $FIX; then
      rm -rf "$agents_skills"
      mkdir -p "$HEX_DIR/.agents"
      ln -s "$hex_skills" "$agents_skills"
      _fixed ".agents/skills/ replaced with symlink to .hex/skills/"
      _rec 5 ".agents/skills/ symlinked" "fixed" "replaced empty directory with symlink"
      return
    fi
  fi

  if $FIX; then
    mkdir -p "$HEX_DIR/.agents"
    ln -s "$hex_skills" "$agents_skills"
    _fixed ".agents/skills/ symlinked to .hex/skills/"
    _rec 5 ".agents/skills/ symlinked" "fixed" "created symlink"
    return
  fi

  _error ".agents/skills/ not linked to .hex/skills/"
  _rec 5 ".agents/skills/ symlinked" "error" ".agents/skills/ not found or not a symlink"
}

# 6: CLAUDE.md exists and >1000 bytes (error, no fix)
check_6() {
  if [ -f "$HEX_DIR/CLAUDE.md" ]; then
    local size
    size=$(wc -c < "$HEX_DIR/CLAUDE.md" | tr -d ' ')
    if [ "${size:-0}" -gt 1000 ]; then
      _pass "CLAUDE.md exists (${size} bytes)"
      _rec 6 "CLAUDE.md exists and >1000 bytes" "pass" "CLAUDE.md found (${size} bytes)"
      return
    fi
    _error "CLAUDE.md exists but only ${size} bytes (expected >1000)"
    _rec 6 "CLAUDE.md exists and >1000 bytes" "error" "CLAUDE.md is ${size} bytes"
    return
  fi
  _error "CLAUDE.md missing"
  _rec 6 "CLAUDE.md exists and >1000 bytes" "error" "CLAUDE.md not found"
}

# 7: Agent fleet validates + core agents healthy
check_7() {
  local hex_agent="$HEX_DIR/.hex/bin/hex"
  if [ ! -x "$hex_agent" ]; then
    _warn "hex binary missing at $hex_agent — fleet validation skipped (install Rust to enable)"
    _rec 7 "agent-fleet" "warn" "hex binary missing"
    return
  fi

  # Validate all charters
  local fleet_out
  fleet_out=$(HEX_DIR="$HEX_DIR" "$hex_agent" agent fleet 2>&1)
  local fleet_exit=$?
  if [ $fleet_exit -ne 0 ]; then
    _error "hex agent fleet failed (exit $fleet_exit):"
    echo "$fleet_out" | grep -E 'ERROR' | while read -r line; do _error "  $line"; done
    _rec 7 "agent-fleet" "error" "fleet validation failed"
    return
  fi

  local agent_count core_count
  agent_count=$(HEX_DIR="$HEX_DIR" "$hex_agent" agent list 2>/dev/null | wc -l | tr -d ' ')
  core_count=$(HEX_DIR="$HEX_DIR" "$hex_agent" agent list --core 2>/dev/null | wc -l | tr -d ' ')
  _pass "Fleet OK — $agent_count agents ($core_count core) discovered from charters"
  _rec 7 "agent-fleet" "pass" "$agent_count agents, $core_count core"

  # Check core agents are not halted
  local halted_core=0
  while IFS= read -r core_id; do
    [ -z "$core_id" ] && continue
    if [ -f "$HOME/.hex-${core_id}-HALT" ]; then
      _error "Core agent '$core_id' is HALTED — system operations degraded"
      halted_core=$((halted_core + 1))
    fi
  done < <(HEX_DIR="$HEX_DIR" "$hex_agent" agent list --core 2>/dev/null)

  if [ $halted_core -gt 0 ]; then
    _rec 7 "core-agents" "error" "$halted_core core agent(s) halted"
  elif [ "$core_count" -gt 0 ]; then
    _pass "All $core_count core agents active"
    _rec 7 "core-agents" "pass" "all core agents active"
  fi

  # Check for drift from reference core agents (single invocation)
  local check_out check_rc
  check_out=$(HEX_DIR="$HEX_DIR" "$hex_agent" agent check-core 2>&1)
  check_rc=$?
  if [ $check_rc -eq 0 ]; then
    _pass "Core agents match reference set"
    _rec 7 "core-reference" "pass" "no drift"
  else
    local missing_count broken_count
    missing_count=$(echo "$check_out" | grep -c 'MISSING:' || true)
    broken_count=$(echo "$check_out" | grep -c 'BROKEN:' || true)
    if [ "$missing_count" -gt 0 ]; then
      _error "Core agent drift: $missing_count missing — run 'hex-agent restore-core' to fix"
    fi
    if [ "$broken_count" -gt 0 ]; then
      _error "Core agent drift: $broken_count broken — run 'hex-agent check-core' for details"
    fi
    _rec 7 "core-reference" "error" "${missing_count} missing, ${broken_count} broken"
  fi
}

# 8: .codex/config.toml exists (warning, fix: create with CLAUDE.md fallback)
check_8() {
  if [ -f "$HEX_DIR/.codex/config.toml" ]; then
    _pass ".codex/config.toml exists"
    _rec 8 ".codex/config.toml exists" "pass" ".codex/config.toml found"
    return
  fi
  if $FIX; then
    mkdir -p "$HEX_DIR/.codex"
    {
      echo "# codex config — generated by hex doctor"
      echo "[profile]"
      echo 'model = "codex-mini-latest"'
      echo ""
      echo "# Uses AGENTS.md or CLAUDE.md as system prompt"
    } > "$HEX_DIR/.codex/config.toml"
    _fixed ".codex/config.toml created"
    _rec 8 ".codex/config.toml exists" "fixed" ".codex/config.toml created"
    return
  fi
  _warn ".codex/config.toml missing"
  _rec 8 ".codex/config.toml exists" "warn" ".codex/config.toml not found"
}

# 9: me/me.md exists with content (info, report only — no fix, no counter increment)
check_9() {
  if [ -f "$HEX_DIR/me/me.md" ]; then
    local size
    size=$(wc -c < "$HEX_DIR/me/me.md" | tr -d ' ')
    _pass "me/me.md exists (${size} bytes)"
    _rec 9 "me/me.md has content" "pass" "me/me.md found (${size} bytes)"
    return
  fi
  # Severity is info — do NOT increment WARN_COUNT or set HAS_WARNINGS
  _info "me/me.md not found (not critical — create to personalize your agent)"
  _rec 9 "me/me.md has content" "warn" "me/me.md not found"
}

# 10: todo.md exists (warning, fix: create skeleton)
check_10() {
  if [ -f "$HEX_DIR/todo.md" ]; then
    _pass "todo.md exists"
    _rec 10 "todo.md exists" "pass" "todo.md found"
    return
  fi
  if $FIX; then
    cat > "$HEX_DIR/todo.md" << 'SKELETON'
# Todo

## Now
<!-- Active tasks -->

## Next
<!-- Upcoming tasks -->

## Later
<!-- Backlog -->
SKELETON
    _fixed "todo.md created (skeleton)"
    _rec 10 "todo.md exists" "fixed" "todo.md skeleton created"
    return
  fi
  _warn "todo.md missing"
  _rec 10 "todo.md exists" "warn" "todo.md not found"
}

# 11: memory.db exists (warning, fix: rebuild index)
check_11() {
  local memory_db="$HEX_SYSTEM_DIR/memory.db"
  if [ -f "$memory_db" ]; then
    _pass "memory.db exists"
    _rec 11 "memory.db exists" "pass" "memory.db found"
    return
  fi
  if $FIX; then
    local indexer="$HEX_SYSTEM_DIR/skills/memory/scripts/memory_index.py"
    if [ -f "$indexer" ]; then
      if ! python3 "$indexer" --full 2>&1; then
        _warn "memory_index.py --full failed — memory reindex error above"
        WARNS=$((WARNS+1))
      fi
      if [ -f "$memory_db" ]; then
        _fixed "memory.db rebuilt via memory_index.py"
        _rec 11 "memory.db exists" "fixed" "memory.db rebuilt"
        return
      fi
    fi
    _warn "memory.db missing — memory_index.py not found or rebuild failed"
    _rec 11 "memory.db exists" "warn" "memory.db rebuild failed"
    return
  fi
  _warn "memory.db missing — run: python3 .hex/skills/memory/scripts/memory_index.py --full"
  _rec 11 "memory.db exists" "warn" "memory.db not found"
}

# 12: No broken symlinks in .hex/ and .agents/ (error, fix: remove)
check_12() {
  local broken=()
  local base
  for base in "$HEX_SYSTEM_DIR" "$HEX_DIR/.agents"; do
    [ -d "$base" ] || continue
    while IFS= read -r link; do
      case "$link" in
        */raw/*) continue ;;
      esac
      broken+=("$link")
    done < <(find "$base" -maxdepth 3 -type l ! -e 2>/dev/null || true)
  done

  if [ ${#broken[@]} -eq 0 ]; then
    _pass "No broken symlinks in .hex/ or .agents/"
    _rec 12 "No broken symlinks" "pass" "no broken symlinks found"
    return
  fi

  if $FIX; then
    local link
    for link in "${broken[@]}"; do
      rm -f "$link"
    done
    _fixed "Removed ${#broken[@]} broken symlink(s)"
    _rec 12 "No broken symlinks" "fixed" "removed ${#broken[@]} broken symlink(s)"
    return
  fi

  _error "${#broken[@]} broken symlink(s) in .hex/ or .agents/"
  local link
  for link in "${broken[@]}"; do
    _info "  broken: $link"
  done
  _rec 12 "No broken symlinks" "error" "${#broken[@]} broken symlinks found"
}

# 13: .sh scripts in .hex/scripts/ are executable (warning, fix: chmod +x)
check_13() {
  if [ ! -d "$SCRIPTS_DIR" ]; then
    _warn ".hex/scripts/ directory not found"
    _rec 13 ".sh scripts are executable" "warn" ".hex/scripts/ directory not found"
    return
  fi

  local not_exec=()
  while IFS= read -r script; do
    [ -x "$script" ] || not_exec+=("$script")
  done < <(find "$SCRIPTS_DIR" -maxdepth 1 -name "*.sh" -type f 2>/dev/null || true)

  if [ ${#not_exec[@]} -eq 0 ]; then
    _pass ".sh scripts in .hex/scripts/ are executable"
    _rec 13 ".sh scripts are executable" "pass" "all scripts are executable"
    return
  fi

  if $FIX; then
    local script
    for script in "${not_exec[@]}"; do
      chmod +x "$script"
    done
    _fixed "chmod +x on ${#not_exec[@]} script(s)"
    _rec 13 ".sh scripts are executable" "fixed" "chmod +x applied to ${#not_exec[@]} scripts"
    return
  fi

  _warn "${#not_exec[@]} script(s) not executable in .hex/scripts/"
  _rec 13 ".sh scripts are executable" "warn" "${#not_exec[@]} scripts not executable"
}

# 14: .hex/llm-preference exists (warning, fix: detect cli and create)
check_14() {
  if [ -f "$HEX_SYSTEM_DIR/llm-preference" ]; then
    local pref
    pref=$(cat "$HEX_SYSTEM_DIR/llm-preference" | tr -d '[:space:]')
    _pass ".hex/llm-preference = $pref"
    _rec 14 ".hex/llm-preference exists" "pass" "llm-preference = $pref"
    return
  fi

  if $FIX; then
    mkdir -p "$HEX_SYSTEM_DIR"
    local detected=""
    if command -v claude &>/dev/null; then
      detected="claude"
    elif command -v codex &>/dev/null; then
      detected="codex"
    fi
    if [ -n "$detected" ]; then
      echo "$detected" > "$HEX_SYSTEM_DIR/llm-preference"
      _fixed ".hex/llm-preference = $detected (auto-detected)"
      _rec 14 ".hex/llm-preference exists" "fixed" "detected $detected on PATH"
      return
    fi
    _warn ".hex/llm-preference missing — could not detect claude or codex on PATH"
    _rec 14 ".hex/llm-preference exists" "warn" "could not auto-detect LLM CLI"
    return
  fi

  _warn ".hex/llm-preference missing"
  _rec 14 ".hex/llm-preference exists" "warn" ".hex/llm-preference not found"
}

# 15: No stale .hex/llm-preference in wrong location (warning, fix: move to .hex/)
check_15() {
  # Check for legacy llm-preference at root level
  local stale="$HEX_DIR/llm-preference"
  if [ ! -f "$stale" ]; then
    _pass "No stale root-level llm-preference"
    _rec 15 "No stale root llm-preference" "pass" "no stale llm-preference"
    return
  fi
  if $FIX; then
    mkdir -p "$HEX_SYSTEM_DIR"
    mv "$stale" "$HEX_SYSTEM_DIR/llm-preference"
    _fixed "Moved root llm-preference → .hex/llm-preference"
    _rec 15 "No stale root llm-preference" "fixed" "moved to .hex/llm-preference"
    return
  fi
  _warn "Stale root llm-preference found (should be .hex/llm-preference)"
  _rec 15 "No stale root llm-preference" "warn" "stale llm-preference at root level"
}

# 16: hex-events reachable (degrade gracefully if not installed)
check_16() {
  local hex_eventd="$HOME/.hex-events/hex_eventd.py"
  if [ ! -f "$hex_eventd" ]; then
    # Fallback: daemon may run from repo path instead of ~/.hex-events/
    if pgrep -f hex_eventd.py >/dev/null 2>&1; then
      _pass "hex-events daemon running (process found, not at ~/.hex-events/)"
      _rec 16 "hex-events reachable" "pass" "hex_eventd.py process running"
      return
    fi
    _info "hex-events not installed (~/.hex-events/hex_eventd.py not found)"
    _rec 16 "hex-events reachable" "warn" "hex-events not installed"
    return
  fi

  local venv_python="$HOME/.hex-events/venv/bin/python3"
  if [ ! -f "$venv_python" ]; then
    _warn "hex-events venv missing ($HOME/.hex-events/venv/bin/python3 not found)"
    _rec 16 "hex-events reachable" "warn" "hex-events venv not found"
    return
  fi

  local hex_events_cmd=""
  if command -v hex-events &>/dev/null; then
    hex_events_cmd="hex-events"
  elif [ -f "$HOME/.hex-events/venv/bin/hex-events" ]; then
    hex_events_cmd="$HOME/.hex-events/venv/bin/hex-events"
  fi

  if [ -n "$hex_events_cmd" ]; then
    if "$hex_events_cmd" validate &>/dev/null 2>&1; then
      _pass "hex-events reachable (validate OK)"
      _rec 16 "hex-events reachable" "pass" "hex-events validate succeeded"
      return
    fi
    _warn "hex-events validate failed"
    _rec 16 "hex-events reachable" "warn" "hex-events validate returned non-zero"
    return
  fi

  _pass "hex-events installed (hex_eventd.py + venv found)"
  _rec 16 "hex-events reachable" "pass" "hex_eventd.py and venv found"
}

# 17: BOI health — runtime checks, not just file-existence
check_17() {
  local boi_bin="$HOME/.boi/bin/boi"
  local boi_wrapper="$HOME/.boi/boi"
  local versions_file="$HEX_DIR/VERSIONS"
  local expected_ver=""

  if [ -f "$versions_file" ]; then
    expected_ver=$(grep "^BOI_VERSION=" "$versions_file" | cut -d= -f2 | tr -d '[:space:]')
  fi

  # Check: symlink not dangling (must test -L before -e; -e follows symlinks)
  if [ -L "$boi_bin" ] && [ ! -f "$boi_bin" ]; then
    _error "BOI: $boi_bin is a dangling symlink — run install.sh or rebuild: cd ~/github.com/mrap/boi && cargo build --release"
    _rec 17 "BOI symlink" "error" "dangling symlink at $boi_bin"
    return
  fi

  # Check: binary exists (graceful degrade if BOI not installed)
  if [ ! -e "$boi_bin" ]; then
    _info "BOI not installed ($boi_bin not found) — run install.sh to install BOI"
    _rec 17 "BOI installed" "warn" "binary not found — not installed"
    return
  fi

  # Check: boi --help exits 0
  local help_rc=0
  "$boi_bin" --help >/dev/null 2>&1 || help_rc=$?
  if [ $help_rc -ne 0 ]; then
    _error "boi --help returned exit $help_rc — binary may be corrupt; run install.sh to rebuild BOI"
    _rec 17 "BOI --help" "error" "boi --help failed (exit $help_rc)"
  else
    _pass "boi --help exits 0"
    _rec 17 "BOI --help" "pass" "boi --help OK"
  fi

  # Check: boi --version exits 0 and matches VERSIONS BOI_VERSION
  local ver_out ver_rc=0
  ver_out=$("$boi_bin" --version 2>&1) || ver_rc=$?
  if [ $ver_rc -ne 0 ]; then
    _error "boi --version returned exit $ver_rc — run install.sh to rebuild BOI"
    _rec 17 "BOI --version" "error" "boi --version failed (exit $ver_rc)"
  elif [ -n "$expected_ver" ]; then
    local clean_ver="${expected_ver#v}"
    if echo "$ver_out" | grep -qF "$clean_ver"; then
      _pass "boi --version matches VERSIONS ($expected_ver)"
      _rec 17 "BOI --version" "pass" "version $expected_ver OK"
    else
      _error "boi --version '$ver_out' does not match VERSIONS $expected_ver — run install.sh to upgrade BOI"
      _rec 17 "BOI --version" "error" "version mismatch: got '$ver_out', want $expected_ver"
    fi
  else
    _pass "boi --version exits 0"
    _rec 17 "BOI --version" "pass" "boi --version OK"
  fi

  # Check: boi status exits 0 (DB queryable)
  local status_rc=0
  "$boi_bin" status >/dev/null 2>&1 || status_rc=$?
  if [ $status_rc -ne 0 ]; then
    _warn "boi status returned exit $status_rc — daemon may not be running; start with: boi start"
    _rec 17 "BOI status" "warn" "boi status failed (exit $status_rc)"
  else
    _pass "boi status exits 0 (DB queryable)"
    _rec 17 "BOI status" "pass" "boi status OK"
  fi

  # Check: wrapper chain ~/.boi/boi --help exits 0
  if [ ! -e "$boi_wrapper" ]; then
    _warn "BOI wrapper missing at $boi_wrapper — run install.sh to restore"
    _rec 17 "BOI wrapper" "warn" "wrapper not found at $boi_wrapper"
  else
    local wrapper_rc=0
    "$boi_wrapper" --help >/dev/null 2>&1 || wrapper_rc=$?
    if [ $wrapper_rc -ne 0 ]; then
      _error "BOI wrapper chain broken: $boi_wrapper --help failed (exit $wrapper_rc) — run install.sh"
      _rec 17 "BOI wrapper" "error" "wrapper chain broken (exit $wrapper_rc)"
    else
      _pass "BOI wrapper chain OK ($boi_wrapper --help exits 0)"
      _rec 17 "BOI wrapper" "pass" "wrapper chain OK"
    fi
  fi
}

# 18: Python 3.10+ available (error)
check_18() {
  local py3; py3=$(for p in python3.14 python3.13 python3.12 python3.11 python3.10 python3; do for d in "" /opt/homebrew/bin/ /usr/local/bin/; do cmd="${d}${p}"; [[ -x "$cmd" ]] && "$cmd" -c "import sys; assert sys.version_info>=(3,10)" 2>/dev/null && echo "$cmd" && break 2; done; done)
  if [[ -n "$py3" ]]; then
    local ver
    ver=$("$py3" -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")
    _pass "Python $ver available"
    _rec 18 "Python 3.10+ available" "pass" "Python $ver found"
    return
  fi
  _error "Python 3.10+ not available"
  _rec 18 "Python 3.10+ available" "error" "python3 not found or version < 3.10"
}

# 19: .hex/settings.json exists and is valid JSON (warning, no fix)
check_19() {
  local settings="$HEX_SYSTEM_DIR/settings.json"
  if [ ! -f "$settings" ]; then
    _warn ".hex/settings.json missing"
    _rec 19 ".hex/settings.json valid" "warn" "settings.json not found"
    return
  fi
  if python3 -m json.tool "$settings" > /dev/null 2>&1; then
    _pass ".hex/settings.json is valid JSON"
    _rec 19 ".hex/settings.json valid" "pass" "settings.json is valid JSON"
    return
  fi
  _warn ".hex/settings.json exists but is invalid JSON"
  _rec 19 ".hex/settings.json valid" "warn" "settings.json parse error"
}

# 20: .hex/timezone exists and contains valid TZ value (warning, report only)
check_20() {
  local tz_file="$HEX_SYSTEM_DIR/timezone"
  if [ ! -f "$tz_file" ]; then
    _warn ".hex/timezone missing"
    _rec 20 ".hex/timezone valid" "warn" "timezone file not found"
    return
  fi
  local tz_val
  tz_val=$(cat "$tz_file" | tr -d '[:space:]')
  if [ -z "$tz_val" ]; then
    _warn ".hex/timezone is empty"
    _rec 20 ".hex/timezone valid" "warn" "timezone file is empty"
    return
  fi
  if TZ="$tz_val" date &>/dev/null 2>&1; then
    _pass ".hex/timezone = $tz_val (valid)"
    _rec 20 ".hex/timezone valid" "pass" "timezone = $tz_val"
    return
  fi
  _warn ".hex/timezone = '$tz_val' (may not be a valid TZ identifier)"
  _rec 20 ".hex/timezone valid" "warn" "timezone '$tz_val' may be invalid"
}

# 21: Agent liveness — all wake scripts source env.sh, claude is reachable, recent logs show successes (error, fix: add source)
check_21() {
  local env_file="$HEX_DIR/.hex/scripts/env.sh"
  if [ ! -f "$env_file" ]; then
    _error ".hex/scripts/env.sh missing — agents have no shared environment"
    _rec 21 "agent-liveness" "error" "env.sh missing"
    return
  fi

  # Verify claude is reachable via env.sh
  if ! bash -c "source '$env_file' && command -v claude" &>/dev/null; then
    _error "claude not reachable after sourcing .hex/scripts/env.sh — check PATH in env.sh"
    _rec 21 "agent-liveness" "error" "claude not on PATH via env.sh"
    return
  fi

  # Check all agents discovered from charters (no hardcoded lists)
  local hex_agent="$HEX_DIR/.hex/bin/hex"
  if [ ! -x "$hex_agent" ]; then
    _warn "hex binary missing — skipping per-agent liveness checks"
    _rec 21 "agent-liveness" "warn" "hex binary missing"
    return
  fi

  local dead_agents=0
  local total_agents=0
  while IFS= read -r agent_id; do
    [ -z "$agent_id" ] && continue
    total_agents=$((total_agents + 1))
    local adir="$HEX_DIR/projects/$agent_id"
    local alog="$adir/log.jsonl"
    [ -f "$alog" ] || continue
    local fail_streak=0
    while IFS= read -r line; do
      if echo "$line" | grep -q '"status":"failed"\|"status":"throttled"'; then
        fail_streak=$((fail_streak + 1))
      else
        fail_streak=0
      fi
    done < <(tail -5 "$alog")
    if [ $fail_streak -ge 5 ]; then
      dead_agents=$((dead_agents + 1))
      local err_msg=""
      [ -f "$adir/last-error.txt" ] && err_msg=$(tail -1 "$adir/last-error.txt" 2>/dev/null | head -c 120)
      _error "Agent $agent_id: last 5+ log entries are failures${err_msg:+ — $err_msg}"
    fi
  done < <(HEX_DIR="$HEX_DIR" "$hex_agent" agent list 2>/dev/null)

  if [ $dead_agents -eq 0 ]; then
    _pass "All $total_agents agents healthy (env.sh OK, no failure streaks)"
    _rec 21 "agent-liveness" "pass" "all $total_agents agents healthy"
  else
    _rec 21 "agent-liveness" "error" "$dead_agents/$total_agents agents dead"
  fi
}

check_23() {
  if [[ -z "${HEX_DIR:-}" ]]; then
    _error "HEX_DIR not set — add 'export HEX_DIR=\"\$HEX_DIR\"' to your shell rc"
    _rec 23 "hex-dir-set" "error" "HEX_DIR not exported"
    return
  fi
  if [[ ! -d "$HEX_DIR/.hex" ]]; then
    _error "HEX_DIR=$HEX_DIR does not contain .hex/ — wrong path"
    _rec 23 "hex-dir-set" "error" "invalid path"
    return
  fi
  _pass "HEX_DIR=$HEX_DIR"
  _rec 23 "hex-dir-set" "pass" "$HEX_DIR"
}

check_22() {
  local hex_bin="$HEX_DIR/.hex/bin/hex"
  if [ ! -x "$hex_bin" ]; then
    _warn "hex binary not found at $hex_bin (install Rust to enable fleet)"
    _rec 22 "hex-binary-on-path" "warn" "binary missing"
    return
  fi

  local resolved
  resolved="$(command -v hex 2>/dev/null || true)"
  if [ -z "$resolved" ]; then
    _error "'hex' not on PATH — add $HEX_DIR/.hex/bin to PATH in your shell rc"
    _rec 22 "hex-binary-on-path" "error" "not on PATH"
  elif [ "$resolved" = "$hex_bin" ] || [ "$(readlink -f "$resolved" 2>/dev/null)" = "$(readlink -f "$hex_bin" 2>/dev/null)" ]; then
    _pass "hex binary on PATH ($resolved)"
    _rec 22 "hex-binary-on-path" "pass" "$resolved"
  else
    _warn "'hex' resolves to $resolved (expected $hex_bin) — stale alias or wrong PATH order"
    _rec 22 "hex-binary-on-path" "warn" "wrong target: $resolved"
  fi
}

# ─── Main ─────────────────────────────────────────────────────────────────────
# When sourced for unit testing, all functions above are now defined; stop here.
[[ "${DOCTOR_SOURCE_ONLY:-}" == "1" ]] && return 0

if ! $JSON_MODE; then
  echo ""
  echo -e "${BOLD}Hex Doctor — Health Check${RESET}"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo -e "${DIM}HEX_DIR=$HEX_DIR${RESET}"
  echo ""
fi

check_1
check_2
check_3
check_4
check_5
check_6
check_7
check_8
check_9
check_10
check_11
check_12
check_13
check_14
check_15
check_16
check_17
check_18
check_19
check_20
check_21
check_22
check_23

# ─── Output ───────────────────────────────────────────────────────────────────
if $JSON_MODE; then
  OVERALL="pass"
  if $HAS_ERRORS; then
    OVERALL="error"
  elif $HAS_WARNINGS; then
    OVERALL="warning"
  fi

  python3 << PYEOF
import json, sys

checks = []
try:
    with open("$CHECKS_FILE") as f:
        for line in f:
            line = line.rstrip('\n')
            parts = line.split('\t', 3)
            if len(parts) == 4:
                checks.append({
                    'id': int(parts[0]),
                    'name': parts[1],
                    'status': parts[2],
                    'message': parts[3]
                })
except Exception as e:
    sys.stderr.write(f"error reading checks file: {e}\n")

result = {
    'status': '$OVERALL',
    'checks': checks,
    'summary': {
        'pass': $PASS_COUNT,
        'warn': $WARN_COUNT,
        'error': $ERROR_COUNT,
        'fixed': $FIXED_COUNT
    }
}
print(json.dumps(result, indent=2))
PYEOF
else
  echo ""
  echo -e "  ${BOLD}Summary:${RESET} ${PASS_COUNT} passed, ${WARN_COUNT} warnings, ${ERROR_COUNT} errors, ${FIXED_COUNT} fixed"
  echo ""
fi

# ─── Exit code ───────────────────────────────────────────────────────────────
if $HAS_ERRORS; then
  exit 1
elif $HAS_WARNINGS; then
  exit 2
fi
exit 0
