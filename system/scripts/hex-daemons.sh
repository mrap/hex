#!/bin/bash
# hex-daemons.sh — Unified cross-platform daemon management for hex
#
# Wraps systemd (Linux) and launchd (macOS) to manage all hex daemons.
# Claude sessions should only call `status`; never `start`/`stop`/`setup`.
#
# Usage:
#   hex-daemons setup              Install and enable all daemon services
#   hex-daemons status             Show status of all managed daemons
#   hex-daemons start <name>       Start a specific daemon
#   hex-daemons stop <name>        Stop a specific daemon
#   hex-daemons restart <name>     Restart a specific daemon
#   hex-daemons logs <name>        Tail logs for a specific daemon
#   hex-daemons list               List all managed daemon names

set -uo pipefail

# ─── Daemon Registry ─────────────────────────────────────────────────────────
# Format: name|description|exec_start|working_dir
# Edit this registry for your environment. Paths are expanded at runtime.
# Remove entries for services you don't use (e.g., syncthing, boi-poold).
_init_daemons() {
  DAEMONS=(
    "boi-daemon|BOI task orchestrator daemon|python3 $HOME/.boi/src/daemon.py|$HOME/.boi/src"
    "hex-events|hex-events policy engine|$HOME/github.com/mrap/hex-events/venv/bin/python3 $HOME/github.com/mrap/hex-events/hex_eventd.py|$HOME/github.com/mrap/hex-events"
  )
}

# ─── Agent Dir (resolved from script location) ──────────────────────────────
_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_AGENT_DIR="$(cd "$_SCRIPT_DIR/../.." && pwd)"
_LOG_DIR="$_AGENT_DIR/.hex/logs"

# ─── Helpers ──────────────────────────────────────────────────────────────────
GREEN='\033[32m'
YELLOW='\033[33m'
RED='\033[31m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

info()  { echo -e "  ${DIM}>${RESET} $1"; }
ok()    { echo -e "  [${GREEN}OK${RESET}]   $1"; }
warn()  { echo -e "  [${YELLOW}WARN${RESET}] $1"; }
err()   { echo -e "  [${RED}FAIL${RESET}] $1"; }

die() { echo -e "${RED}Error:${RESET} $1" >&2; exit 1; }

# Parse a daemon entry into D_NAME, D_DESC, D_EXEC, D_WDIR
_parse_daemon() {
  local entry="$1"
  D_NAME=$(echo "$entry" | cut -d'|' -f1)
  D_DESC=$(echo "$entry" | cut -d'|' -f2)
  D_EXEC=$(echo "$entry" | cut -d'|' -f3)
  D_WDIR=$(echo "$entry" | cut -d'|' -f4)
}

# Find daemon entry by name, populates D_* vars, returns 0 if found
_find_daemon() {
  local name="$1"
  for entry in "${DAEMONS[@]}"; do
    _parse_daemon "$entry"
    if [[ "$D_NAME" == "$name" ]]; then
      return 0
    fi
  done
  return 1
}

# ─── Platform Detection ──────────────────────────────────────────────────────
_detect_platform() {
  if [[ "$OSTYPE" == linux* ]] && command -v systemctl &>/dev/null; then
    echo "systemd"
  elif [[ "$OSTYPE" == darwin* ]] && command -v launchctl &>/dev/null; then
    echo "launchd"
  else
    echo "unknown"
  fi
}

PLATFORM=$(_detect_platform)

# ─── Systemd Helpers ──────────────────────────────────────────────────────────
_systemd_unit_name() { echo "${1}.service"; }
_systemd_unit_dir()  { echo "$HOME/.config/systemd/user"; }
_systemd_unit_path() { echo "$(_systemd_unit_dir)/$(_systemd_unit_name "$1")"; }

_systemd_is_installed() { [[ -f "$(_systemd_unit_path "$1")" ]]; }
_systemd_is_active()    { systemctl --user is-active "$(_systemd_unit_name "$1")" &>/dev/null; }

