#!/usr/bin/env bash
# sync-safe
# migrate-v1-to-v2.sh — Migrate a hex instance from v1 layout (.claude/ owned
# by hex) to v2 layout (.hex/ owned by hex, .claude/ narrowed to Claude Code
# discovery). Idempotent: re-running on a v2 instance exits cleanly.
#
# Usage:
#   migrate-v1-to-v2.sh [options]
#
# Options:
#   --dry-run           Print planned operations, change nothing
#   --force             Don't prompt on unknown .claude/ entries; leave them in place
#   --rollback          Restore from the most recent .claude.v1-backup-* and exit
#   --hex-dir PATH      Operate on a specific hex instance (default: autodetect)
#   --skip-backup       Skip the .claude.v1-backup-* copy (faster, but no rollback)
#   --skip-validate     Skip post-migration doctor+startup checks
#   -h, --help          Print this help
#
# What moves from .claude/ to .hex/:
#   scripts/, skills/, hooks/, commands/ (mirrored), lib/, templates/,
#   workflows/, boi-scripts/, hex-events-policies/, handoffs/, secrets/,
#   evolution/ (renamed to evolution-scripts/), memory.db,
#   statusline.sh, timezone, upgrade.json, llm-preference
#
# What stays in .claude/:
#   settings.json (hook paths rewritten), settings.local.json,
#   commands/ (mirrored copy for Claude Code discovery)
#
# Required preconditions:
#   - HEX_DIR is a git repo
#   - Clean git status (no uncommitted changes)
#   - On a branch (not detached HEAD)
#   - Running from outside HEX_DIR (or in a throwaway session that can die mid-run)
#
# Exit codes:
#   0 = migrated successfully, or already on v2 (no-op)
#   1 = preflight failure (dirty git, unknown dirs, etc.)
#   2 = runtime failure during migration (see stderr; .claude.v1-backup-* exists for rollback)
#   3 = validation failure post-migration (see stderr; state is v2 but doctor/startup unhappy)

set -uo pipefail

# ─── Defaults ─────────────────────────────────────────────────────────────────

DRY_RUN=false
FORCE=false
ROLLBACK=false
SKIP_BACKUP=false
SKIP_VALIDATE=false
HEX_DIR=""

BACKUP_STAMP="$(date +%Y-%m-%d-%H%M%S)"

# ─── Helpers ──────────────────────────────────────────────────────────────────

GREEN='\033[32m'; YELLOW='\033[33m'; RED='\033[31m'; BOLD='\033[1m'; DIM='\033[2m'; RESET='\033[0m'

log()   { echo -e "[migrate] $*"; }
info()  { echo -e "  ${DIM}→${RESET} $*"; }
pass()  { echo -e "  [${GREEN}PASS${RESET}] $*"; }
warn()  { echo -e "  [${YELLOW}WARN${RESET}] $*"; }
fail()  { echo -e "  [${RED}FAIL${RESET}] $*" >&2; }
die()   { fail "$*"; exit 1; }
header(){ echo -e "\n${BOLD}$*${RESET}"; }

run() {
    # Execute command; in dry-run mode, just print it.
    if $DRY_RUN; then
        echo -e "  ${DIM}[dry-run]${RESET} $*"
    else
        eval "$@"
    fi
}

run_silent() {
    if $DRY_RUN; then
        echo -e "  ${DIM}[dry-run]${RESET} $*"
    else
        eval "$@" >/dev/null 2>&1
    fi
}

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# \?//'
    exit 0
}

# ─── Args ─────────────────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)       DRY_RUN=true; shift ;;
        --force)         FORCE=true; shift ;;
        --rollback)      ROLLBACK=true; shift ;;
        --hex-dir)       HEX_DIR="$2"; shift 2 ;;
        --skip-backup)   SKIP_BACKUP=true; shift ;;
        --skip-validate) SKIP_VALIDATE=true; shift ;;
        -h|--help)       usage ;;
        *)               die "Unknown option: $1" ;;
    esac
done

# ─── Detect HEX_DIR ───────────────────────────────────────────────────────────

