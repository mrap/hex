#!/usr/bin/env bash
# hex-agent-spawn.sh — factory script for spawning new hex agents
# Usage: bash hex-agent-spawn.sh <role-spec-file.yaml>
set -uo pipefail

HEX_DIR="/Users/mrap/mrap-hex"
TEMPLATES_DIR="$HEX_DIR/.hex/templates/agent"
POLICIES_DIR="$HOME/.hex-events/policies"
SPAWNS_LOG_DIR="$HEX_DIR/projects/hex-agents/_spawns"
AGENTS_MD="$HEX_DIR/projects/hex-agents/AGENTS.md"
DECISIONS_DIR="$HEX_DIR/me/decisions"
HEX_EVENTS_CLI="/Users/mrap/github.com/mrap/hex-events/hex_events_cli.py"

RESERVED_IDS="mike hex hex-main hex-agents hex-v2-team"

# ── helpers ──────────────────────────────────────────────────────────────────

die() { echo "ERROR: $*" >&2; exit 1; }

usage() {
  echo "Usage: hex-agent-spawn.sh <role-spec-file.yaml>" >&2
  echo "  role-spec-file.yaml  Required fields: id, name, role, scope, reason, parent," >&2
  echo "                       wake_triggers, authority.{green,yellow,red}," >&2
  echo "                       memory_access.{read_tiers,write_paths}," >&2
  echo "                       budget.{wakes_per_hour,usd_per_day}, escalation_channel" >&2
  exit 1
}

yaml_field() {
  # Extract a scalar field from YAML using Python
  python3 - "$1" "$2" <<'PYEOF'
import sys, re
path = sys.argv[1]
key  = sys.argv[2]
with open(path) as f:
    content = f.read()
# simple scalar match (key: value) — works for all required scalar fields
m = re.search(r'^\s*' + re.escape(key) + r'\s*:\s*(.+)', content, re.MULTILINE)
if m:
    val = m.group(1).strip().strip('"').strip("'")
    print(val)
PYEOF
}

yaml_list() {
  # Extract a list field, returning space-separated items
  python3 - "$1" "$2" <<'PYEOF'
import sys, yaml
path  = sys.argv[1]
*keys = sys.argv[2].split('.')
with open(path) as f:
    data = yaml.safe_load(f)
node = data
for k in keys:
    if node is None or k not in node:
        sys.exit(0)
    node = node[k]
if isinstance(node, list):
    for item in node:
        print(str(item))
PYEOF
}

yaml_count_parent_spawns() {
  local spawns_file="$1"
  local parent="$2"
  if [[ ! -f "$spawns_file" ]]; then echo 0; return; fi
  python3 - "$spawns_file" "$parent" <<'PYEOF'
import sys, json
path   = sys.argv[1]
parent = sys.argv[2]
count  = 0
with open(path) as f:
    for line in f:
        line = line.strip()
        if not line: continue
        try:
            obj = json.loads(line)
            if obj.get('parent') == parent:
                count += 1
        except Exception:
            pass
print(count)
PYEOF
}

# ── rollback state ────────────────────────────────────────────────────────────
ROLLBACK_FILES=()
ROLLBACK_DIRS=()
ROLLBACK_HALT=""
ROLLBACK_AGENTS_MD_LINES=0

rollback() {
  echo "Rolling back spawn artifacts…" >&2
  for f in "${ROLLBACK_FILES[@]:-}"; do
    [[ -n "$f" && -f "$f" ]] && rm -f "$f"
  done
  for d in "${ROLLBACK_DIRS[@]:-}"; do
    [[ -n "$d" && -d "$d" ]] && rm -rf "$d"
  done
  if [[ -n "${ROLLBACK_HALT:-}" && -f "${ROLLBACK_HALT:-}" ]]; then
    rm -f "$ROLLBACK_HALT"
  fi
  # Trim appended AGENTS.md lines
  if [[ ${ROLLBACK_AGENTS_MD_LINES:-0} -gt 0 && -f "$AGENTS_MD" ]]; then
    TOTAL=$(wc -l < "$AGENTS_MD")
    KEEP=$((TOTAL - ROLLBACK_AGENTS_MD_LINES))
    if [[ $KEEP -gt 0 ]]; then
      TMPF=$(mktemp)
      head -n "$KEEP" "$AGENTS_MD" > "$TMPF" && mv "$TMPF" "$AGENTS_MD"
    fi
  fi
}