_systemd_start()   { systemctl --user start "$(_systemd_unit_name "$1")"; }
_systemd_stop()    { systemctl --user stop "$(_systemd_unit_name "$1")"; }
_systemd_restart() { systemctl --user restart "$(_systemd_unit_name "$1")"; }

_systemd_logs() {
  journalctl --user -u "$(_systemd_unit_name "$1")" -f --no-pager -n 50
}

_systemd_install() {
  local name="$1"
  _find_daemon "$name" || die "Unknown daemon: $name"

  local unit_dir
  unit_dir=$(_systemd_unit_dir)
  mkdir -p "$unit_dir"

  local script_dir
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  local template_dir="$script_dir/../templates/systemd"
  local unit_file="$unit_dir/$(_systemd_unit_name "$name")"

  if [[ -d "$template_dir" ]] && [[ -f "$template_dir/$(_systemd_unit_name "$name")" ]]; then
    local python3_path; python3_path="$(command -v python3)"
    local syncthing_path; syncthing_path="$(command -v syncthing 2>/dev/null || echo /usr/bin/syncthing)"
    sed -e "s|%h|$HOME|g" \
        -e "s|PYTHON3_PLACEHOLDER|$python3_path|g" \
        -e "s|SYNCTHING_PLACEHOLDER|$syncthing_path|g" \
        "$template_dir/$(_systemd_unit_name "$name")" > "$unit_file"
  else
    local log_dir="$_LOG_DIR"
    cat > "$unit_file" <<EOF
[Unit]
Description=$D_DESC
After=network.target

[Service]
Type=simple
ExecStart=$D_EXEC
WorkingDirectory=$D_WDIR
Restart=on-failure
RestartSec=10
StandardOutput=append:${log_dir}/${name}.log
StandardError=append:${log_dir}/${name}.log

[Install]
WantedBy=default.target
EOF
  fi

  systemctl --user daemon-reload
  systemctl --user enable "$(_systemd_unit_name "$name")" 2>/dev/null
}

# ─── Launchd Helpers ──────────────────────────────────────────────────────────
_launchd_label()      { echo "com.hex.${1}"; }
_launchd_plist_dir()  { echo "$HOME/Library/LaunchAgents"; }
_launchd_plist_path() { echo "$(_launchd_plist_dir)/$(_launchd_label "$1").plist"; }

_launchd_is_installed() { [[ -f "$(_launchd_plist_path "$1")" ]]; }
_launchd_is_active()    { launchctl list "$(_launchd_label "$1")" 2>/dev/null | grep -q "PID"; }

_launchd_start()   { launchctl load -w "$(_launchd_plist_path "$1")"; }
_launchd_stop()    { launchctl unload "$(_launchd_plist_path "$1")"; }
_launchd_restart() { _launchd_stop "$1" 2>/dev/null || true; _launchd_start "$1"; }

_launchd_logs() {
  local log_file="$_LOG_DIR/${1}.log"
  if [[ -f "$log_file" ]]; then
    tail -f "$log_file"
  else
    echo "No log file at $log_file"
  fi
}

