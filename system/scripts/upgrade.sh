#!/bin/bash
# sync-safe
# upgrade.sh — Pull latest hex-foundation and upgrade local installation
#
# Upgrades: scripts, skills, commands, hooks, settings.json
# Preserves: memory.db, settings.local.json, user data, CLAUDE.md
#
# Usage:
#   upgrade.sh                  # Upgrade from configured repo
#   upgrade.sh --dry-run        # Show what would change without applying
#   upgrade.sh --repo URL       # Override repo URL
#   upgrade.sh --local PATH     # Use a local hex-foundation checkout

set -uo pipefail

# ─── Resolve paths ───────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HEX_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
HEX_DOTDIR="$HEX_DIR/.hex"
CACHE_DIR="$HEX_DOTDIR/.upgrade-cache"
CONFIG_FILE="$HEX_DOTDIR/upgrade.json"

# Source path-mapping library for v1/v2 layout handling
# shellcheck source=./path-mapping.sh
source "$SCRIPT_DIR/path-mapping.sh"

# These are populated after we know the source layout.
SOURCE_LAYOUT=""
SOURCE_SUBDIR_SCRIPTS=""
SOURCE_SUBDIR_SKILLS=""
SOURCE_SUBDIR_COMMANDS=""
SOURCE_SUBDIR_HOOKS=""
SOURCE_CLAUDE_MD_PATH=""

# Colors
GREEN='\033[32m'
YELLOW='\033[33m'
RED='\033[31m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

# Defaults
DEFAULT_REPO="https://github.com/mrap/hex-foundation.git"
DRY_RUN=false
LOCAL_PATH=""
REPO_URL=""
COMPONENT_WARNINGS=0

# ─── Helpers ─────────────────────────────────────────────────────────────────
pass()   { echo -e "  [${GREEN}OK${RESET}] $1"; }
warn()   { echo -e "  [${YELLOW}WARN${RESET}] $1"; }
fail()   { echo -e "  [${RED}FAIL${RESET}] $1"; }
info()   { echo -e "  ${DIM}→${RESET} $1"; }
header() { echo -e "\n${BOLD}$1${RESET}"; }

portable_sha256() {
  if command -v sha256sum &>/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum &>/dev/null; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  fi
}

# ─── Parse arguments ────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)  DRY_RUN=true; shift ;;
    --repo)     REPO_URL="${2:-}"; shift 2 ;;
    --local)    LOCAL_PATH="${2:-}"; shift 2 ;;
    --help|-h)
      echo "Usage: upgrade.sh [--dry-run] [--repo URL] [--local PATH]"
      echo ""
      echo "Options:"
      echo "  --dry-run    Show what would change without applying"
      echo "  --repo URL   Override repo URL"
      echo "  --local PATH Use a local hex-foundation checkout"
      exit 0
      ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

# ─── Load config ────────────────────────────────────────────────────────────
if [ -z "$REPO_URL" ] && [ -f "$CONFIG_FILE" ]; then
  REPO_URL=$(python3 -c "import json; print(json.load(open('$CONFIG_FILE')).get('repo', ''))" 2>/dev/null || echo "")
fi
REPO_URL="${REPO_URL:-$DEFAULT_REPO}"

echo ""
echo "════════════════════════════════════════════════════"
echo " Hexagon Upgrade — $(date '+%Y-%m-%d %H:%M')"
echo "════════════════════════════════════════════════════"
if $DRY_RUN; then
  echo -e "  ${YELLOW}${BOLD}[DRY RUN]${RESET} No changes will be made."
fi
echo ""

# ─── Step 1: Get latest source ──────────────────────────────────────────────
header "1. Get Latest Source"

SOURCE_DIR=""

if [ -n "$LOCAL_PATH" ]; then
  # Use local checkout — accept v1 (dot-claude/) or v2 (system/ + templates/) layout
  if [ ! -d "$LOCAL_PATH/dot-claude" ] && ! ([ -d "$LOCAL_PATH/system" ] && [ -f "$LOCAL_PATH/templates/CLAUDE.md" ]); then
    fail "No recognized hex layout at $LOCAL_PATH (expected dot-claude/ for v1, or system/ + templates/CLAUDE.md for v2)"
    exit 1
  fi
  SOURCE_DIR="$LOCAL_PATH"
  pass "Using local checkout: $LOCAL_PATH"
else
  # Clone or pull from remote
  if [ -d "$CACHE_DIR/.git" ]; then
    info "Pulling latest from $REPO_URL"
    PULL_OUT=$(cd "$CACHE_DIR" && git pull --ff-only 2>&1) || {
      warn "Fast-forward pull failed. Re-cloning."
      rm -rf "$CACHE_DIR"
    }
    if [ -d "$CACHE_DIR/.git" ]; then
      if echo "$PULL_OUT" | grep -q "Already up to date"; then
        info "Already up to date"
      else
        info "$PULL_OUT"
      fi
    fi
  fi

  if [ ! -d "$CACHE_DIR/.git" ]; then
    info "Cloning $REPO_URL"
    git clone --depth 1 "$REPO_URL" "$CACHE_DIR" 2>&1 | while read -r line; do
      info "$line"
    done
    if [ ! -d "$CACHE_DIR/dot-claude" ] && ! ([ -d "$CACHE_DIR/system" ] && [ -f "$CACHE_DIR/templates/CLAUDE.md" ]); then
      fail "Clone succeeded but no recognized hex layout found. Wrong repo?"
      exit 1
    fi
  fi

  SOURCE_DIR="$CACHE_DIR"
  pass "Source ready"
