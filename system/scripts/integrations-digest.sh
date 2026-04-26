#!/usr/bin/env bash
# integrations-digest.sh — Post weekly integrations summary to #integrations
set -uo pipefail

STATE_DIR="/Users/mrap/mrap-hex/projects/integrations/_state"
SLACK_SCRIPT="/Users/mrap/mrap-hex/.hex/scripts/slack-post.sh"
CHANNEL="C0AUJJ63CG4"

DIGEST=$(python3 - "$STATE_DIR" <<'PYEOF'
import json, os, sys, datetime

state_dir = sys.argv[1]
rows = []
if os.path.isdir(state_dir):
    for fname in sorted(os.listdir(state_dir)):
        if not fname.endswith('.json') or fname.startswith('.'):
            continue
        fpath = os.path.join(state_dir, fname)
        try:
            with open(fpath) as f:
                d = json.load(f)
        except Exception:
            continue
        name = fname[:-5]
        status = d.get('status', '?')
        streak = d.get('streak', 0)
        last_ok = d.get('last_ok') or '-'
        if last_ok != '-':
            try:
                dt = datetime.datetime.fromisoformat(last_ok.replace('Z', '+00:00'))
                last_ok = dt.strftime('%Y-%m-%d %H:%M')
            except Exception:
                pass
        icon = ':large_green_circle:' if status == 'ok' else ':red_circle:' if status == 'fail' else ':white_circle:'
        rows.append(f"{icon} *{name}*  status={status}  streak={streak}  last_ok={last_ok}")

if not rows:
    print(":white_circle: No integration state data found yet.")
else:
    header = f":calendar: *Weekly Integrations Digest* — {datetime.date.today().strftime('%Y-%m-%d')}"
    body = "\n".join(rows)
    print(f"{header}\n\n{body}")
PYEOF
)

bash "$SLACK_SCRIPT" --channel "$CHANNEL" --text "$DIGEST"