_launchd_install() {
  local name="$1"
  _find_daemon "$name" || die "Unknown daemon: $name"

  local plist_dir
  plist_dir=$(_launchd_plist_dir)
  mkdir -p "$plist_dir"

  local script_dir
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  local template_dir="$script_dir/../templates/launchd"
  local plist_file="$plist_dir/$(_launchd_label "$name").plist"

  if [[ -d "$template_dir" ]] && [[ -f "$template_dir/$(_launchd_label "$name").plist" ]]; then
    local python3_path; python3_path="$(command -v python3)"
    local syncthing_path; syncthing_path="$(command -v syncthing 2>/dev/null || echo syncthing)"
    sed -e "s|HOME_PLACEHOLDER|$HOME|g" \
        -e "s|PYTHON3_PLACEHOLDER|$python3_path|g" \
        -e "s|SYNCTHING_PLACEHOLDER|$syncthing_path|g" \
        "$template_dir/$(_launchd_label "$name").plist" > "$plist_file"
  else
    local log_dir="$_LOG_DIR"
    IFS=' ' read -ra exec_parts <<< "$D_EXEC"
    local prog_args=""
    for part in "${exec_parts[@]}"; do
      prog_args+="      <string>${part}</string>"$'\n'
    done

    cat > "$plist_file" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$(_launchd_label "$name")</string>
    <key>ProgramArguments</key>
    <array>
${prog_args}    </array>
    <key>WorkingDirectory</key>
    <string>$D_WDIR</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>${log_dir}/${name}.log</string>
    <key>StandardErrorPath</key>
    <string>${log_dir}/${name}.log</string>
</dict>
</plist>
EOF
  fi
}

# ─── Platform-Agnostic Wrappers ──────────────────────────────────────────────
_is_installed() {
  case "$PLATFORM" in
    systemd) _systemd_is_installed "$1" ;;
    launchd) _launchd_is_installed "$1" ;;
    *) return 1 ;;
  esac
}

_is_active() {
  case "$PLATFORM" in
    systemd) _systemd_is_active "$1" ;;
    launchd) _launchd_is_active "$1" ;;
    *) pgrep -f "$D_EXEC" &>/dev/null ;;
  esac
}

_install() {
  case "$PLATFORM" in
    systemd) _systemd_install "$1" ;;
    launchd) _launchd_install "$1" ;;
    *) die "Unsupported platform: $PLATFORM" ;;
  esac
}

_start() {
  case "$PLATFORM" in
    systemd) _systemd_start "$1" ;;
    launchd) _launchd_start "$1" ;;
    *) die "Unsupported platform: $PLATFORM" ;;
  esac
}

_stop() {
  case "$PLATFORM" in
    systemd) _systemd_stop "$1" ;;
    launchd) _launchd_stop "$1" ;;
    *) die "Unsupported platform: $PLATFORM" ;;
  esac
}

_restart() {
  case "$PLATFORM" in
    systemd) _systemd_restart "$1" ;;
    launchd) _launchd_restart "$1" ;;
    *) die "Unsupported platform: $PLATFORM" ;;
  esac
}

_logs() {
  case "$PLATFORM" in
    systemd) _systemd_logs "$1" ;;
    launchd) _launchd_logs "$1" ;;
    *) die "Unsupported platform: $PLATFORM" ;;
  esac
}

# ─── Commands ─────────────────────────────────────────────────────────────────
cmd_setup() {
  echo -e "${BOLD}hex-daemons setup${RESET} (platform: $PLATFORM)"
  echo ""

  mkdir -p "$_LOG_DIR"

  # Enable linger on Linux for user services to persist after logout
  if [[ "$PLATFORM" == "systemd" ]]; then
    if command -v loginctl &>/dev/null; then
      if ! loginctl show-user "$USER" 2>/dev/null | grep -q "Linger=yes"; then
        info "Enabling linger for user $USER..."
        loginctl enable-linger "$USER" 2>/dev/null || warn "Could not enable linger (may need sudo)"
      fi
    fi
  fi

  for entry in "${DAEMONS[@]}"; do
    _parse_daemon "$entry"

    # Check if the binary/script exists
    local exec_bin
    exec_bin=$(echo "$D_EXEC" | awk '{print $1}')
    local exec_target
    exec_target=$(echo "$D_EXEC" | awk '{print $2}')

    if [[ "$exec_bin" == "python3" ]] && [[ -n "${exec_target:-}" ]] && [[ ! -f "$exec_target" ]]; then
      warn "$D_NAME: script not found at $exec_target (skipping)"
      continue
    elif [[ "$exec_bin" != "python3" ]] && ! command -v "$exec_bin" &>/dev/null; then
      warn "$D_NAME: binary not found: $exec_bin (skipping)"
      continue
    fi

    _install "$D_NAME"
    ok "$D_NAME installed and enabled"
  done

  if [[ "$PLATFORM" == "systemd" ]]; then
    systemctl --user daemon-reload
    info "systemd daemon-reload complete"
  fi

  echo ""
  echo -e "${BOLD}Setup complete.${RESET} Start daemons with: hex-daemons start <name>"
}

