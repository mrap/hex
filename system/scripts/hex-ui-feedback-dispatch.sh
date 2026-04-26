#!/usr/bin/env bash
# hex-ui-feedback-dispatch.sh
#
# Runs every 3 minutes via cron. For each new comment on the pitch-site,
# writes a small BOI spec and dispatches it. BOI workers handle the actual
# work in parallel (up to 5). Each comment gets its own spec, its own
# git commit, its own audit log.
#
# Replaces the prior headless-claude tick approach.

set -uo pipefail

PATH="/Users/mrap/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
export PATH

API="https://mac-mini.tailbd5748.ts.net/visions/api/comments"
LOG="/tmp/hex-ui-feedback-loop.log"
LOCK="/tmp/hex-ui-feedback-loop.lock"
SPEC_DIR="/Users/mrap/mrap-hex/specs/feedback-comments"
PROJECT_DIR="/Users/mrap/mrap-hex"

mkdir -p "$SPEC_DIR"

# Lock against concurrent ticks
if [ -e "$LOCK" ]; then
  LOCK_PID=$(cat "$LOCK" 2>/dev/null)
  if [ -n "$LOCK_PID" ] && kill -0 "$LOCK_PID" 2>/dev/null; then
    echo "=== $(date) === tick skipped (lock held by pid $LOCK_PID)" >> "$LOG"
    exit 0
  fi
fi
echo $$ > "$LOCK"
trap 'rm -f "$LOCK"' EXIT

# Fetch new comments
NEW_JSON=$(curl -sk --max-time 5 "$API" 2>/dev/null | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    news = [c for c in d.get('comments', []) if c.get('status') == 'new']
    print(json.dumps(news))
except Exception:
    print('[]')
" 2>/dev/null)
NEW_JSON=${NEW_JSON:-"[]"}

NEW_COUNT=$(echo "$NEW_JSON" | python3 -c "import sys, json; print(len(json.load(sys.stdin)))" 2>/dev/null)
NEW_COUNT=${NEW_COUNT:-0}

if [ "$NEW_COUNT" = "0" ]; then
  MIN=$(date +%M)
  if [ "$MIN" = "00" ]; then
    echo "=== $(date) === idle (no new comments)" >> "$LOG"
  fi
  exit 0
fi

echo "=== $(date) === $NEW_COUNT new comment(s) — dispatching BOI specs" >> "$LOG"

# Iterate each new comment, write a BOI spec, dispatch
python3 <<'PY' >> "$LOG" 2>&1
import json, os, subprocess, re
import urllib.request, ssl

ctx = ssl._create_unverified_context()
with urllib.request.urlopen('https://mac-mini.tailbd5748.ts.net/visions/api/comments', context=ctx, timeout=5) as r:
    full = json.load(r)
data = [c for c in full.get('comments', []) if c.get('status') == 'new']

SPEC_DIR = "/Users/mrap/mrap-hex/specs/feedback-comments"
DISPATCH_LIMIT = 3  # per tick

def slug(s):
    return re.sub(r'[^a-z0-9]+', '-', s.lower())[:40].strip('-')

dispatched = 0
for c in data:
    if dispatched >= DISPATCH_LIMIT:
        break
    cid = c['id']
    demo_id = c.get('demo_id', 'general')
    text = c.get('text', '').strip()
    sp = f"{SPEC_DIR}/{cid}.spec.md"
    if os.path.exists(sp):
        # already dispatched (or at least spec exists). skip.
        print(f"  spec exists for {cid}; skipping re-dispatch")
        continue

    content = f"""# hex-ui feedback · {cid}

**Mode:** execute
**Source comment:** {cid} on `{demo_id}`

## Mike's comment (verbatim)

> {text}

## Context

This is a feedback comment left by Mike on the hex-ui pitch site at
`https://mac-mini.tailbd5748.ts.net/demos`. Mark it building now, make the
change he asked for, post an inline reply, mark built. All comments live
in `projects/hex-ui/feedback/comments.json` and are served via the
pitch-site API at `/visions/api/comments`.

## Guardrails

- Only modify files under `projects/hex-ui/vision-pitch-site/`.
- Respect `projects/hex-ui/design-principles-2026-04-21.md` and the
  rejected-patterns memory (`~/.claude/projects/-Users-mrap-mrap-hex/memory/feedback_hex_ui_rejected_patterns.md`).
- Light mode default. Tailscale URLs only.
- Do NOT modify `projects/hex-ui/feedback/comments.json`.
- If the comment is ambiguous, POST a clarifying reply and leave status as `new`.

### t-1: Process the comment
PENDING

**Spec:**
1. `curl -sk -X POST -H "Content-Type: application/json" -d '{{\\"status\\":\\"building\\"}}' https://mac-mini.tailbd5748.ts.net/visions/api/comments/{cid}/status`
2. Read the comment text above. Identify the scope (demo `{demo_id}`).
3. Edit the relevant files under `projects/hex-ui/vision-pitch-site/` to address the feedback.
4. Verify the served HTML reflects the change (curl with a cache-bust query).
5. POST a 1–3 sentence reply describing what changed:
   `curl -sk -X POST -H "Content-Type: application/json" -d '{{"reply":"..."}}' https://mac-mini.tailbd5748.ts.net/visions/api/comments/{cid}/reply`
6. POST `status=built`:
   `curl -sk -X POST -H "Content-Type: application/json" -d '{{"status":"built"}}' https://mac-mini.tailbd5748.ts.net/visions/api/comments/{cid}/status`

**Verify:** `curl -sk https://mac-mini.tailbd5748.ts.net/visions/api/comments | python3 -c "import sys,json; d=json.load(sys.stdin); c=[x for x in d['comments'] if x['id']=='{cid}'][0]; assert c['status']=='built', f'status still {{c[\\"status\\"]}}'"`
"""

    with open(sp, 'w') as f:
        f.write(content)

    # dispatch
    r = subprocess.run(
        ["bash", "/Users/mrap/.boi/boi", "dispatch", sp],
        capture_output=True, text=True, timeout=30
    )
    if r.returncode == 0:
        # extract queue id from output
        m = re.search(r'(q-\d+)', r.stdout + r.stderr)
        qid = m.group(1) if m else '?'
        print(f"  {cid} → {qid} ({demo_id})")
        dispatched += 1
    else:
        print(f"  {cid} DISPATCH FAILED: {r.stderr[:200]}")

print(f"dispatched {dispatched} spec(s)")
PY

echo "=== $(date) === dispatch tick done" >> "$LOG"
