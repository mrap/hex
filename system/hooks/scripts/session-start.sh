#!/usr/bin/env bash
# Hex SessionStart hook — channel-scoped context injection.
# Fires on every Claude Code SessionStart.
#
# 2026-05-06: implements the long-standing TODO for channel→topic checkpoint
# resume. Channel keys matching `hex-<topic>` (with or without leading `#`)
# now surface projects/<topic>/checkpoint.md as additionalContext. The OKR
# overdue gate still takes precedence (single-output limitation).

set -uo pipefail

HEX_DIR="${CLAUDE_PROJECT_DIR:-${HEX_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}}"

{
  _ch="${CC_SESSION_KEY:-local-dev}"
  _pid=$$
  _ts="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  _payload=$(printf '{"channel":"%s","agent":"claude-code","pid":%d,"start_ts":"%s"}' \
    "$_ch" "$_pid" "$_ts")
  bash "$HEX_DIR/.hex/bin/hex-emit.sh" "session.start" "$_payload" "claude-code"
} 2>/dev/null &

# ─── Generalized blocker primitive ───────────────────────────────────────────
# Producers across the system drop blocker flags into .hex/state/blockers/*.flag
# (or okrs/_state/overdue.flag for the legacy OKR producer) and this hook
# surfaces them at session start. Each flag file is a plain-text payload —
# first line is the headline, rest is context. Filename ordering is the
# priority: `01-foo.flag` outranks `09-bar.flag`. Top 5 are surfaced inline;
# remainder collapse to "+N more" so a flood of flags doesn't drown signal.
#
# Producers wired today: okrs/_state/overdue.flag (OKR review overdue).
# Producer slots reserved (synthesis plan A4): cos max-escalation, suggestion
# frozen >7d, CRITICAL alert delivery failure. Add files to .hex/state/blockers/
# from any policy or script — no consumer wiring needed beyond this hook.
HEX_BLOCKER_DIR="$HEX_DIR/.hex/state/blockers"
HEX_BLOCKER_LIMIT=5
OVERDUE_FLAG="$HEX_DIR/okrs/_state/overdue.flag"

mapfile -t _flag_files < <(
  if [[ -d "$HEX_BLOCKER_DIR" ]]; then
    find "$HEX_BLOCKER_DIR" -maxdepth 1 -name '*.flag' -type f 2>/dev/null | sort
  fi
  [[ -f "$OVERDUE_FLAG" ]] && echo "$OVERDUE_FLAG"
)

if [[ ${#_flag_files[@]} -gt 0 ]]; then
  HEX_BLOCKER_LIMIT="$HEX_BLOCKER_LIMIT" python3 - "${_flag_files[@]}" <<'PYEOF'
import json, os, sys

limit = int(os.environ.get('HEX_BLOCKER_LIMIT', '5'))
paths = sys.argv[1:]

entries = []
for p in paths:
    try:
        with open(p, 'r', encoding='utf-8', errors='replace') as f:
            content = f.read().strip()
    except Exception as e:
        entries.append({'path': p, 'err': str(e), 'headline': '<read error>', 'detail': ''})
        continue
    lines = content.splitlines()
    headline = lines[0] if lines else '(empty)'
    detail = '\n'.join(lines[1:]).strip()
    entries.append({'path': p, 'headline': headline, 'detail': detail})

if not entries:
    sys.exit(0)

shown = entries[:limit]
overflow = len(entries) - len(shown)

parts = ['*** SESSION-START BLOCKERS — ADDRESS BEFORE PROCEEDING ***', '']
for i, e in enumerate(shown, 1):
    parts.append(f'### Blocker {i}: {e["headline"]}')
    parts.append(f'  source: {os.path.relpath(e["path"], os.environ.get("HEX_DIR", "/"))}')
    if e.get('detail'):
        parts.append('')
        parts.append(e['detail'][:1500])
    parts.append('')
if overflow > 0:
    parts.append(f'+ {overflow} more blocker(s) — see {os.path.relpath(os.path.dirname(shown[-1]["path"]), os.environ.get("HEX_DIR", "/"))}/')
parts.append('')
parts.append('Resolve or explicitly defer each blocker before normal session startup. Each producer documents its own clearance protocol.')

print(json.dumps({
    'hookSpecificOutput': {
        'hookEventName': 'SessionStart',
        'additionalContext': '\n'.join(parts),
    }
}))
PYEOF
  exit 0
fi

# ─── Channel → topic checkpoint resume ───────────────────────────────────────
# Map CC_SESSION_KEY → projects/<topic>/checkpoint.md when the channel name
# follows the `hex-<topic>` convention. CLAUDE.md FRESH state protocol relies
# on this to surface prior-session context for topic-scoped channels. The
# default `#hex-main` channel and `local-dev` (no cc-connect) are skipped —
# the assistant loads ops context (todo.md, landings, evolution) on its own.
_topic=""
case "${CC_SESSION_KEY:-}" in
  ""|local-dev|hex-main|"#hex-main")
    _topic=""
    ;;
  "#hex-"*|"hex-"*)
    _topic="${CC_SESSION_KEY#\#}"
    _topic="${_topic#hex-}"
    ;;
esac

# Sanitize the inferred topic. Critic 2026-05-06 noted that an attacker-
# controlled CC_SESSION_KEY (e.g., 'hex-../../etc/...') would let topic
# escape into the parent path. Reject anything that isn't a clean slug.
if [[ -n "$_topic" && ! "$_topic" =~ ^[a-zA-Z0-9_-]+$ ]]; then
  _topic=""
fi

if [[ -n "$_topic" ]]; then
  _ckpt="$HEX_DIR/projects/$_topic/checkpoint.md"
  if [[ -f "$_ckpt" ]]; then
    HEX_TOPIC="$_topic" HEX_CKPT="$_ckpt" python3 - <<'PYEOF'
import json, os
ckpt = os.environ['HEX_CKPT']
topic = os.environ['HEX_TOPIC']
try:
    with open(ckpt, 'r', encoding='utf-8', errors='replace') as f:
        content = f.read().strip()
except Exception as e:
    # Loud failure — surface the read error rather than swallow.
    msg = f'[session-start hook] failed to read checkpoint at {ckpt}: {e}'
    print(json.dumps({
        'hookSpecificOutput': {
            'hookEventName': 'SessionStart',
            'additionalContext': msg,
        }
    }))
    raise SystemExit(0)

# Cap context injection at ~4KB so the prompt budget stays sane.
preview = content[:4096]
truncated = '' if len(content) <= 4096 else f'\n\n[…truncated, full file at projects/{topic}/checkpoint.md]'
msg = (
    f"*** Topic-scoped session: projects/{topic}/checkpoint.md ***\n\n"
    f"Picking up where the prior session left off in this topic. Below is the "
    f"checkpoint content; review it before responding to the user.\n\n"
    f"{preview}{truncated}"
)
print(json.dumps({
    'hookSpecificOutput': {
        'hookEventName': 'SessionStart',
        'additionalContext': msg,
    }
}))
PYEOF
    exit 0
  fi
fi