fi

# Detect source layout and populate layout-specific path variables.
SOURCE_LAYOUT=$(detect_layout "$SOURCE_DIR")
if [ "$SOURCE_LAYOUT" = "v1" ]; then
  SOURCE_SUBDIR_SCRIPTS="dot-claude/scripts"
  SOURCE_SUBDIR_SKILLS="dot-claude/skills"
  SOURCE_SUBDIR_COMMANDS="dot-claude/commands"
  SOURCE_SUBDIR_HOOKS="dot-claude/hooks"
  SOURCE_CLAUDE_MD_PATH="CLAUDE.md"
elif [ "$SOURCE_LAYOUT" = "v2" ]; then
  SOURCE_SUBDIR_SCRIPTS="system/scripts"
  SOURCE_SUBDIR_SKILLS="system/skills"
  SOURCE_SUBDIR_COMMANDS="system/commands"
  SOURCE_SUBDIR_HOOKS="system/hooks"
  SOURCE_CLAUDE_MD_PATH="templates/CLAUDE.md"
else
  fail "Unknown source layout at $SOURCE_DIR (not v1 or v2)"
  exit 1
fi
info "Source layout: $SOURCE_LAYOUT"

# ─── Step 2: Core Components (hex-events + BOI) ─────────────────────────────
header "2. Core Components"

HEX_EVENTS_REPO="https://github.com/mrap/hex-events.git"
HEX_EVENTS_DIR="$HOME/.hex-events"
HEX_EVENTS_SRC="${HEX_EVENTS_SRC:-$HOME/github.com/mrap/hex-events}"
BOI_INSTALLER_URL="https://raw.githubusercontent.com/mrap/boi/main/install-public.sh"

# --- Ensure Python 3.10+ is available (auto-install via uv if needed) ---
PYTHON_OK=false

