#!/bin/bash
# doctor-checks/codex.sh — Codex CLI + config health checks
#
# Sourced by doctor.sh (or run via run-codex-checks.sh). Requires:
#   FIX    — true/false whether auto-fix is enabled
#   SMOKE  — true/false whether smoke tests are enabled
#   _pass / _warn / _error / _fixed / _rec / _info — output helpers
#
# Integration: source this file, then call run_codex_checks()

set -uo pipefail

# Minimum sections AGENTS.md must contain to be considered complete
CODEX_AGENTS_MD_REQUIRED_SECTIONS=(
  "Standing Orders"
  "Session Lifecycle"
  "BOI"
  "Memory"
)

# check_codex_1: codex binary on PATH
check_codex_1() {
  if command -v codex &>/dev/null; then
    local codex_path
    codex_path="$(command -v codex)"
    _pass "codex.cli-on-path: found at $codex_path"
    _rec 51 "codex.cli-on-path" "pass" "found at $codex_path"
  else
    _error "codex.cli-on-path: codex CLI not found — install with: npm install -g @openai/codex"
    _rec 51 "codex.cli-on-path" "error" "codex not on PATH"
  fi
}

# check_codex_2: codex --version exits 0
check_codex_2() {
  if ! command -v codex &>/dev/null; then
    _info "codex.version-ok: skipped (codex not on PATH)"
    return
  fi

  local version_out
  if version_out=$(codex --version 2>&1); then
    local ver
    ver=$(echo "$version_out" | head -1 | tr -d '[:space:]')
    _pass "codex.version-ok: $ver"
    _rec 52 "codex.version-ok" "pass" "$ver"
  else
    _error "codex.version-ok: 'codex --version' exited non-zero — fix: reinstall codex CLI"
    _rec 52 "codex.version-ok" "error" "'codex --version' failed"
  fi
}

# check_codex_3: OPENAI_API_KEY is set (env or ~/.hex-test.env)
check_codex_3() {
  local key_source=""

  if [ -n "${OPENAI_API_KEY:-}" ]; then
    key_source="environment variable"
  elif [ -f "$HOME/.hex-test.env" ] && grep -q "OPENAI_API_KEY=" "$HOME/.hex-test.env" 2>/dev/null; then
    key_source="~/.hex-test.env"
  fi

  if [ -n "$key_source" ]; then
    _pass "codex.api-key: OPENAI_API_KEY found via $key_source"
    _rec 53 "codex.api-key" "pass" "OPENAI_API_KEY present ($key_source)"
  else
    _warn "codex.api-key: OPENAI_API_KEY not set — Codex will fail at runtime"
    _info "  Fix: export OPENAI_API_KEY=sk-... in env.sh or add to ~/.hex-test.env"
    _rec 53 "codex.api-key" "warn" "OPENAI_API_KEY not found in env or ~/.hex-test.env"
  fi
}

# check_codex_4: AGENTS.md exists at $HEX_DIR/AGENTS.md
check_codex_4() {
  local agent_dir="${HEX_DIR:-$HEX_DIR}"
  local agents_md="$agent_dir/AGENTS.md"

  if [ -f "$agents_md" ]; then
    local size
    size=$(wc -c < "$agents_md" | tr -d '[:space:]')
    _pass "codex.agents-md-exists: AGENTS.md found ($size bytes)"
    _rec 54 "codex.agents-md-exists" "pass" "AGENTS.md found at $agents_md ($size bytes)"
  else
    _warn "codex.agents-md-exists: AGENTS.md missing at $agents_md"
    _info "  Fix: create AGENTS.md — Codex reads this as its primary instruction file"
    _rec 54 "codex.agents-md-exists" "warn" "AGENTS.md not found at $agents_md"
  fi
}

# check_codex_5: AGENTS.md contains minimum required sections
check_codex_5() {
  local agent_dir="${HEX_DIR:-$HEX_DIR}"
  local agents_md="$agent_dir/AGENTS.md"

  if [ ! -f "$agents_md" ]; then
    _info "codex.agents-md-complete: skipped (AGENTS.md not present)"
    return
  fi

  local missing_sections=()
  local section
  for section in "${CODEX_AGENTS_MD_REQUIRED_SECTIONS[@]}"; do
    if ! grep -qi "$section" "$agents_md" 2>/dev/null; then
      missing_sections+=("$section")
    fi
  done

  if [ ${#missing_sections[@]} -eq 0 ]; then
    _pass "codex.agents-md-complete: all required sections present"
    _rec 55 "codex.agents-md-complete" "pass" "all required sections found"
  else
    local missing_list
    missing_list=$(printf '"%s" ' "${missing_sections[@]}")
    _warn "codex.agents-md-complete: AGENTS.md missing sections: $missing_list"
    _info "  Required sections: ${CODEX_AGENTS_MD_REQUIRED_SECTIONS[*]}"
    _rec 55 "codex.agents-md-complete" "warn" "missing sections: $missing_list"
  fi
}

# run_codex_checks — entry point, runs all Codex checks in order
run_codex_checks() {
  check_codex_1
  check_codex_2
  check_codex_3
  check_codex_4
  check_codex_5
}