if [ -z "$HEX_DIR" ]; then
    # Autodetect: caller's CWD if it has CLAUDE.md, else error
    if [ -f "$(pwd)/CLAUDE.md" ]; then
        HEX_DIR="$(pwd)"
    else
        die "No HEX_DIR specified and no CLAUDE.md in CWD. Pass --hex-dir PATH."
    fi
fi

[ -d "$HEX_DIR" ] || die "HEX_DIR does not exist: $HEX_DIR"
[ -f "$HEX_DIR/CLAUDE.md" ] || die "HEX_DIR has no CLAUDE.md: $HEX_DIR"

cd "$HEX_DIR" || die "cannot cd to $HEX_DIR"

header "hex v1 → v2 migrator"
info "HEX_DIR=$HEX_DIR"
$DRY_RUN && info "mode: DRY RUN (no changes will be made)"

# ─── Rollback mode ────────────────────────────────────────────────────────────

if $ROLLBACK; then
    header "Rollback"
    LATEST_BACKUP=$(ls -dt "$HEX_DIR"/.claude.v1-backup-* 2>/dev/null | head -1)
    [ -n "$LATEST_BACKUP" ] || die "no .claude.v1-backup-* found in $HEX_DIR"
    info "most recent backup: $LATEST_BACKUP"

    if [ -d "$HEX_DIR/.claude" ]; then
        run "mv \"$HEX_DIR/.claude\" \"$HEX_DIR/.claude.failed-migration-$BACKUP_STAMP\""
    fi
    run "mv \"$LATEST_BACKUP\" \"$HEX_DIR/.claude\""

    if [ -d "$HEX_DIR/.hex" ]; then
        warn ".hex/ still exists; moving to .hex.failed-migration-$BACKUP_STAMP"
        run "mv \"$HEX_DIR/.hex\" \"$HEX_DIR/.hex.failed-migration-$BACKUP_STAMP\""
    fi

    pass "Rolled back. Your hex instance is at .claude/ again."
    info "If you had a feat/* branch for the migration, reset it: git reset --hard <pre-migration-sha>"
    exit 0
fi

# ─── Preflight ────────────────────────────────────────────────────────────────

header "Preflight"

# Check git status
if [ ! -d "$HEX_DIR/.git" ] && [ ! -f "$HEX_DIR/.git" ]; then
    die "$HEX_DIR is not a git repo"
fi

if [ -n "$(git -C "$HEX_DIR" status --porcelain)" ]; then
    fail "git status is not clean. Commit or stash first."
    git -C "$HEX_DIR" status --short | head -10
    exit 1
fi
pass "git status clean"

BRANCH=$(git -C "$HEX_DIR" branch --show-current 2>/dev/null)
[ -n "$BRANCH" ] || die "detached HEAD; check out a branch first"
info "current branch: $BRANCH"

# Detect v1 vs v2
IS_V1=false; IS_V2=false
if [ -d "$HEX_DIR/.claude/scripts" ] || [ -d "$HEX_DIR/.claude/skills" ]; then
    IS_V1=true
fi
if [ -d "$HEX_DIR/.hex/scripts" ] || [ -d "$HEX_DIR/.hex/skills" ]; then
    IS_V2=true
fi

if $IS_V2 && ! $IS_V1; then
    pass "Already on v2 layout. Nothing to do."
    exit 0
fi
if $IS_V2 && $IS_V1; then
    fail "Hybrid state detected: both .claude/scripts AND .hex/scripts exist."
    info "Resolve manually before re-running, or --rollback and start over."
    exit 1
fi
$IS_V1 || die "No v1 layout found (.claude/scripts or .claude/skills missing)."
pass "v1 layout detected"

# ─── Classification ──────────────────────────────────────────────────────────

# Hex-owned directories that move .claude/X → .hex/X
HEX_OWNED_DIRS=(
    scripts skills hooks lib templates workflows
    boi-scripts hex-events-policies handoffs secrets
)

# Directory renamed during move (critic-flagged mismatch)
# .claude/evolution → .hex/evolution-scripts (avoids collision with hex root's evolution/)
EVOLUTION_RENAME_FROM=".claude/evolution"
EVOLUTION_RENAME_TO=".hex/evolution-scripts"