# Check if system Python is sufficient
check_system_python() {
  local cmd=""
  if command -v python3 &>/dev/null; then
    cmd="python3"
  elif command -v python &>/dev/null; then
    cmd="python"
  fi

  if [ -n "$cmd" ]; then
    local ver
    ver=$("$cmd" -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')" 2>/dev/null)
    local major minor
    major=$(echo "$ver" | cut -d. -f1)
    minor=$(echo "$ver" | cut -d. -f2)
    if [ "${major:-0}" -ge 3 ] && [ "${minor:-0}" -ge 10 ]; then
      echo "$ver"
      return 0
    fi
  fi
  return 1
}

# Install uv (Python toolchain manager) if not present
ensure_uv() {
  if command -v uv &>/dev/null; then
    return 0
  fi
  # Check common install locations
  if [ -f "$HOME/.local/bin/uv" ]; then
    export PATH="$HOME/.local/bin:$PATH"
    return 0
  fi
  if [ -f "$HOME/.cargo/bin/uv" ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
    return 0
  fi

  info "Installing uv (Python toolchain manager)..."
  if curl -LsSf https://astral.sh/uv/install.sh 2>/dev/null | sh 2>&1 | while read -r line; do info "  $line"; done; then
    # uv installer puts binary in ~/.local/bin or ~/.cargo/bin
    if [ -f "$HOME/.local/bin/uv" ]; then
      export PATH="$HOME/.local/bin:$PATH"
    elif [ -f "$HOME/.cargo/bin/uv" ]; then
      export PATH="$HOME/.cargo/bin:$PATH"
    fi
    if command -v uv &>/dev/null; then
      pass "uv installed"
      return 0
    fi
  fi
  return 1
}

# Install Python 3.12 via uv and put it on PATH
install_python_via_uv() {
  if ! ensure_uv; then
    fail "Could not install uv. Cannot auto-install Python."
    return 1
  fi

  info "Installing Python 3.12 via uv..."
  if uv python install 3.12 2>&1 | while read -r line; do info "  $line"; done; then
    # Get the path to the installed Python
    local py_path
    py_path=$(uv python find 3.12 2>/dev/null)
    if [ -n "$py_path" ] && [ -f "$py_path" ]; then
      # Add the Python's directory to PATH so downstream installers find it as 'python3'
      local py_dir
      py_dir=$(dirname "$py_path")
      export PATH="$py_dir:$PATH"
      pass "Python 3.12 installed at $py_path"
      return 0
    fi
  fi
  fail "Failed to install Python 3.12 via uv"
  return 1
}

# Main Python resolution
if PY_VER=$(check_system_python); then
  PYTHON_OK=true
  info "Python $PY_VER"
else
  # System Python is missing or too old — auto-install via uv
  if [ -n "$PY_VER" ]; then
    warn "System Python is $PY_VER (need 3.10+). Auto-installing Python 3.12..."
  else
    warn "No Python found. Auto-installing Python 3.12..."
  fi

  if $DRY_RUN; then
    info "[dry-run] Would install uv and Python 3.12"
    PYTHON_OK=true  # assume it would work for dry-run
  else
    if install_python_via_uv; then
      # Verify it worked
      if PY_VER=$(check_system_python); then
        PYTHON_OK=true
      fi
    fi
  fi

  if ! $PYTHON_OK; then
    fail "Could not get Python 3.10+. hex-events and BOI require it."
    echo ""
    echo -e "  ${YELLOW}${BOLD}Manual fix:${RESET}"
    echo -e "  ${DIM}macOS:${RESET}  brew install python@3.12"
    echo -e "  ${DIM}Linux:${RESET}  sudo apt install python3.12"
    echo -e "  ${DIM}Any:${RESET}    curl -LsSf https://astral.sh/uv/install.sh | sh && uv python install 3.12"
    echo ""
    echo -e "  hex-events and BOI are ${BOLD}core components${RESET}, not optional."
    echo -e "  Without them, hex cannot run background automation or dispatch parallel work."
    echo ""
  fi
fi

# --- Verification helpers ---

# Verify hex-events is functional after install/update.
# Sets COMPONENT_WARNINGS+=1 on failure (non-fatal, matches bootstrap.sh behavior).
verify_hex_events() {
  # Find hex-events binary: prefer PATH, fall back to venv location
  local hex_events_bin=""
  if command -v hex-events &>/dev/null; then
    hex_events_bin="hex-events"
  elif [ -x "$HEX_EVENTS_DIR/venv/bin/hex-events" ]; then
    hex_events_bin="$HEX_EVENTS_DIR/venv/bin/hex-events"
  fi

  if [ -z "$hex_events_bin" ]; then
    warn "hex-events binary not found on PATH or at $HEX_EVENTS_DIR/venv/bin/hex-events — skipping verification"
    COMPONENT_WARNINGS=$((COMPONENT_WARNINGS + 1))
    return
  fi

  local verify_failed=false

  if "$hex_events_bin" validate &>/dev/null; then
    info "hex-events validate: OK"
  else
    warn "hex-events validate failed"
    verify_failed=true
  fi

  if "$hex_events_bin" status &>/dev/null; then
    info "hex-events status: OK"
  else
    warn "hex-events status failed"
    verify_failed=true
  fi

  if $verify_failed; then
    warn "hex-events verification failed — install may be incomplete (non-fatal)"
    COMPONENT_WARNINGS=$((COMPONENT_WARNINGS + 1))
    return 1
  else
    pass "hex-events verified"
    return 0
  fi
}

# Verify BOI is functional after install/update.
# Sets COMPONENT_WARNINGS+=1 on failure (BOI is optional).
verify_boi() {
  if [ ! -f "$HOME/.boi/src/boi.sh" ]; then
    return  # BOI not installed, nothing to verify
  fi

  if bash "$HOME/.boi/src/boi.sh" status &>/dev/null; then
    pass "BOI verified"
    return 0
  else
    warn "boi status failed — BOI install may be incomplete (non-fatal)"
    COMPONENT_WARNINGS=$((COMPONENT_WARNINGS + 1))
    return 1
  fi
}

# --- hex-events ---
install_hex_events() {
  if [ -f "$HEX_EVENTS_DIR/hex_eventd.py" ] || [ -f "$HEX_EVENTS_SRC/hex_eventd.py" ]; then
    # Already installed — pull latest and re-run installer
    local src_dir=""
    if [ -d "$HEX_EVENTS_SRC/.git" ]; then
      src_dir="$HEX_EVENTS_SRC"
    elif [ -L "$HEX_EVENTS_DIR" ] && [ -d "$(readlink "$HEX_EVENTS_DIR")/.git" ]; then
      src_dir="$(readlink "$HEX_EVENTS_DIR")"
    elif [ -d "$HEX_EVENTS_DIR/.git" ]; then
      src_dir="$HEX_EVENTS_DIR"
    fi

    if [ -n "$src_dir" ]; then
      # Snapshot current commit SHA for rollback if upgrade verification fails.
      # Assumption: hex-events DB schema changes are additive (no destructive migrations),
      # so git reset + install.sh is sufficient to restore a working state.
      local SAVED_SHA=""
      SAVED_SHA=$(git -C "$src_dir" rev-parse HEAD 2>/dev/null || echo "")
      info "Updating hex-events at $src_dir (snapshot: ${SAVED_SHA:0:8})"
      (cd "$src_dir" && git pull --ff-only 2>&1 | head -3) || warn "hex-events git pull failed (non-fatal)"
      if [ -f "$src_dir/install.sh" ]; then
        if $DRY_RUN; then
          info "[dry-run] Would re-run hex-events install.sh"
        else
          bash "$src_dir/install.sh" 2>&1 | while read -r line; do info "  $line"; done
          pass "hex-events updated"
          if ! verify_hex_events; then
            # Verification failed — roll back to the pre-upgrade commit
            if [ -n "$SAVED_SHA" ]; then
              warn "Rolling back hex-events to $SAVED_SHA"
              git -C "$src_dir" reset --hard "$SAVED_SHA" 2>&1 | head -3 || true
              # Re-run install.sh to rebuild venv and LaunchAgents against the rolled-back code
              if [ -f "$src_dir/install.sh" ]; then
                bash "$src_dir/install.sh" 2>&1 | while read -r line; do info "  $line"; done
              fi
              # On macOS: reload the LaunchAgent so the daemon restarts with rolled-back code
              if [[ "$(uname)" == "Darwin" ]]; then
                local plist="$HOME/Library/LaunchAgents/com.hex-events.plist"
                if [ -f "$plist" ]; then
                  launchctl unload "$plist" 2>/dev/null || true
                  launchctl load  "$plist" 2>/dev/null || true
                  info "hex-events LaunchAgent reloaded"
                fi
              fi
              info "hex-events rolled back to $SAVED_SHA"
            fi
          fi
        fi
      fi
    else
      pass "hex-events present (no git repo found for update)"
      verify_hex_events
    fi
  else
    # Fresh install
    info "hex-events not found. Installing..."
    if $DRY_RUN; then
      info "[dry-run] Would clone $HEX_EVENTS_REPO and run install.sh"
    else
      mkdir -p "$(dirname "$HEX_EVENTS_SRC")"
      if git clone "$HEX_EVENTS_REPO" "$HEX_EVENTS_SRC" 2>&1 | while read -r line; do info "  $line"; done; then
        if [ -f "$HEX_EVENTS_SRC/install.sh" ]; then
          bash "$HEX_EVENTS_SRC/install.sh" 2>&1 | while read -r line; do info "  $line"; done
          pass "hex-events installed"
          verify_hex_events
        else
          fail "hex-events cloned but no install.sh found"
          COMPONENT_WARNINGS=$((COMPONENT_WARNINGS + 1))
        fi
      else
        fail "Failed to clone hex-events (non-fatal, continuing)"
        COMPONENT_WARNINGS=$((COMPONENT_WARNINGS + 1))
      fi
    fi
  fi
}

# --- BOI ---
install_boi() {
  if [ -f "$HOME/.boi/src/boi.sh" ] || [ -f "$HOME/.boi/config.json" ]; then
    # Already installed — run public installer in update mode
    if [ -f "$HOME/.boi/src/install-public.sh" ]; then
      info "Updating BOI"
      # Snapshot current BOI src commit for best-effort rollback (BOI is optional)
      local boi_saved_sha=""
      if [ -d "$HOME/.boi/src/.git" ]; then
        boi_saved_sha=$(git -C "$HOME/.boi/src" rev-parse HEAD 2>/dev/null || echo "")
      fi
      if $DRY_RUN; then
        info "[dry-run] Would re-run BOI install-public.sh --update"
      else
        BOI_CONTEXT_ROOT="${HEX_DIR:?HEX_DIR must be set for BOI context_root}" bash "$HOME/.boi/src/install-public.sh" --update 2>&1 | while read -r line; do info "  $line"; done
        pass "BOI updated"
        if ! verify_boi; then
          # Best-effort rollback — BOI is optional so this is non-fatal
          if [ -n "$boi_saved_sha" ]; then
            warn "Rolling back BOI to $boi_saved_sha"
            git -C "$HOME/.boi/src" reset --hard "$boi_saved_sha" 2>&1 | head -3 || true
            info "BOI rolled back to $boi_saved_sha"
            info "Note: BOI venv/config may need manual repair. Run: bash ~/.boi/src/install-public.sh"
          fi
        fi
      fi
    else
      pass "BOI present (no installer found for update)"
      verify_boi
    fi
  else
    # Fresh install
    info "BOI not found. Installing..."
    if $DRY_RUN; then
      info "[dry-run] Would run BOI public installer"
    else
      local tmp_installer
      tmp_installer=$(mktemp /tmp/boi-install-XXXXXX.sh)
      if curl -fsSL "$BOI_INSTALLER_URL" -o "$tmp_installer" 2>/dev/null; then
        BOI_CONTEXT_ROOT="${HEX_DIR:?HEX_DIR must be set for BOI context_root}" bash "$tmp_installer" 2>&1 | while read -r line; do info "  $line"; done
        rm -f "$tmp_installer"
        pass "BOI installed"
        verify_boi
      else
        rm -f "$tmp_installer"
        fail "Failed to download BOI installer (non-fatal, continuing)"
        COMPONENT_WARNINGS=$((COMPONENT_WARNINGS + 1))
      fi
    fi
  fi
}

if $PYTHON_OK; then
  install_hex_events
  install_boi
else
  warn "Skipping hex-events and BOI installation (Python 3.10+ required)"
fi

# ─── Step 3: Detect what will change ────────────────────────────────────────
header "3. Detect Changes"

# Build list of files that would be updated
CHANGED=0
NEW=0
UNCHANGED=0
CHANGES_LOG=""

# Compare source layout contents against .hex/
# We enumerate the three canonical subdirs (scripts, skills, commands) so we
# don't accidentally sync v1-only dirs (ui, evolution, boi-scripts, hex-events-policies).
while IFS= read -r src_file; do
  # Compute the relative path within the source subdir, then strip it to
  # the bare path that maps into $HEX_DOTDIR.
  if [[ "$src_file" == "$SOURCE_DIR/$SOURCE_SUBDIR_SCRIPTS/"* ]]; then
    rel_path="scripts/${src_file#"$SOURCE_DIR"/"$SOURCE_SUBDIR_SCRIPTS"/}"
  elif [[ "$src_file" == "$SOURCE_DIR/$SOURCE_SUBDIR_SKILLS/"* ]]; then
    rel_path="skills/${src_file#"$SOURCE_DIR"/"$SOURCE_SUBDIR_SKILLS"/}"
  elif [[ "$src_file" == "$SOURCE_DIR/$SOURCE_SUBDIR_COMMANDS/"* ]]; then
    rel_path="commands/${src_file#"$SOURCE_DIR"/"$SOURCE_SUBDIR_COMMANDS"/}"
  elif [[ "$src_file" == "$SOURCE_DIR/$SOURCE_SUBDIR_HOOKS/"* ]]; then
    rel_path="hooks/${src_file#"$SOURCE_DIR"/"$SOURCE_SUBDIR_HOOKS"/}"
  else
    continue
  fi

  # Skip files we preserve
  case "$rel_path" in
    settings.local.json) continue ;;
    __pycache__/*) continue ;;
  esac

  dst_file="$HEX_DOTDIR/$rel_path"

  if [ ! -f "$dst_file" ]; then
    NEW=$((NEW + 1))
    CHANGES_LOG="${CHANGES_LOG}  + ${rel_path}\n"
  elif ! diff -q "$src_file" "$dst_file" > /dev/null 2>&1; then
    CHANGED=$((CHANGED + 1))
    CHANGES_LOG="${CHANGES_LOG}  ~ ${rel_path}\n"
  else
    UNCHANGED=$((UNCHANGED + 1))
  fi
done < <(
  for _subdir in "$SOURCE_SUBDIR_SCRIPTS" "$SOURCE_SUBDIR_SKILLS" "$SOURCE_SUBDIR_COMMANDS"; do
    [ -d "$SOURCE_DIR/$_subdir" ] && find "$SOURCE_DIR/$_subdir" -type f ! -path "*/__pycache__/*"
  done
  [ -d "$SOURCE_DIR/$SOURCE_SUBDIR_HOOKS" ] && find "$SOURCE_DIR/$SOURCE_SUBDIR_HOOKS" -type f ! -path "*/__pycache__/*"
)

info "$CHANGED changed, $NEW new, $UNCHANGED unchanged"

if [ -n "$CHANGES_LOG" ]; then
  echo -e "$CHANGES_LOG"
fi

# Check CLAUDE.md template changes
TEMPLATE_CHANGED=false
if [ -f "$SOURCE_DIR/templates/CLAUDE.md.template" ]; then
  if [ -f "$CACHE_DIR/.last-template-hash" ]; then
    OLD_HASH=$(cat "$CACHE_DIR/.last-template-hash")
    NEW_HASH=$(portable_sha256 "$SOURCE_DIR/templates/CLAUDE.md.template")
    if [ "$OLD_HASH" != "$NEW_HASH" ]; then
      TEMPLATE_CHANGED=true
      info "CLAUDE.md template has changed"
    fi
  else
    TEMPLATE_CHANGED=true
  fi
fi

if [ "$CHANGED" -eq 0 ] && [ "$NEW" -eq 0 ] && [ "$TEMPLATE_CHANGED" = false ]; then
  pass "Everything is up to date. Nothing to do."
  exit 0
fi

# ─── Step 3: Apply changes ──────────────────────────────────────────────────
if $DRY_RUN; then
  header "4. Dry Run Complete"
  info "Run without --dry-run to apply changes."
  if $TEMPLATE_CHANGED; then
    info "CLAUDE.md template changed — agent will merge on next upgrade."
  fi
  exit 0
fi

header "4. Apply Changes"

# Backup changed files
if [ "$CHANGED" -gt 0 ]; then
  BACKUP_DIR="$HEX_DOTDIR/.upgrade-backup-$(date +%Y%m%d-%H%M%S)"
  mkdir -p "$BACKUP_DIR"

  while IFS= read -r src_file; do
    if [[ "$src_file" == "$SOURCE_DIR/$SOURCE_SUBDIR_SCRIPTS/"* ]]; then
      rel_path="scripts/${src_file#"$SOURCE_DIR"/"$SOURCE_SUBDIR_SCRIPTS"/}"
    elif [[ "$src_file" == "$SOURCE_DIR/$SOURCE_SUBDIR_SKILLS/"* ]]; then
      rel_path="skills/${src_file#"$SOURCE_DIR"/"$SOURCE_SUBDIR_SKILLS"/}"
    elif [[ "$src_file" == "$SOURCE_DIR/$SOURCE_SUBDIR_COMMANDS/"* ]]; then
      rel_path="commands/${src_file#"$SOURCE_DIR"/"$SOURCE_SUBDIR_COMMANDS"/}"
    elif [[ "$src_file" == "$SOURCE_DIR/$SOURCE_SUBDIR_HOOKS/"* ]]; then
      rel_path="hooks/${src_file#"$SOURCE_DIR"/"$SOURCE_SUBDIR_HOOKS"/}"
    else
      continue
    fi
    case "$rel_path" in
      settings.local.json|__pycache__/*) continue ;;
    esac
    dst_file="$HEX_DOTDIR/$rel_path"
    if [ -f "$dst_file" ] && ! diff -q "$src_file" "$dst_file" > /dev/null 2>&1; then
      backup_path="$BACKUP_DIR/$rel_path"
      mkdir -p "$(dirname "$backup_path")"
      cp "$dst_file" "$backup_path"
    fi
  done < <(
    for _subdir in "$SOURCE_SUBDIR_SCRIPTS" "$SOURCE_SUBDIR_SKILLS" "$SOURCE_SUBDIR_COMMANDS"; do
      [ -d "$SOURCE_DIR/$_subdir" ] && find "$SOURCE_DIR/$_subdir" -type f ! -path "*/__pycache__/*"
    done
    [ -d "$SOURCE_DIR/$SOURCE_SUBDIR_HOOKS" ] && find "$SOURCE_DIR/$SOURCE_SUBDIR_HOOKS" -type f ! -path "*/__pycache__/*"
  )

  info "Backed up $CHANGED file(s) to ${BACKUP_DIR##*/}"
