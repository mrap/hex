#!/usr/bin/env bash
set -uo pipefail

AGENT_DIR="${AGENT_DIR:-${AGENT_DIR:-$HOME/hex}}"
PROJECT_ID="${1:-hex-os}"
BOI_DB="${HOME}/.boi/boi.db"

# Capture landings state to temp file (safe from special chars)
STATE_TMP=$(mktemp)
trap 'rm -f "$STATE_TMP"' EXIT

AGENT_DIR="$AGENT_DIR" bash "$AGENT_DIR/.hex/scripts/hex-landings-state.sh" > "$STATE_TMP" 2>/dev/null || true

# Count active BOI jobs
ACTIVE_BOI=0
if command -v sqlite3 &>/dev/null && [[ -f "$BOI_DB" ]]; then
  ACTIVE_BOI=$(sqlite3 "$BOI_DB" "SELECT COUNT(*) FROM specs WHERE status IN ('running','queued')" 2>/dev/null || echo 0)
fi
ACTIVE_BOI="${ACTIVE_BOI:-0}"

# Most recent decision date
LAST_DECISION="none"
DECISIONS_DIR="$AGENT_DIR/me/decisions"
if [[ -d "$DECISIONS_DIR" ]]; then
  _latest=$(ls -t "$DECISIONS_DIR"/*.md 2>/dev/null | head -1 || true)
  if [[ -n "${_latest:-}" ]]; then
    LAST_DECISION=$(date -r "$_latest" +%Y-%m-%d 2>/dev/null || echo "none")
  fi
fi

# Build JSON with python3
python3 - "$STATE_TMP" "$PROJECT_ID" "$ACTIVE_BOI" "$LAST_DECISION" <<'PYEOF'
import re, json, sys
from datetime import datetime, timezone

state_file  = sys.argv[1]
project_id  = sys.argv[2]
active_boi  = int(sys.argv[3]) if sys.argv[3].strip().isdigit() else 0
last_decision = sys.argv[4]

with open(state_file, encoding='utf-8') as f:
    state_output = f.read()

lands = []
lines = state_output.split('\n')
i = 0
while i < len(lines):
    line = lines[i]
    m = re.match(r'^(L\d+)\.\s+(.+)', line)
    if m:
        land_id = m.group(1)
        title   = m.group(2).strip()
        state_val = 'Unknown'
        holder    = '🧑'
        weekly    = ''

        if i + 1 < len(lines):
            sl = lines[i + 1]
            # Split on 2+ spaces to isolate key: value tokens
            parts = re.split(r'\s{2,}', sl.strip())
            for part in parts:
                if part.startswith('State:'):
                    state_val = part[len('State:'):].strip()
                elif part.startswith('Holder:'):
                    holder = part[len('Holder:'):].strip()
                elif part.startswith('Weekly:'):
                    weekly = part[len('Weekly:'):].strip()

        lands.append({
            'id': land_id,
            'title': title,
            'state': state_val,
            'holder': holder,
            'weekly_target': weekly,
        })
    i += 1

in_flight = sum(1 for l in lands if l['state'] not in ('Done', 'Blocked'))
blocked   = sum(1 for l in lands if l['state'] == 'Blocked')
done      = sum(1 for l in lands if l['state'] == 'Done')
summary   = f"{in_flight} in-flight, {blocked} blocked, {done} done"

result = {
    'project': project_id,
    'generated_at': datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'),
    'lands': lands,
    'active_boi_count': active_boi,
    'last_decision_date': last_decision,
    'summary_line': summary,
}
print(json.dumps(result, ensure_ascii=False))
PYEOF
