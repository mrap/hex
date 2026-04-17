#!/usr/bin/env bash
# bootstrap-migrate.sh — One-line entry point for hex v1 → v2 migration.
#
# Usage (from your hex instance directory):
#   bash <(curl -fsSL https://raw.githubusercontent.com/mrap/hex-foundation/v0.2.2/bootstrap-migrate.sh)
#
# Or pass the hex dir explicitly:
#   HEX_DIR=/path/to/hex bash <(curl -fsSL .../bootstrap-migrate.sh)
#
# What this does:
#   1. Detects your hex instance
#   2. Downloads the tested migrator from hex-foundation@v0.2.2
#   3. Backs up your .claude/ directory
#   4. Migrates .claude/ hex-owned content → .hex/
#   5. Validates (doctor, startup, /hex-upgrade --dry-run)
#   6. Reports. If anything fails: rolls back automatically.
#
# Rollback (if you need it later):
#   bash .hex/scripts/migrate-v1-to-v2.sh --rollback

set -uo pipefail

GREEN='\033[32m'; YELLOW='\033[33m'; RED='\033[31m'; BOLD='\033[1m'; DIM='\033[2m'; RESET='\033[0m'
log() { echo -e "[bootstrap] $*"; }
pass(){ echo -e "  [${GREEN}OK${RESET}] $*"; }
warn(){ echo -e "  [${YELLOW}WARN${RESET}] $*"; }
fail(){ echo -e "  [${RED}FAIL${RESET}] $*" >&2; }
die() { fail "$*"; exit 1; }

# ─── Config ───────────────────────────────────────────────────────────────────

VERSION="${HEX_FOUNDATION_VERSION:-v0.2.2}"
FOUNDATION_REPO="${HEX_FOUNDATION_REPO:-https://raw.githubusercontent.com/mrap/hex-foundation}"
MIGRATOR_URL="${FOUNDATION_REPO}/${VERSION}/system/scripts/migrate-v1-to-v2.sh"

# ─── Detect HEX_DIR ───────────────────────────────────────────────────────────

if [ -z "${HEX_DIR:-}" ]; then
    if [ -f "$(pwd)/CLAUDE.md" ]; then
        HEX_DIR="$(pwd)"
    else
        die "No HEX_DIR set and no CLAUDE.md in current directory. cd into your hex instance first, or set HEX_DIR=/path/to/hex"
    fi
fi

[ -d "$HEX_DIR" ] || die "HEX_DIR does not exist: $HEX_DIR"
[ -f "$HEX_DIR/CLAUDE.md" ] || die "$HEX_DIR has no CLAUDE.md — is this a hex instance?"

echo -e "\n${BOLD}hex v1 → v2 Bootstrap Migrator${RESET}"
echo "  Target: $HEX_DIR"
echo "  Migrator source: $MIGRATOR_URL"
echo ""

# ─── Preflight ────────────────────────────────────────────────────────────────

log "Preflight"

command -v git    >/dev/null 2>&1 || die "git not installed"
command -v curl   >/dev/null 2>&1 || die "curl not installed"
command -v python3 >/dev/null 2>&1 || die "python3 not installed"
pass "required tools: git, curl, python3"

# Check git state early so we don't waste time downloading
if [ ! -d "$HEX_DIR/.git" ] && [ ! -f "$HEX_DIR/.git" ]; then
    die "$HEX_DIR is not a git repo. Initialize it first: cd $HEX_DIR && git init"
fi
if [ -n "$(git -C "$HEX_DIR" status --porcelain)" ]; then
    fail "git status is not clean in $HEX_DIR:"
    git -C "$HEX_DIR" status --short | head -10
    die "Commit or stash your changes first, then re-run."
fi
pass "git status clean"

# Detect v1 vs v2 early
if [ -d "$HEX_DIR/.hex/scripts" ] || [ -d "$HEX_DIR/.hex/skills" ]; then
    if [ -d "$HEX_DIR/.claude/scripts" ] || [ -d "$HEX_DIR/.claude/skills" ]; then
        die "Hybrid state: both .claude/scripts and .hex/scripts exist. Clean up one before migrating."
    fi
    pass "Already on v2 layout — nothing to do."
    echo ""
    echo "  Run /hex-upgrade to pull latest foundation content."
    exit 0