fi

# Track whether Web UI is newly added (v1-only feature)
UI_IS_NEW=false
if [ "$SOURCE_LAYOUT" = "v1" ]; then
  if [ -d "$SOURCE_DIR/dot-claude/ui" ] && [ ! -d "$HEX_DOTDIR/ui" ]; then
    UI_IS_NEW=true
  fi

  # Protect user-customized style.css: back it up before rsync, restore after
  USER_STYLE=""
  if [ -f "$HEX_DOTDIR/ui/static/style.css" ] && [ -f "$SOURCE_DIR/dot-claude/ui/static/style.css" ]; then
    if ! diff -q "$HEX_DOTDIR/ui/static/style.css" "$SOURCE_DIR/dot-claude/ui/static/style.css" > /dev/null 2>&1; then
      USER_STYLE="$(mktemp)"
      cp "$HEX_DOTDIR/ui/static/style.css" "$USER_STYLE"
    fi
  fi
fi

# Copy files — sync scripts, skills, commands individually (layout-aware).
if [ -d "$SOURCE_DIR/$SOURCE_SUBDIR_SCRIPTS" ]; then
  rsync -a     --exclude='__pycache__'     "$SOURCE_DIR/$SOURCE_SUBDIR_SCRIPTS/" "$HEX_DOTDIR/scripts/"