cmd_status() {
  echo -e "${BOLD}hex-daemons status${RESET} (platform: $PLATFORM)"
  echo ""

  local all_ok=true

  for entry in "${DAEMONS[@]}"; do
    _parse_daemon "$entry"

    if _is_installed "$D_NAME"; then
      if _is_active "$D_NAME"; then
        ok "$D_NAME: running"
      else
        warn "$D_NAME: installed but NOT running"
        all_ok=false
      fi
    else
      info "$D_NAME: not installed (run: hex-daemons setup)"
      all_ok=false
    fi
  done

  echo ""
  if $all_ok; then
    echo -e "  ${GREEN}All daemons healthy.${RESET}"
  else
    echo -e "  ${YELLOW}Some daemons need attention.${RESET}"
  fi
}

cmd_start() {
  local name="${1:-}"
  [[ -z "$name" ]] && die "Usage: hex-daemons start <name>"
  _find_daemon "$name" || die "Unknown daemon: $name. Run 'hex-daemons list' for available daemons."

  if ! _is_installed "$name"; then
    die "$name not installed. Run: hex-daemons setup"
  fi

  _start "$name"
  ok "$name started"
}

cmd_stop() {
  local name="${1:-}"
  [[ -z "$name" ]] && die "Usage: hex-daemons stop <name>"
  _find_daemon "$name" || die "Unknown daemon: $name"
  _stop "$name"
  ok "$name stopped"
}

cmd_restart() {
  local name="${1:-}"
  [[ -z "$name" ]] && die "Usage: hex-daemons restart <name>"
  _find_daemon "$name" || die "Unknown daemon: $name"
  _restart "$name"
  ok "$name restarted"
}

cmd_logs() {
  local name="${1:-}"
  [[ -z "$name" ]] && die "Usage: hex-daemons logs <name>"
  _find_daemon "$name" || die "Unknown daemon: $name"
  _logs "$name"
}

cmd_list() {
  echo -e "${BOLD}Managed daemons:${RESET}"
  for entry in "${DAEMONS[@]}"; do
    _parse_daemon "$entry"
    echo "  $D_NAME — $D_DESC"
  done
}

# ─── Main ─────────────────────────────────────────────────────────────────────
main() {
  _init_daemons

  local cmd="${1:-}"
  shift 2>/dev/null || true

  case "$cmd" in
    setup)   cmd_setup ;;
    status)  cmd_status ;;
    start)   cmd_start "$@" ;;
    stop)    cmd_stop "$@" ;;
    restart) cmd_restart "$@" ;;
    logs)    cmd_logs "$@" ;;
    list)    cmd_list ;;
    ""|--help|-h)
      echo "Usage: hex-daemons <command> [args]"
      echo ""
      echo "Commands:"
      echo "  setup              Install and enable all daemon services"
      echo "  status             Show status of all managed daemons"
      echo "  start <name>       Start a specific daemon"
      echo "  stop <name>        Stop a specific daemon"
      echo "  restart <name>     Restart a specific daemon"
      echo "  logs <name>        Tail logs for a specific daemon"
      echo "  list               List all managed daemon names"
      echo ""
      echo "Platform: $PLATFORM"
      ;;
    *)
      die "Unknown command: $cmd. Run 'hex-daemons --help' for usage."
      ;;
  esac
}

main "$@"