# ── step 1: parse argv ────────────────────────────────────────────────────────
if [[ $# -lt 1 ]]; then usage; fi
SPEC_FILE="$1"
[[ -f "$SPEC_FILE" ]] || die "role-spec file not found or unreadable: $SPEC_FILE"

# ── step 2: validate required fields ─────────────────────────────────────────
REQUIRED_SCALARS="id name role scope reason parent escalation_channel"
for field in $REQUIRED_SCALARS; do
  val=$(yaml_field "$SPEC_FILE" "$field")
  [[ -n "$val" ]] || die "role-spec missing required field: $field"
done

AGENT_ID=$(yaml_field   "$SPEC_FILE" "id")
AGENT_NAME=$(yaml_field "$SPEC_FILE" "name")
AGENT_ROLE=$(yaml_field "$SPEC_FILE" "role")
AGENT_SCOPE=$(yaml_field "$SPEC_FILE" "scope")
AGENT_REASON=$(yaml_field "$SPEC_FILE" "reason")
AGENT_PARENT=$(yaml_field "$SPEC_FILE" "parent")
ESCALATION_CHANNEL=$(yaml_field "$SPEC_FILE" "escalation_channel")
BUDGET_WPH=$(yaml_field "$SPEC_FILE" "wakes_per_hour") || true
BUDGET_USD=$(yaml_field "$SPEC_FILE" "usd_per_day")   || true

# budget fields are nested — try nested paths if scalar failed
if [[ -z "${BUDGET_WPH:-}" ]]; then
  BUDGET_WPH=$(python3 -c "
import sys, yaml
with open('$SPEC_FILE') as f: d = yaml.safe_load(f)
print(d.get('budget',{}).get('wakes_per_hour',''))
" 2>/dev/null) || true
fi
if [[ -z "${BUDGET_USD:-}" ]]; then
  BUDGET_USD=$(python3 -c "
import sys, yaml
with open('$SPEC_FILE') as f: d = yaml.safe_load(f)
print(d.get('budget',{}).get('usd_per_day',''))
" 2>/dev/null) || true
fi

[[ -n "${BUDGET_WPH:-}" ]] || die "role-spec missing required field: budget.wakes_per_hour"
[[ -n "${BUDGET_USD:-}" ]] || die "role-spec missing required field: budget.usd_per_day"

# Validate wake_triggers, authority, memory_access exist (list fields)
python3 - "$SPEC_FILE" <<'PYEOF' || die "role-spec missing required list fields (wake_triggers, authority.*, memory_access.*)"
import sys, yaml
with open(sys.argv[1]) as f:
    d = yaml.safe_load(f)
assert d.get('wake_triggers'), "wake_triggers empty"
assert 'authority' in d, "authority missing"
assert 'green'  in d['authority'], "authority.green missing"
assert 'yellow' in d['authority'], "authority.yellow missing"
assert 'red'    in d['authority'], "authority.red missing"
assert 'memory_access' in d, "memory_access missing"
assert 'read_tiers'  in d['memory_access'], "memory_access.read_tiers missing"
assert 'write_paths' in d['memory_access'], "memory_access.write_paths missing"
PYEOF

# ── step 3: validate id ───────────────────────────────────────────────────────
for reserved in $RESERVED_IDS; do
  [[ "$AGENT_ID" != "$reserved" ]] || die "id '$AGENT_ID' is reserved"
done

[[ ! -d "$HEX_DIR/projects/$AGENT_ID" ]] \
  || die "projects/$AGENT_ID/ already exists"
[[ ! -f "$HEX_DIR/.hex/bin/${AGENT_ID}-wake.sh" ]] \
  || die ".hex/bin/${AGENT_ID}-wake.sh already exists"
[[ ! -f "$POLICIES_DIR/${AGENT_ID}-agent.yaml" ]] \
  || die "policy ${AGENT_ID}-agent.yaml already exists"

# ── step 4: validate spawn rate ───────────────────────────────────────────────
TODAY=$(date -u +"%Y-%m-%d")
SPAWNS_FILE="$SPAWNS_LOG_DIR/$TODAY.jsonl"
PARENT_COUNT=$(yaml_count_parent_spawns "$SPAWNS_FILE" "$AGENT_PARENT")
if [[ "$PARENT_COUNT" -ge 5 ]]; then
  die "Parent '$AGENT_PARENT' has reached the 5 spawns/day limit. Escalate to Mike in #cos."
fi

# ── step 5: build template variables and render ───────────────────────────────
SPAWN_TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
STATE_DIR="$HEX_DIR/projects/$AGENT_ID"
CHARTER_PATH="$STATE_DIR/charter.md"
BOARD_PATH="$STATE_DIR/board.md"
LOG_PATH="$STATE_DIR/log.jsonl"
WAKE_SCRIPT_PATH="$HEX_DIR/.hex/bin/${AGENT_ID}-wake.sh"
HALT_FILE="$HOME/.hex-${AGENT_ID}-HALT"
POLICY_PATH="$POLICIES_DIR/${AGENT_ID}-agent.yaml"

# Build indented block strings from YAML list fields using Python
build_blocks() {
  python3 - "$SPEC_FILE" <<'PYEOF'
import sys, yaml

with open(sys.argv[1]) as f:
    d = yaml.safe_load(f)

def indent_list(items, prefix="    - "):
    if not items:
        return prefix + "(none)"
    return "\n".join(f"{prefix}{item}" for item in items)

wt = d.get('wake_triggers', [])
auth = d.get('authority', {})
ma = d.get('memory_access', {})

print("WAKE_TRIGGERS_BLOCK<<EOF")
print(indent_list(wt))
print("EOF")

print("GREEN_ACTIONS_BLOCK<<EOF")
print(indent_list(auth.get('green', [])))
print("EOF")

print("YELLOW_ACTIONS_BLOCK<<EOF")
print(indent_list(auth.get('yellow', [])))
print("EOF")

print("RED_ACTIONS_BLOCK<<EOF")
print(indent_list(auth.get('red', [])))
print("EOF")

print("READ_TIERS_BLOCK<<EOF")
print(indent_list(ma.get('read_tiers', []), prefix="    - "))
print("EOF")

print("WRITE_PATHS_BLOCK<<EOF")
print(indent_list(ma.get('write_paths', []), prefix="      - "))
print("EOF")

# Policy trigger rules: one rule per trigger
rules_events = "\n".join(f"    - {t}" for t in wt)
print("WAKE_TRIGGERS_RULES_EVENTS<<EOF")
print(rules_events)
print("EOF")

rules = []
for trigger in wt:
    safe_name = trigger.replace('.', '-')
    rules.append(f"""  - name: wake-on-{safe_name}
    trigger:
      event: {trigger}
    actions:
      - type: shell
        command: bash {HEX_DIR}/.hex/bin/{AGENT_ID}-wake.sh {trigger} '{{{{ event | tojson }}}}'
        timeout: 600
        on_success:
          - type: emit
            event: hex.agent.{AGENT_ID}.wake
            payload:
              trigger: {trigger}
              timestamp: "{{{{ now.isoformat() }}}}" """.replace('{HEX_DIR}', sys.argv[2]).replace('{AGENT_ID}', sys.argv[3]))

print("WAKE_TRIGGERS_RULES<<EOF")
print("\n".join(rules))
print("EOF")
PYEOF
}

# Export for the Python script
HEX_DIR_EXPORT="$HEX_DIR"
AGENT_ID_EXPORT="$AGENT_ID"

eval "$(python3 - "$SPEC_FILE" "$HEX_DIR" "$AGENT_ID" <<'PYEOF'
import sys, yaml

spec_file = sys.argv[1]
hex_dir   = sys.argv[2]
agent_id  = sys.argv[3]

with open(spec_file) as f:
    d = yaml.safe_load(f)

def indent_list(items, prefix="    - "):
    if not items:
        return prefix + "(none)"
    return "\n".join(f"{prefix}{item}" for item in items)

wt   = d.get('wake_triggers', [])
auth = d.get('authority', {})
ma   = d.get('memory_access', {})

def q(s):
    return s.replace("'", "'\\''")

# We output shell variable assignments using heredoc-safe base64 to avoid quoting issues
import base64

def export_b64(name, value):
    encoded = base64.b64encode(value.encode()).decode()
    print(f"export {name}_B64='{encoded}'")

export_b64("WAKE_TRIGGERS", "\n".join(f"    - {t}" for t in wt))
export_b64("GREEN_ACTIONS",  "\n".join(f"    - {a}" for a in auth.get('green', [])) or "    - (none)")
export_b64("YELLOW_ACTIONS", "\n".join(f"    - {a}" for a in auth.get('yellow', [])) or "    - (none)")
export_b64("RED_ACTIONS",    "\n".join(f"    - {a}" for a in auth.get('red', [])) or "    - (none)")
export_b64("READ_TIERS",     "\n".join(f"    - {t}" for t in ma.get('read_tiers', [])))
export_b64("WRITE_PATHS",    "\n".join(f"      - {p}" for p in ma.get('write_paths', [])))
export_b64("WAKE_TRIGGERS_RULES_EVENTS", "\n".join(f"    - {t}" for t in wt))

rules = []
for trigger in wt:
    safe_name = trigger.replace('.', '-')
    rules.append(f"  - name: wake-on-{safe_name}\n    trigger:\n      event: {trigger}\n    actions:\n      - type: shell\n        command: bash {hex_dir}/.hex/bin/{agent_id}-wake.sh {trigger} '{{{{ event | tojson }}}}'\n        timeout: 600\n        on_success:\n          - type: emit\n            event: hex.agent.{agent_id}.wake\n            payload:\n              trigger: {trigger}\n              timestamp: \"{{{{ now.isoformat() }}}}\"")
export_b64("WAKE_TRIGGERS_RULES", "\n".join(rules))
PYEOF
)"

decode_b64() { python3 -c "import sys, base64; print(base64.b64decode(sys.argv[1]).decode(), end='')" "$1"; }

WAKE_TRIGGERS=$(decode_b64 "$WAKE_TRIGGERS_B64")
GREEN_ACTIONS=$(decode_b64 "$GREEN_ACTIONS_B64")
YELLOW_ACTIONS=$(decode_b64 "$YELLOW_ACTIONS_B64")
RED_ACTIONS=$(decode_b64 "$RED_ACTIONS_B64")
READ_TIERS=$(decode_b64 "$READ_TIERS_B64")
WRITE_PATHS=$(decode_b64 "$WRITE_PATHS_B64")
WAKE_TRIGGERS_RULES_EVENTS=$(decode_b64 "$WAKE_TRIGGERS_RULES_EVENTS_B64")
WAKE_TRIGGERS_RULES=$(decode_b64 "$WAKE_TRIGGERS_RULES_B64")

render_template() {
  local tpl="$1"
  local out="$2"
  python3 - "$tpl" "$out" \
    "$AGENT_ID" "$AGENT_NAME" "$AGENT_ROLE" "$AGENT_SCOPE" \
    "$AGENT_PARENT" "$SPAWN_TIMESTAMP" \
    "$BUDGET_WPH" "$BUDGET_USD" "$ESCALATION_CHANNEL" \
    "$STATE_DIR" "$HALT_FILE" "$CHARTER_PATH" "$BOARD_PATH" "$LOG_PATH" \
    "$WAKE_SCRIPT_PATH" "$POLICY_PATH" \
    "$WAKE_TRIGGERS" "$GREEN_ACTIONS" "$YELLOW_ACTIONS" "$RED_ACTIONS" \
    "$READ_TIERS" "$WRITE_PATHS" "$WAKE_TRIGGERS_RULES_EVENTS" "$WAKE_TRIGGERS_RULES" \
    <<'PYEOF'
import sys

tpl_path = sys.argv[1]
out_path  = sys.argv[2]
(agent_id, agent_name, agent_role, agent_scope,
 agent_parent, spawn_ts,
 budget_wph, budget_usd, esc_channel,
 state_dir, halt_file, charter_path, board_path, log_path,
 wake_script_path, policy_path,
 wake_triggers, green_actions, yellow_actions, red_actions,
 read_tiers, write_paths, wt_rules_events, wt_rules) = sys.argv[3:]

with open(tpl_path) as f:
    content = f.read()

replacements = {
    '{{ID}}':                        agent_id,
    '{{NAME}}':                      agent_name,
    '{{ROLE}}':                      agent_role,
    '{{SCOPE}}':                     agent_scope,
    '{{PARENT}}':                    agent_parent,
    '{{SPAWN_TIMESTAMP}}':           spawn_ts,
    '{{BUDGET_WPH}}':                budget_wph,
    '{{BUDGET_USD}}':                budget_usd,
    '{{ESCALATION_CHANNEL}}':        esc_channel,
    '{{STATE_DIR}}':                 state_dir,
    '{{HALT_FILE}}':                 halt_file,
    '{{CHARTER_PATH}}':              charter_path,
    '{{BOARD_PATH}}':                board_path,
    '{{LOG_PATH}}':                  log_path,
    '{{WAKE_SCRIPT_PATH}}':          wake_script_path,
    '{{KILL_SWITCH_PATH}}':          halt_file,
    '{{RATE_LIMIT_WPH}}':            budget_wph,
    '{{WAKE_TRIGGERS}}':             wake_triggers,
    '{{GREEN_ACTIONS}}':             green_actions,
    '{{YELLOW_ACTIONS}}':            yellow_actions,
    '{{RED_ACTIONS}}':               red_actions,
    '{{READ_TIERS}}':                read_tiers,
    '{{WRITE_PATHS}}':               write_paths,
    '{{WAKE_TRIGGERS_RULES_EVENTS}}': wt_rules_events,
    '{{WAKE_TRIGGERS_RULES}}':       wt_rules,
}

for placeholder, value in replacements.items():
    content = content.replace(placeholder, value)

import os
os.makedirs(os.path.dirname(out_path), exist_ok=True)
tmp = out_path + '.tmp'
with open(tmp, 'w') as f:
    f.write(content)
os.rename(tmp, out_path)
PYEOF
}

# ── create project directory and render templates ─────────────────────────────
mkdir -p "$STATE_DIR"
ROLLBACK_DIRS+=("$STATE_DIR")

render_template "$TEMPLATES_DIR/charter.yaml.tpl" "$STATE_DIR/charter.yaml"
render_template "$TEMPLATES_DIR/charter.md.tpl"   "$STATE_DIR/charter.md"

# Initialize board, state, log, checkpoint, UNDO files
cat > "$STATE_DIR/board.md.tmp" <<BOARD
# $AGENT_NAME — board

**State:** HALTED (pending activation)
**Created:** $SPAWN_TIMESTAMP
**Parent:** $AGENT_PARENT

## Backlog
_Empty — activate agent to begin_

## In Progress
_None_

## Done
_None_
BOARD
mv "$STATE_DIR/board.md.tmp" "$STATE_DIR/board.md"

echo '{}' > "$STATE_DIR/state.md"
touch "$STATE_DIR/log.jsonl"
touch "$STATE_DIR/checkpoint.md"

cat > "$STATE_DIR/UNDO.md.tmp" <<UNDO
# UNDO — $AGENT_NAME ($AGENT_ID)

Run these commands to fully dissolve this agent:

\`\`\`bash
rm -rf $STATE_DIR
rm -f  $WAKE_SCRIPT_PATH
rm -f  $POLICY_PATH
rm -f  $HALT_FILE
sed -i '' '/$AGENT_ID/d' $AGENTS_MD
\`\`\`

Also remove the spawn entry from \`projects/hex-agents/_spawns/$TODAY.jsonl\`.
UNDO
mv "$STATE_DIR/UNDO.md.tmp" "$STATE_DIR/UNDO.md"

# Render wake script and policy
render_template "$TEMPLATES_DIR/wake.sh.tpl"    "$WAKE_SCRIPT_PATH"
ROLLBACK_FILES+=("$WAKE_SCRIPT_PATH")

render_template "$TEMPLATES_DIR/policy.yaml.tpl" "$POLICY_PATH"
ROLLBACK_FILES+=("$POLICY_PATH")

# ── step 6: chmod wake script ─────────────────────────────────────────────────
chmod +x "$WAKE_SCRIPT_PATH"

# ── step 7: create HALT file ──────────────────────────────────────────────────
touch "$HALT_FILE"
ROLLBACK_HALT="$HALT_FILE"

# ── step 8: append registry row ───────────────────────────────────────────────
ROLLBACK_AGENTS_MD_LINES=1
printf '| %s | %s | `%s` | `.hex/bin/%s-wake.sh` | `%s/%s-agent.yaml` | `touch %s` |\n' \
  "$AGENT_ID" "$AGENT_SCOPE" "projects/$AGENT_ID/" "$AGENT_ID" \
  "~/.hex-events/policies" "$AGENT_ID" "$HALT_FILE" \
  >> "$AGENTS_MD"

# ── step 9: append spawn audit JSONL ─────────────────────────────────────────
mkdir -p "$SPAWNS_LOG_DIR"
AUDIT_FILE="$SPAWNS_LOG_DIR/$TODAY.jsonl"
python3 -c "
import json, sys
obj = {
  'ts':       '$SPAWN_TIMESTAMP',
  'parent':   '$AGENT_PARENT',
  'child_id': '$AGENT_ID',
  'spec_path':'$SPEC_FILE',
  'role':     '$AGENT_ROLE',
  'scope':    '$AGENT_SCOPE',
}
print(json.dumps(obj))
" >> "$AUDIT_FILE"
ROLLBACK_FILES+=("$AUDIT_FILE")  # not ideal (file may pre-exist), handled in rollback via line-count

# ── step 10: write decision record ───────────────────────────────────────────
python3 ~/.boi/lib/coordination.py lock "$DECISIONS_DIR/spawn-${AGENT_ID}-${TODAY}.md" "hex-agent-spawn" 2>/dev/null || true
DECISION_FILE="$DECISIONS_DIR/spawn-${AGENT_ID}-${TODAY}.md"
cat > "${DECISION_FILE}.tmp" <<DEC
# Spawn Decision: $AGENT_NAME ($AGENT_ID)

**Date:** $TODAY
**Parent agent:** $AGENT_PARENT
**Spawned by:** hex-agent-spawn.sh
**Spec file:** $SPEC_FILE

## Reason

$AGENT_REASON

## Agent summary

- **Role:** $AGENT_ROLE
- **Scope:** $AGENT_SCOPE
- **Budget:** ${BUDGET_WPH} wakes/hr, \$${BUDGET_USD}/day
- **Escalation:** $ESCALATION_CHANNEL

## Status

Agent starts **HALTED**. To activate: \`rm $HALT_FILE\`
DEC
mv "${DECISION_FILE}.tmp" "$DECISION_FILE"
python3 ~/.boi/lib/coordination.py unlock "$DECISION_FILE" "hex-agent-spawn" 2>/dev/null || true
ROLLBACK_FILES+=("$DECISION_FILE")

# ── step 11: validate policy ──────────────────────────────────────────────────
if [[ -f "$HEX_EVENTS_CLI" ]]; then
  if ! python3 "$HEX_EVENTS_CLI" validate "$POLICY_PATH" 2>&1; then
    echo "Policy validation failed — rolling back" >&2
    rollback
    exit 1
  fi
else
  echo "WARNING: hex_events_cli.py not found at $HEX_EVENTS_CLI — skipping policy validation" >&2
fi

# ── step 11b: validate wake script (env.sh sourced, no hardcoded claude path) ─
WAKE_ERRORS=0
if ! grep -q 'source.*env\.sh' "$WAKE_SCRIPT_PATH"; then
  echo "FATAL: $WAKE_SCRIPT_PATH does not source env.sh" >&2
  WAKE_ERRORS=$((WAKE_ERRORS + 1))
fi
if grep -q '/Users/mrap/.local/bin/claude' "$WAKE_SCRIPT_PATH"; then
  echo "FATAL: $WAKE_SCRIPT_PATH hardcodes claude path (must use env.sh function)" >&2
  WAKE_ERRORS=$((WAKE_ERRORS + 1))
fi
if grep -v '^\s*#' "$WAKE_SCRIPT_PATH" | grep -q '\-\-dangerously-skip-permissions'; then
  echo "FATAL: $WAKE_SCRIPT_PATH hardcodes --dangerously-skip-permissions in code (must use env.sh function)" >&2
  WAKE_ERRORS=$((WAKE_ERRORS + 1))
fi
if [[ $WAKE_ERRORS -gt 0 ]]; then
  echo "Wake script validation failed ($WAKE_ERRORS errors) — rolling back" >&2
  rollback
  exit 1
fi

# ── step 12: print activation command ────────────────────────────────────────
echo ""
echo "✓ Agent '$AGENT_ID' spawned successfully."
echo "  State dir:   $STATE_DIR"
echo "  Wake script: $WAKE_SCRIPT_PATH"
echo "  Policy:      $POLICY_PATH"
echo "  HALT file:   $HALT_FILE (agent is HALTED)"
echo ""
echo "To activate + verify first wake: bash /Users/mrap/mrap-hex/.hex/bin/hex-agent-activate.sh $AGENT_ID"
echo "  (or halt-only: rm $HALT_FILE + emit your own attention event)"
echo ""
echo "NOTE: activate-and-verify is preferred — plain 'rm HALT_FILE' is emit-and-forget;"
echo "      if the attention event misses, the agent sits dormant with no wake history."