fi
if [ -d "$SOURCE_DIR/$SOURCE_SUBDIR_SKILLS" ]; then
  rsync -a     --exclude='__pycache__'     "$SOURCE_DIR/$SOURCE_SUBDIR_SKILLS/" "$HEX_DOTDIR/skills/"
fi
if [ -d "$SOURCE_DIR/$SOURCE_SUBDIR_COMMANDS" ]; then
  rsync -a     --exclude='__pycache__'     "$SOURCE_DIR/$SOURCE_SUBDIR_COMMANDS/" "$HEX_DOTDIR/commands/"
fi

# For v1 layout, also sync hooks and the ui directory (not present in v2).
if [ "$SOURCE_LAYOUT" = "v1" ]; then
  if [ -d "$SOURCE_DIR/$SOURCE_SUBDIR_HOOKS" ]; then
    rsync -a       --exclude='__pycache__'       "$SOURCE_DIR/$SOURCE_SUBDIR_HOOKS/" "$HEX_DOTDIR/hooks/"
  fi
  if [ -d "$SOURCE_DIR/dot-claude/ui" ]; then
    rsync -a       --exclude='__pycache__'       "$SOURCE_DIR/dot-claude/ui/" "$HEX_DOTDIR/ui/"
  fi
  # Restore user-customized style.css if it was different from source
  if [ -n "${USER_STYLE:-}" ] && [ -f "$USER_STYLE" ]; then
    cp "$USER_STYLE" "$HEX_DOTDIR/ui/static/style.css"
    rm -f "$USER_STYLE"
    info "Preserved your customized ui/static/style.css"
  fi
