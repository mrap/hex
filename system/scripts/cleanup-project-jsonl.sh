#!/bin/bash
# cleanup-project-jsonl.sh — Delete Claude Code project .jsonl files older than 30 days.
# Invoked by hex-events policy weekly. Safe to re-run.
set -uo pipefail

PROJECTS_DIR="$HOME/.claude/projects"
RETENTION_DAYS="${1:-30}"
LOG_FILE="${AGENT_DIR:-$HOME/hex}/.hex/hooks/logs/cleanup-project-jsonl.log"
mkdir -p "$(dirname "$LOG_FILE")"

TS=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
DELETED=0
FREED=0

while IFS= read -r -d '' fpath; do
    sz=$(stat -f '%z' "$fpath" 2>/dev/null) || sz=0
    rm -f "$fpath" && DELETED=$((DELETED+1)) && FREED=$((FREED+sz))
done < <(find "$PROJECTS_DIR" -maxdepth 2 -name "*.jsonl" -mtime "+${RETENTION_DAYS}" -print0 2>/dev/null)

find "$PROJECTS_DIR" -mindepth 1 -maxdepth 1 -type d -empty -delete 2>/dev/null || true

printf '%s deleted=%d freed_bytes=%d retention_days=%d\n' "$TS" "$DELETED" "$FREED" "$RETENTION_DAYS" >> "$LOG_FILE"
printf '{"deleted":%d,"freed_bytes":%d,"retention_days":%d}\n' "$DELETED" "$FREED" "$RETENTION_DAYS"