# Loose hex-owned files that move .claude/X → .hex/X
HEX_OWNED_FILES=(
    statusline.sh timezone upgrade.json llm-preference memory.db
)

# Things that STAY in .claude/ (Claude Code discovery)
CLAUDE_RETAINED_ITEMS=(
    settings.json settings.local.json commands
)

# Regeneratable / junk — deleted during migration
REGEN_JUNK_PATTERNS=(
    "__pycache__"
    ".pytest_cache"
    ".dream-last-run"
    ".update-available"
    ".update-checked"
    ".upgrade-cache"
    ".upgrade-backup-*"
)

# Audit unknown entries
header "Audit .claude/ contents"
UNKNOWN=()
for entry in "$HEX_DIR"/.claude/*; do
    [ -e "$entry" ] || continue
    name=$(basename "$entry")
    known=false
    for d in "${HEX_OWNED_DIRS[@]}"; do [[ "$name" == "$d" ]] && known=true; done
    for f in "${HEX_OWNED_FILES[@]}"; do [[ "$name" == "$f" ]] && known=true; done
    for c in "${CLAUDE_RETAINED_ITEMS[@]}"; do [[ "$name" == "$c" ]] && known=true; done
    for p in "${REGEN_JUNK_PATTERNS[@]}"; do [[ "$name" == $p ]] && known=true; done
    [[ "$name" == "evolution" ]] && known=true
    if ! $known; then
        UNKNOWN+=("$name")
    fi
done

# Also scan hidden files in .claude/
for entry in "$HEX_DIR"/.claude/.*; do
    [ -e "$entry" ] || continue
    name=$(basename "$entry")
    [[ "$name" == "." || "$name" == ".." ]] && continue
    known=false
    for p in "${REGEN_JUNK_PATTERNS[@]}"; do [[ "$name" == $p ]] && known=true; done
    if ! $known; then
        UNKNOWN+=("$name")
    fi
done

if [ ${#UNKNOWN[@]} -gt 0 ]; then
    warn "Unknown entries under .claude/ (not in standard hex v1 manifest):"
    for u in "${UNKNOWN[@]}"; do echo "    .claude/$u"; done
    if ! $FORCE; then
        echo ""
        echo "  These will be LEFT IN PLACE in .claude/ (treated as companion/extension-owned)."
        echo "  Pass --force to proceed, or move/delete them first."
        exit 1
    else
        info "proceeding with --force; ${#UNKNOWN[@]} unknown entr(y/ies) will stay in .claude/"
    fi
fi
pass "audit complete (${#UNKNOWN[@]} unknown)"

# ─── Backup ───────────────────────────────────────────────────────────────────

BACKUP_DIR="$HEX_DIR/.claude.v1-backup-$BACKUP_STAMP"
if $SKIP_BACKUP; then
    warn "--skip-backup: no rollback possible"
else
    header "Backup"
    info "copying .claude/ → $BACKUP_DIR"
    run "cp -a \"$HEX_DIR/.claude\" \"$BACKUP_DIR\""
    pass "backup complete"
fi

# ─── Create .hex/ skeleton ────────────────────────────────────────────────────

header "P1: Create .hex/ skeleton"
run "mkdir -p \"$HEX_DIR/.hex\""
for d in "${HEX_OWNED_DIRS[@]}"; do
    [ -d "$HEX_DIR/.claude/$d" ] && run "mkdir -p \"$HEX_DIR/.hex/$d\""
done
# evolution → evolution-scripts
[ -d "$HEX_DIR/$EVOLUTION_RENAME_FROM" ] && run "mkdir -p \"$HEX_DIR/$EVOLUTION_RENAME_TO\""
pass "skeleton created"

# ─── P2: git mv tracked directories ───────────────────────────────────────────

header "P2: git mv tracked directories"

git_mv_if_tracked() {
    local src="$1"; local dst="$2"
    # Only git mv if the src has tracked files. Otherwise, do filesystem mv.
    if [ ! -e "$HEX_DIR/$src" ]; then return 0; fi
    if git -C "$HEX_DIR" ls-files --error-unmatch "$src" >/dev/null 2>&1 \
       || [ -n "$(git -C "$HEX_DIR" ls-files "$src" 2>/dev/null)" ]; then
        # Remove the pre-created empty dst dir so git mv can move the source into it
        run_silent "rmdir \"$HEX_DIR/$dst\" 2>/dev/null || true"
        run "git -C \"$HEX_DIR\" mv \"$src\" \"$dst\""
    else
        warn "$src has no tracked files; filesystem mv"
        run_silent "rmdir \"$HEX_DIR/$dst\" 2>/dev/null || true"
        run "mv \"$HEX_DIR/$src\" \"$HEX_DIR/$dst\""
    fi
}

for d in "${HEX_OWNED_DIRS[@]}"; do
    [ -e "$HEX_DIR/.claude/$d" ] && git_mv_if_tracked ".claude/$d" ".hex/$d"
done

# Mirror .claude/commands/ for Claude Code discovery: copy .hex/commands/ → .claude/commands/
# This happens AFTER the git mv so .hex/commands/ has the source of truth.
if [ -d "$HEX_DIR/.hex/commands" ]; then
    run "mkdir -p \"$HEX_DIR/.claude/commands\""
    run "cp -a \"$HEX_DIR/.hex/commands/\"*.md \"$HEX_DIR/.claude/commands/\" 2>/dev/null || true"
    info "mirrored .hex/commands/ → .claude/commands/ for Claude Code discovery"
fi

# Handle evolution → evolution-scripts rename
if [ -d "$HEX_DIR/$EVOLUTION_RENAME_FROM" ]; then
    git_mv_if_tracked "$EVOLUTION_RENAME_FROM" "$EVOLUTION_RENAME_TO"
fi

pass "directories moved"

# ─── P3: git mv tracked loose files ───────────────────────────────────────────

header "P3: git mv tracked loose files"
for f in "${HEX_OWNED_FILES[@]}"; do
    if [ -f "$HEX_DIR/.claude/$f" ]; then
        if git -C "$HEX_DIR" ls-files --error-unmatch ".claude/$f" >/dev/null 2>&1; then
            run "git -C \"$HEX_DIR\" mv \".claude/$f\" \".hex/$f\""
        else
            run "mv \"$HEX_DIR/.claude/$f\" \"$HEX_DIR/.hex/$f\""
        fi
    fi
done
pass "loose files moved"

# ─── P4: Clean up regeneratable junk left in .claude/ ─────────────────────────

header "P4: Clean up regeneratable junk in .claude/"
# After git mv, some leftover dirs/files might remain (untracked caches, etc.)
for pat in "${REGEN_JUNK_PATTERNS[@]}"; do
    for leftover in "$HEX_DIR"/.claude/$pat; do
        [ -e "$leftover" ] && run "rm -rf \"$leftover\""
    done
    # Also scan nested (common: __pycache__ deep in .claude/)
    if ! $DRY_RUN; then
        find "$HEX_DIR/.claude" -name "$pat" -exec rm -rf {} + 2>/dev/null || true
    fi
done
# Remove now-empty hex-owned dir leftovers (e.g., .claude/scripts/ that still has pycache children)
for d in "${HEX_OWNED_DIRS[@]}"; do
    [ -d "$HEX_DIR/.claude/$d" ] && rmdir "$HEX_DIR/.claude/$d" 2>/dev/null || true
done
pass "cleanup done"

# ─── P5: Narrow path rewrites in tracked files ────────────────────────────────

header "P5: Narrow path rewrites in tracked files"

# Enumerated-segment sed (critic-approved): rewrite only specific .claude/ path
# segments, not blanket .claude/ → .hex/. Avoids corrupting references to
# .claude/settings.json or .claude/commands/ inside other files.

SED_TARGETS=(
    scripts skills hooks lib templates workflows
    boi-scripts hex-events-policies handoffs secrets
    memory.db timezone upgrade.json statusline.sh llm-preference
)

# Build sed script: one rule per target
SED_SCRIPT=""
for t in "${SED_TARGETS[@]}"; do
    # Escape the target (it has no special chars in practice but be safe)
    escaped=$(printf '%s' "$t" | sed 's/[][\\/.^$*]/\\&/g')
    SED_SCRIPT+="s|\\.claude/${escaped}|.hex/${escaped}|g; "
    SED_SCRIPT+="s|\\\$AGENT_DIR/\\.claude/${escaped}|\\\$HEX_DIR/.hex/${escaped}|g; "
    SED_SCRIPT+="s|\\\$CLAUDE_PROJECT_DIR/\\.claude/${escaped}|\\\$CLAUDE_PROJECT_DIR/.hex/${escaped}|g; "
done
# Evolution rename: .claude/evolution → .hex/evolution-scripts
SED_SCRIPT+="s|\\.claude/evolution/|.hex/evolution-scripts/|g; "
SED_SCRIPT+="s|\\\$AGENT_DIR/\\.claude/evolution/|\\\$HEX_DIR/.hex/evolution-scripts/|g; "

# Also AGENT_DIR → HEX_DIR where it's clearly the hex root variable (common pattern)
# Narrow: only in contexts where AGENT_DIR immediately precedes a .claude or .hex ref.
# This sed keeps other AGENT_DIR uses alone.

# Apply sed to every tracked file that isn't binary
if ! $DRY_RUN; then
    REWRITTEN=0
    while IFS= read -r -d '' f; do
        # Skip binary files
        file -b --mime "$f" 2>/dev/null | grep -q 'binary' && continue
        # Skip the settings.json which gets a dedicated rewrite in P6
        [[ "$f" == *".claude/settings.json" || "$f" == *".claude/settings.local.json" ]] && continue
        # Apply sed — if it changes the file, stage it
        if sed -i.bak "$SED_SCRIPT" "$f" 2>/dev/null; then
            if ! cmp -s "$f" "$f.bak"; then
                REWRITTEN=$((REWRITTEN + 1))
            fi
            rm -f "$f.bak"
        fi
    done < <(git -C "$HEX_DIR" ls-files -z)
    info "rewrote paths in $REWRITTEN tracked file(s)"
else
    info "[dry-run] would sed-rewrite ~$(git -C "$HEX_DIR" grep -l "\.claude/" 2>/dev/null | wc -l | xargs) tracked files"
fi
pass "path rewrites complete"

# ─── P6: Update .claude/settings.json hook paths ──────────────────────────────

header "P6: Update .claude/settings.json hook paths"

SETTINGS_JSON="$HEX_DIR/.claude/settings.json"
if [ -f "$SETTINGS_JSON" ]; then
    # Rewrite hook commands: .claude/hooks/ → .hex/hooks/
    if ! $DRY_RUN; then
        python3 - "$SETTINGS_JSON" <<'PY'
import json, sys, re
path = sys.argv[1]
with open(path) as f:
    data = json.load(f)

def rewrite_hooks(obj):
    if isinstance(obj, dict):
        for k, v in obj.items():
            if k == "command" and isinstance(v, str):
                obj[k] = re.sub(r'\.claude/hooks/', '.hex/hooks/', v)
            else:
                rewrite_hooks(v)
    elif isinstance(obj, list):
        for item in obj:
            rewrite_hooks(item)

if "hooks" in data:
    rewrite_hooks(data["hooks"])

with open(path, 'w') as f:
    json.dump(data, f, indent=2)
    f.write('\n')
PY
        pass "settings.json hook paths rewritten"
    else
        info "[dry-run] would rewrite hook paths in $SETTINGS_JSON"
    fi
else
    info "no .claude/settings.json found (skipping)"
fi

# ─── P7: Symlinks, .gitignore, optional configs ──────────────────────────────

header "P7: Symlinks + gitignore + optional configs"

# .agents/skills → ../.hex/skills (for tools that look in .agents/)
run "mkdir -p \"$HEX_DIR/.agents\""
run "ln -sfn ../.hex/skills \"$HEX_DIR/.agents/skills\""

# Update .gitignore — add .hex/ and .agents/ ephemeral entries if not present
GITIGNORE="$HEX_DIR/.gitignore"
if [ -f "$GITIGNORE" ] && ! $DRY_RUN; then
    {
        echo ""
        echo "# hex v2 layout"
        echo ".hex/memory.db"
        echo ".hex/llm-preference"
        echo ".hex/.update-checked"
        echo ".hex/.upgrade-cache/"
        echo ".agents/"
        echo ".codex/"
    } >> "$GITIGNORE"
    # Dedupe
    awk '!seen[$0]++' "$GITIGNORE" > "$GITIGNORE.tmp" && mv "$GITIGNORE.tmp" "$GITIGNORE"
    info "updated $GITIGNORE"
elif $DRY_RUN; then
    info "[dry-run] would append v2 entries to $GITIGNORE"
fi

# Seed optional configs via doctor --fix (if doctor.sh is present at new location)
DOCTOR="$HEX_DIR/.hex/scripts/doctor.sh"
if [ -x "$DOCTOR" ] && ! $DRY_RUN; then
    info "running doctor --fix to seed optional configs (.codex/config.toml, .hex/timezone, etc.)"
    HEX_DIR="$HEX_DIR" bash "$DOCTOR" --fix --quiet >/dev/null 2>&1 || true
elif $DRY_RUN; then
    info "[dry-run] would run doctor --fix to seed optional configs"
fi

pass "symlinks + gitignore + optional configs done"

# ─── P8: Commit ───────────────────────────────────────────────────────────────

header "P8: Commit migration"

if $DRY_RUN; then
    info "[dry-run] would git add -A && git commit"
else
    git -C "$HEX_DIR" add -A
    if [ -n "$(git -C "$HEX_DIR" status --porcelain)" ]; then
        git -C "$HEX_DIR" commit -m "migrate: hex v1 → v2 layout

Moved hex-owned content from .claude/ to .hex/. .claude/ narrowed to
Claude Code discovery (settings.json, commands/). Path refs rewritten
via enumerated-segment sed. Settings hook paths updated.
.agents/skills symlink created. Optional configs seeded.

Run /hex-doctor to verify. Run /hex-upgrade --dry-run — it should now
be a plain rsync against hex-foundation." \
            || die "commit failed"
        COMMIT_SHA=$(git -C "$HEX_DIR" rev-parse --short HEAD)
        pass "committed: $COMMIT_SHA"
    else
        warn "nothing to commit — migration was a no-op"
    fi
fi

# ─── P9: Validate ─────────────────────────────────────────────────────────────

if $SKIP_VALIDATE || $DRY_RUN; then
    header "Validate (skipped)"
    info "pass --skip-validate=false to run doctor + startup + upgrade --dry-run"
else
    header "P9: Validate"

    if [ -x "$DOCTOR" ]; then
        info "doctor.sh --quiet"
        HEX_DIR="$HEX_DIR" bash "$DOCTOR" --quiet >/dev/null 2>&1
        rc=$?
        if [ $rc -eq 0 ]; then pass "doctor: 0 errors, 0 warnings";
        elif [ $rc -eq 2 ]; then warn "doctor: warnings only (exit 2)";
        else fail "doctor exit=$rc"; exit 3; fi
    fi

    STARTUP="$HEX_DIR/.hex/scripts/startup.sh"
    if [ -x "$STARTUP" ]; then
        info "startup.sh (smoke)"
        if HEX_DIR="$HEX_DIR" bash "$STARTUP" >/dev/null 2>&1; then
            pass "startup ran cleanly"
        else
            warn "startup returned nonzero (review manually)"
        fi
    fi

    UPGRADE="$HEX_DIR/.hex/scripts/upgrade.sh"
    if [ -x "$UPGRADE" ]; then
        info "upgrade.sh --dry-run (should now be plain rsync)"
        if HEX_DIR="$HEX_DIR" bash "$UPGRADE" --dry-run >/dev/null 2>&1; then
            pass "upgrade --dry-run clean"
        else
            warn "upgrade --dry-run returned nonzero (check manually)"
        fi
    fi
fi

# ─── Report ───────────────────────────────────────────────────────────────────

header "Done"
info "Backup: ${BACKUP_DIR:-(skipped)}"
info "Next steps:"
echo "    1. Review the commit: git show HEAD"
echo "    2. Launch a fresh Claude Code session at $HEX_DIR"
echo "    3. Run /hex-upgrade whenever you want foundation content updates"
echo ""
echo "  Rollback: bash migrate-v1-to-v2.sh --rollback --hex-dir $HEX_DIR"
echo ""
