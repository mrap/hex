#!/usr/bin/env bash
# tech-scout.sh — Proactive tech research agent for hex.
#
# Reads Mike's current project state, generates research queries,
# searches multiple sources, filters for relevance, and writes briefs.
#
# Usage:
#   bash tech-scout.sh [--dry-run] [--verbose]
#   bash tech-scout.sh --dry-run    # Show plan only, no network calls
#
# Output:
#   ~/hex/raw/research/scout/YYYY-MM-DD.md   — daily brief
#   ~/hex/raw/research/scout/index.md        — running index
#
# Manual invocation:
#   bash ~/hex/.hex/scripts/tech-scout.sh
#
# Or via hex command (in Claude session):
#   /hex-scout run

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON="${PYTHON:-python3}"

exec "$PYTHON" "$SCRIPT_DIR/tech-scout.py" "$@"
