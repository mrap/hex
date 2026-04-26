#!/usr/bin/env bash
# hex-context-lib.sh — Registry helper functions for hex workspace contexts
# Source this file to get: ctx_get_active, ctx_set_active, ctx_push, ctx_pop,
#                          ctx_register, ctx_get, ctx_list
# JSON operations use python3 stdlib only.
set -u

if [ -z "${AGENT_DIR:-}" ]; then
  _ctx_lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  _ctx_candidate="$_ctx_lib_dir"
  while [ "$_ctx_candidate" != "/" ]; do
    if [ -f "$_ctx_candidate/CLAUDE.md" ]; then
      AGENT_DIR="$_ctx_candidate"
      break
    fi
    _ctx_candidate="$(dirname "$_ctx_candidate")"
  done
  AGENT_DIR="${AGENT_DIR:-$HOME/hex}"
  unset _ctx_lib_dir _ctx_candidate
fi
HEX_CONTEXTS_JSON="${HEX_CONTEXTS_JSON:-$AGENT_DIR/.hex/hex-contexts.json}"

# --- Internal helpers ---

_ctx_ensure_registry() {
  if [[ ! -f "$HEX_CONTEXTS_JSON" ]]; then
    local dir
    dir="$(dirname "$HEX_CONTEXTS_JSON")"
    mkdir -p "$dir"
    printf '{"active":"main","context_stack":["main"],"contexts":{}}' > "$HEX_CONTEXTS_JSON"
  fi
}

_ctx_read() {
  # $1 = python3 code; sys.argv[1]=JSON path, sys.argv[2+]=extra args passed after code
  local code="$1"; shift
  python3 - "$HEX_CONTEXTS_JSON" "$@" <<PYEOF
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
$code
PYEOF
}

_ctx_write() {
  # $1 = python3 code; sys.argv[1]=JSON path, sys.argv[2+]=extra args passed after code
  local code="$1"; shift
  python3 - "$HEX_CONTEXTS_JSON" "$@" <<PYEOF
import json, sys
path = sys.argv[1]
with open(path) as f:
    d = json.load(f)
$code
with open(path + '.tmp', 'w') as f:
    json.dump(d, f, indent=2)
PYEOF
  mv "${HEX_CONTEXTS_JSON}.tmp" "$HEX_CONTEXTS_JSON"
}

# --- Public API ---

# ctx_get_active — print name of active context
ctx_get_active() {
  _ctx_ensure_registry
  _ctx_read "print(d['active'])"
}

# ctx_set_active <name> — set active context (does not push stack)
ctx_set_active() {
  local name="${1:?ctx_set_active requires a context name}"
  _ctx_ensure_registry
  _ctx_write "
import datetime
name = sys.argv[2]
d['active'] = name
d['contexts'].setdefault(name, {})['last_active'] = datetime.datetime.now(datetime.timezone.utc).isoformat().replace('+00:00','Z')
" "$name"
}

# ctx_push <name> — push name onto context_stack and set as active
ctx_push() {
  local name="${1:?ctx_push requires a context name}"
  _ctx_ensure_registry
  _ctx_write "
import datetime
name = sys.argv[2]
stack = d.get('context_stack', [])
if not stack or stack[-1] != name:
    stack.append(name)
d['context_stack'] = stack
d['active'] = name
d['contexts'].setdefault(name, {})['last_active'] = datetime.datetime.now(datetime.timezone.utc).isoformat().replace('+00:00','Z')
" "$name"
}

# ctx_pop — pop context_stack and switch to previous context; prints new active
ctx_pop() {
  _ctx_ensure_registry
  _ctx_write "
stack = d.get('context_stack', [])
if len(stack) > 1:
    stack.pop()
    d['context_stack'] = stack
    d['active'] = stack[-1]
"
  ctx_get_active
}

# ctx_register <name> [session_id] [display_name] — add/update a context entry
ctx_register() {
  local name="${1:?ctx_register requires a context name}"
  local session_id="${2:-}"
  local display_name="${3:-$name}"
  _ctx_ensure_registry
  _ctx_write "
import datetime
name = sys.argv[2]
session_id = sys.argv[3]
display_name = sys.argv[4]
now = datetime.datetime.now(datetime.timezone.utc).isoformat().replace('+00:00','Z')
entry = d['contexts'].get(name, {})
entry['window'] = name
entry['display_name'] = display_name
if not entry.get('last_active'):
    entry['last_active'] = now
if session_id:
    entry['session_id'] = session_id
entry.setdefault('active_queue_ids', [])
entry.setdefault('status', 'idle')
d['contexts'][name] = entry
" "$name" "$session_id" "$display_name"
}

# ctx_get <name> [field] — print JSON for a context, or a specific field
ctx_get() {
  local name="${1:?ctx_get requires a context name}"
  local field="${2:-}"
  _ctx_ensure_registry
  if [[ -n "$field" ]]; then
    _ctx_read "
name = sys.argv[2]
field = sys.argv[3]
print(d['contexts'].get(name, {}).get(field, ''))
" "$name" "$field"
  else
    _ctx_read "
import json
name = sys.argv[2]
print(json.dumps(d['contexts'].get(name, {}), indent=2))
" "$name"
  fi
}

# ctx_list — list all context names, one per line; active prefixed with *
ctx_list() {
  _ctx_ensure_registry
  _ctx_read "
active = d.get('active', '')
for name in d.get('contexts', {}):
    prefix = '*' if name == active else ' '
    print(prefix + ' ' + name)
"
}