fi

# Make scripts executable
find "$HEX_DOTDIR" -name "*.sh" -type f -exec chmod +x {} +

pass "Applied $((CHANGED + NEW)) file(s)"

# ─── CLAUDE.md system zone replacement ───────────────────────────────────────
# The system zone (between hex:system-start and hex:system-end markers) is
# managed by hex. On upgrade, replace it wholesale with the new template content
# while preserving everything in the user zone.
CLAUDE_MD="$HEX_DIR/CLAUDE.md"
TEMPLATE_CLAUDE_MD="$SOURCE_DIR/$SOURCE_CLAUDE_MD_PATH"
if [ -f "$CLAUDE_MD" ] && [ -f "$TEMPLATE_CLAUDE_MD" ]; then
  if grep -q 'hex:system-start' "$CLAUDE_MD" && grep -q 'hex:system-end' "$CLAUDE_MD"; then
    if grep -q 'hex:system-start' "$TEMPLATE_CLAUDE_MD" && grep -q 'hex:system-end' "$TEMPLATE_CLAUDE_MD"; then
      # Extract the new system zone from the template
      NEW_SYSTEM=$(sed -n '/hex:system-start/,/hex:system-end/p' "$TEMPLATE_CLAUDE_MD")
      # Extract user content (everything after hex:system-end)
      USER_CONTENT=$(sed -n '/hex:system-end/,$ { /hex:system-end/d; p; }' "$CLAUDE_MD")
      # Extract pre-system content (everything before hex:system-start)
      PRE_CONTENT=$(sed -n '1,/hex:system-start/ { /hex:system-start/d; p; }' "$CLAUDE_MD")

      # Rebuild CLAUDE.md: pre + system zone + user content
      {
        echo "$PRE_CONTENT"
        echo "$NEW_SYSTEM"
        echo "$USER_CONTENT"
      } > "$CLAUDE_MD.tmp" && mv "$CLAUDE_MD.tmp" "$CLAUDE_MD"
      pass "CLAUDE.md system zone updated (user content preserved)"
    else
      warn "Template CLAUDE.md missing zone markers — skipping zone replacement"
    fi
  else
    warn "Local CLAUDE.md missing zone markers — skipping zone replacement"
    info "Hint: add <!-- hex:system-start --> and <!-- hex:system-end --> markers to enable auto-updates"
  fi
