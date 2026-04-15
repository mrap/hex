#!/usr/bin/env bash
# hex upgrade — Pull latest system files from the hex repo.
# Usage: bash .hex/scripts/upgrade.sh [--dry-run] [--skip-boi] [--skip-events]
set -euo pipefail

HEX_DIR="${HEX_DIR:-$(pwd)}"
DRY_RUN=false
SKIP_BOI=false
SKIP_EVENTS=false

for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=true ;;
        --skip-boi) SKIP_BOI=true ;;
        --skip-events) SKIP_EVENTS=true ;;
    esac
done

CURRENT_VERSION=$(cat "$HEX_DIR/.hex/version.txt" 2>/dev/null || echo "unknown")
REPO_URL="${HEX_REPO_URL:-https://github.com/mrap/hex-foundation.git}"
TMPDIR=$(mktemp -d)

cleanup() {
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

echo "hex upgrade"
echo "==========="
echo "Current version: $CURRENT_VERSION"
echo ""

# Phase 1: Fetch latest
echo "[1/5] Fetching latest..."
git clone --depth 1 "$REPO_URL" "$TMPDIR/hex-repo" 2>/dev/null
NEW_VERSION=$(cat "$TMPDIR/hex-repo/system/version.txt" 2>/dev/null || echo "unknown")
echo "  Latest version: $NEW_VERSION"

if [ "$CURRENT_VERSION" = "$NEW_VERSION" ]; then
    echo "  Already up to date."
    exit 0
fi

if [ "$DRY_RUN" = true ]; then
    echo ""
    echo "[dry-run] Would upgrade from $CURRENT_VERSION to $NEW_VERSION"
    echo "[dry-run] Files that would change:"
    diff -rq "$HEX_DIR/.hex/" "$TMPDIR/hex-repo/system/" 2>/dev/null | head -20 || true
    exit 0
fi

# Phase 2: Backup
echo "[2/5] Backing up current .hex/..."
BACKUP_DIR="$HEX_DIR/.hex-upgrade-backup-$(date +%Y%m%d)"
cp -r "$HEX_DIR/.hex" "$BACKUP_DIR"
echo "  Backup: $BACKUP_DIR"

# Phase 3: Update system files
echo "[3/5] Updating system files..."
# Preserve memory.db, replace everything else
cp "$HEX_DIR/.hex/memory.db" "$TMPDIR/memory.db.bak" 2>/dev/null || true
rm -rf "$HEX_DIR/.hex"
cp -r "$TMPDIR/hex-repo/system" "$HEX_DIR/.hex"
cp "$TMPDIR/memory.db.bak" "$HEX_DIR/.hex/memory.db" 2>/dev/null || true
echo "  System files updated"

# Phase 4: Merge CLAUDE.md (preserve user zone)
echo "[4/5] Merging CLAUDE.md..."
python3 << 'PYEOF'
import sys, os

hex_dir = os.environ.get("HEX_DIR", os.getcwd())
current = os.path.join(hex_dir, "CLAUDE.md")
template = os.path.join(os.environ.get("TMPDIR", "/tmp"), "hex-repo", "templates", "CLAUDE.md")

if not os.path.exists(current) or not os.path.exists(template):
    print("  Skipped (files not found)")
    sys.exit(0)

with open(current) as f:
    current_text = f.read()
with open(template) as f:
    template_text = f.read()

# Extract user zone from current CLAUDE.md
user_start = "<!-- hex:user-start"
user_end = "<!-- hex:user-end"

user_content = ""
in_user = False
for line in current_text.split("\n"):
    if user_start in line:
        in_user = True
        continue
    if user_end in line:
        in_user = False
        continue
    if in_user:
        user_content += line + "\n"

# Replace user zone in template with preserved content
result_lines = []
in_template_user = False
for line in template_text.split("\n"):
    if user_start in line:
        result_lines.append(line)
        result_lines.append(user_content.rstrip())
        in_template_user = True
        continue
    if user_end in line:
        in_template_user = False
        result_lines.append(line)
        continue
    if not in_template_user:
        result_lines.append(line)

with open(current, "w") as f:
    f.write("\n".join(result_lines))

print("  CLAUDE.md merged (system zone updated, user zone preserved)")
PYEOF

# Also update AGENTS.md (no zone merge needed — full replace)
cp "$TMPDIR/hex-repo/templates/AGENTS.md" "$HEX_DIR/AGENTS.md"
echo "  AGENTS.md updated"

# Update commands
if [ -d "$TMPDIR/hex-repo/system/commands" ]; then
    mkdir -p "$HEX_DIR/.claude/commands"
    cp "$TMPDIR/hex-repo/system/commands/"*.md "$HEX_DIR/.claude/commands/"
    echo "  Commands updated"
fi

# Phase 5: Post-upgrade
echo "[5/5] Post-upgrade checks..."
bash "$HEX_DIR/.hex/scripts/doctor.sh" 2>/dev/null || true

echo ""
echo "Upgraded from $CURRENT_VERSION to $NEW_VERSION"
echo "Backup at: $BACKUP_DIR"