fi
if [ ! -d "$HEX_DIR/.claude/scripts" ] && [ ! -d "$HEX_DIR/.claude/skills" ]; then
    die "No v1 layout found at $HEX_DIR (neither .claude/scripts nor .claude/skills exists). Is this a hex instance?"
fi
pass "v1 layout detected"

# ─── Download migrator ────────────────────────────────────────────────────────

log "Download migrator"

TMP_MIGRATOR=$(mktemp /tmp/hex-migrate-v1-to-v2.XXXXXX.sh)
trap 'rm -f "$TMP_MIGRATOR"' EXIT

if ! curl -fsSL "$MIGRATOR_URL" -o "$TMP_MIGRATOR"; then
    die "Failed to download migrator from $MIGRATOR_URL"
fi
chmod +x "$TMP_MIGRATOR"

# Sanity-check the downloaded file
if ! head -1 "$TMP_MIGRATOR" | grep -q '^#!/'; then
    die "Downloaded migrator looks malformed (no shebang). Aborting."
fi
if ! bash -n "$TMP_MIGRATOR" 2>/dev/null; then
    die "Downloaded migrator has syntax errors. Aborting."
fi
pass "migrator downloaded and validated ($(wc -l <"$TMP_MIGRATOR") lines)"

# ─── Run migrator ─────────────────────────────────────────────────────────────

log "Running migrator (this will back up .claude/ first)"
echo ""

if bash "$TMP_MIGRATOR" --hex-dir "$HEX_DIR"; then
    MIGRATOR_RC=0
else
    MIGRATOR_RC=$?
fi

echo ""

# ─── Handle outcome ───────────────────────────────────────────────────────────

case "$MIGRATOR_RC" in
    0)
        pass "Migration successful."
        echo ""
        echo "  ${BOLD}Next steps:${RESET}"
        echo "    1. Launch a fresh Claude Code session: cd $HEX_DIR && claude"
        echo "    2. Run /hex-upgrade whenever you want foundation content updates"
        echo "    3. If anything's off, roll back:"
        echo "       bash $HEX_DIR/.hex/scripts/migrate-v1-to-v2.sh --rollback --hex-dir $HEX_DIR"
        echo ""
        exit 0
        ;;
    1)
        fail "Migration failed in preflight (no changes made)."
        echo "  See error above. Fix the issue and re-run."
        exit 1
        ;;
    2)
        fail "Migration failed mid-run."
        echo ""
        echo "  ${BOLD}Automatic rollback:${RESET}"
        LATEST_BACKUP=$(ls -dt "$HEX_DIR"/.claude.v1-backup-* 2>/dev/null | head -1)
        if [ -n "$LATEST_BACKUP" ]; then
            echo "  Found backup at: $LATEST_BACKUP"
            echo "  Attempting auto-rollback..."
            if bash "$TMP_MIGRATOR" --rollback --hex-dir "$HEX_DIR"; then
                pass "Rolled back to pre-migration state."
                echo "  Inspect the backup + git log to understand what went wrong."
            else
                warn "Auto-rollback failed. Manual recovery:"
                echo "    rm -rf $HEX_DIR/.claude"
                echo "    mv $LATEST_BACKUP $HEX_DIR/.claude"
                echo "    rm -rf $HEX_DIR/.hex $HEX_DIR/.agents"
                echo "    (cd $HEX_DIR && git reset --hard HEAD)"
            fi
        else
            warn "No backup found — can't auto-rollback. Manual recovery needed."
        fi
        exit 2
        ;;
    3)
        warn "Migration ran but validation failed."
        echo "  State is v2 but doctor/startup reported issues."
        echo "  Inspect manually: HEX_DIR=$HEX_DIR bash $HEX_DIR/.hex/scripts/doctor.sh"
        echo "  If you want to roll back:"
        echo "    bash $HEX_DIR/.hex/scripts/migrate-v1-to-v2.sh --rollback --hex-dir $HEX_DIR"
        exit 3
        ;;
    *)
        fail "Migration exited with unexpected code: $MIGRATOR_RC"
        exit "$MIGRATOR_RC"
        ;;
esac