fi

# ─── Post-upgrade health check ──────────────────────────────────────────────
# Run doctor in quiet mode to catch any issues introduced by the upgrade.
if [ -f "$HEX_DOTDIR/scripts/doctor.sh" ]; then
  header "4b. Post-Upgrade Health Check"
  if HEX_DIR="$HEX_DIR" bash "$HEX_DOTDIR/scripts/doctor.sh" --quiet 2>/dev/null; then
    pass "Doctor: all checks passed"
  else
    warn "Doctor found issues after upgrade — run 'bash $HEX_DOTDIR/scripts/doctor.sh' for details"
  fi
fi

# ─── Record upgrade SHA (for update notifications) ───────────────────────────
# Write last_remote_sha to upgrade.json so check-update.sh knows our baseline.
# Works for both remote upgrades (CACHE_DIR) and --local upgrades (LOCAL_PATH).
SHA_SOURCE=""
if [ -d "$CACHE_DIR/.git" ]; then
  SHA_SOURCE="$CACHE_DIR"
elif [ -n "${LOCAL_PATH:-}" ] && [ -d "$LOCAL_PATH/.git" ]; then
  SHA_SOURCE="$LOCAL_PATH"
fi
if [ -n "$SHA_SOURCE" ]; then
  UPGRADE_SHA=$(git -C "$SHA_SOURCE" rev-parse HEAD 2>/dev/null || echo "")
  if [ -n "$UPGRADE_SHA" ]; then
    if [ ! -f "$CONFIG_FILE" ]; then
      printf '{"repo":"%s"}\n' "$REPO_URL" > "$CONFIG_FILE.tmp" && mv "$CONFIG_FILE.tmp" "$CONFIG_FILE"
    fi
    python3 -c "
import json, os, sys
sha, path = sys.argv[1], sys.argv[2]
try:
    with open(path) as f:
        data = json.load(f)
except Exception:
    data = {}
data['last_remote_sha'] = sha
tmp = path + '.tmp'
with open(tmp, 'w') as f:
    json.dump(data, f, indent=2)
    f.write('\n')
os.rename(tmp, path)
" "$UPGRADE_SHA" "$CONFIG_FILE" 2>/dev/null && info "Recorded upgrade SHA: ${UPGRADE_SHA:0:8}..."
  fi
fi

# Clear update-available flag (we just upgraded)
rm -f "$HEX_DOTDIR/.update-available"

# v1-only: Copy evolution scripts, BOI archive scripts, and hex-events policies.
# These directories have no equivalent in the v2 layout.
if [ "$SOURCE_LAYOUT" = "v1" ]; then
  # Copy evolution scripts to agent workspace (these live outside .hex/)
  if [ -d "$SOURCE_DIR/dot-claude/evolution" ]; then
    mkdir -p "$HEX_DIR/evolution/eval/test-cases"
    rsync -a --exclude='__pycache__'       "$SOURCE_DIR/dot-claude/evolution/" "$HEX_DIR/evolution/"
    find "$HEX_DIR/evolution" -name "*.sh" -type f -exec chmod +x {} +
    info "Evolution scripts updated"
  fi

  # Copy BOI archive scripts (these live in ~/.boi/scripts/)
  if [ -d "$SOURCE_DIR/dot-claude/boi-scripts" ]; then
    mkdir -p "$HOME/.boi/scripts"
    rsync -a "$SOURCE_DIR/dot-claude/boi-scripts/" "$HOME/.boi/scripts/"
    find "$HOME/.boi/scripts" -name "*.sh" -type f -exec chmod +x {} +
    info "BOI archive scripts updated"
  fi

  # Copy hex-events policies
  if [ -d "$SOURCE_DIR/dot-claude/hex-events-policies" ] && [ -d "$HOME/.hex-events/policies" ]; then
    rsync -a "$SOURCE_DIR/dot-claude/hex-events-policies/" "$HOME/.hex-events/policies/"
    info "hex-events policies updated"
  fi
fi

# Store template hash for next upgrade
if [ -f "$SOURCE_DIR/templates/CLAUDE.md.template" ]; then
  mkdir -p "$CACHE_DIR"
  portable_sha256 "$SOURCE_DIR/templates/CLAUDE.md.template" > "$CACHE_DIR/.last-template-hash"
fi

# ─── Step 4: Ensure shell alias & skip-permissions ──────────────────────────
header "5. Shell Setup"

ALIAS_LINE="alias hex='bash $HEX_DIR/.hex/scripts/workspace.sh'"

# Detect shell rc file from $SHELL (not $BASH_VERSION, since this runs under bash)
USER_SHELL="$(basename "${SHELL:-}")"
if [ "$USER_SHELL" = "zsh" ]; then
  RC_FILE="$HOME/.zshrc"
elif [ "$USER_SHELL" = "bash" ]; then
  RC_FILE="$HOME/.bashrc"
elif [ -f "$HOME/.zshrc" ]; then
  RC_FILE="$HOME/.zshrc"
elif [ -f "$HOME/.bashrc" ]; then
  RC_FILE="$HOME/.bashrc"
else
  RC_FILE=""
fi

if [ -n "$RC_FILE" ]; then
  [ -f "$RC_FILE" ] || touch "$RC_FILE"

  # --- hex alias (three-tier: exact match, any alias hex=, or append) ---
  if grep -qF "$ALIAS_LINE" "$RC_FILE" 2>/dev/null; then
    pass "hex alias already up to date in $RC_FILE"
  elif grep -qF "alias hex=" "$RC_FILE" 2>/dev/null; then
    sed -e "s|^alias hex=.*|$ALIAS_LINE|" "$RC_FILE" > "$RC_FILE.tmp" && mv "$RC_FILE.tmp" "$RC_FILE"
    pass "Updated hex alias in $RC_FILE"
  else
    echo "" >> "$RC_FILE"
    echo "# Hexagon" >> "$RC_FILE"
    echo "$ALIAS_LINE" >> "$RC_FILE"
    pass "Added hex alias to $RC_FILE"
  fi

  # --- hex-ui alias ---
  UI_ALIAS_LINE="alias hex-ui='bash $HEX_DIR/.hex/scripts/ui.sh'"
  if grep -qF "alias hex-ui=" "$RC_FILE" 2>/dev/null; then
    if ! grep -qF "$UI_ALIAS_LINE" "$RC_FILE" 2>/dev/null; then
      sed -e "s|^alias hex-ui=.*|$UI_ALIAS_LINE|" "$RC_FILE" > "$RC_FILE.tmp" && mv "$RC_FILE.tmp" "$RC_FILE"
      pass "Updated hex-ui alias in $RC_FILE"
    else
      pass "hex-ui alias already up to date in $RC_FILE"
    fi
  else
    echo "$UI_ALIAS_LINE" >> "$RC_FILE"
    pass "Added hex-ui alias to $RC_FILE"
  fi

  # --- claude() skip-permissions function ---
  if grep -qF 'dangerously-skip-permissions' "$RC_FILE" 2>/dev/null; then
    pass "claude skip-permissions already configured in $RC_FILE"
  else
    echo "" >> "$RC_FILE"
    echo "# Claude Code — skip permission prompts" >> "$RC_FILE"
    echo "unalias claude 2>/dev/null" >> "$RC_FILE"
    echo 'claude() { command claude --dangerously-skip-permissions "$@"; }' >> "$RC_FILE"
    pass "Added skip-permissions function to $RC_FILE"
  fi
else
  warn "Could not detect shell rc file. Add manually:"
  info "  $ALIAS_LINE"
  info '  claude() { command claude --dangerously-skip-permissions "$@"; }'
fi

# ─── Step 5: Summary ────────────────────────────────────────────────────────
header "6. Summary"

echo -e "  Files updated:  $CHANGED"
echo -e "  Files added:    $NEW"

if $TEMPLATE_CHANGED; then
  echo ""
  echo -e "  ${YELLOW}CLAUDE.md template has changed.${RESET}"
  echo -e "  The agent will merge updates into your CLAUDE.md."
  echo -e "  Template saved at: $SOURCE_DIR/templates/CLAUDE.md.template"
fi

echo ""

# Notify about Web UI if it was newly added
if $UI_IS_NEW && [ -d "$HEX_DOTDIR/ui" ]; then
  echo -e "  ${GREEN}New: Web UI. Run \`hex-ui\` to launch.${RESET}"
fi

# Notify about capture pane if it was newly added
if [ -f "$HEX_DOTDIR/scripts/capture-pane.sh" ]; then
  CAPTURE_EXISTED=false
  if [ -d "${BACKUP_DIR:-}" ] && [ -f "${BACKUP_DIR:-}/scripts/capture-pane.sh" ]; then
    CAPTURE_EXISTED=true
  fi
  if ! $CAPTURE_EXISTED; then
    echo -e "  ${GREEN}New: Quick capture pane in workspace. Restart with \`hex\` to see it.${RESET}"
  fi
fi

echo -e "  ${GREEN}Upgrade complete.${RESET}"
echo ""

# Exit 1 if any component verification failed (partial success)
if [ "$COMPONENT_WARNINGS" -gt 0 ]; then
  warn "$COMPONENT_WARNINGS component verification warning(s) — upgrade partially succeeded"
  exit 1
fi
