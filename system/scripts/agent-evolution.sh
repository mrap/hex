#!/usr/bin/env bash
# agent-evolution.sh — Daily agent performance analysis and evolution proposals
#
# For each agent: reads charter KPIs, state.json trail, and cost ledger.
# Calculates performance metrics, identifies top/under/idle performers,
# generates evolution proposals for underperformers, and writes reports.
#
# Usage: agent-evolution.sh [--dry-run]

set -uo pipefail

# ─── Resolve paths ───────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
PROJECTS_DIR="$AGENT_DIR/projects"
LEDGER="$AGENT_DIR/.hex/cost/ledger.jsonl"
EVOLUTION_DIR="$PROJECTS_DIR/fleet-lead/evolution"
BOARD="$PROJECTS_DIR/fleet-lead/board.md"
TODAY="$(date +%Y-%m-%d)"
REPORT="$EVOLUTION_DIR/$TODAY.md"

DRY_RUN=false
if [[ "${1:-}" == "--dry-run" ]]; then
    DRY_RUN=true
fi

mkdir -p "$EVOLUTION_DIR"

# ─── Collect agent list ───────────────────────────────────────────────────────
mapfile -t AGENT_IDS < <(
    for charter in "$PROJECTS_DIR"/*/charter.yaml; do
        [[ -f "$charter" ]] || continue
        agent_id="$(basename "$(dirname "$charter")")"
        echo "$agent_id"
    done | sort
)

if [[ ${#AGENT_IDS[@]} -eq 0 ]]; then
    echo "ERROR: no agent charters found in $PROJECTS_DIR" >&2
    exit 1
fi

echo "Analyzing ${#AGENT_IDS[@]} agents: ${AGENT_IDS[*]}"

# ─── Python analysis helper ──────────────────────────────────────────────────
# Runs all analysis in one Python call to avoid repeated subprocess overhead
ANALYSIS_JSON=$(python3 - "$PROJECTS_DIR" "$LEDGER" "${AGENT_IDS[@]}" << 'PYEOF'
import sys, json, os, re
from datetime import datetime, timezone, timedelta

projects_dir = sys.argv[1]
ledger_path  = sys.argv[2]
agent_ids    = sys.argv[3:]

NOW = datetime.now(timezone.utc)
SEVEN_DAYS_AGO = NOW - timedelta(days=7)
FORTYEIGHT_H   = NOW - timedelta(hours=48)

def load_json(path):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return None

def load_yaml_kpis(charter_path):
    """Extract KPI strings from charter.yaml (simple line parser, no pyyaml dep)."""
    kpis = []
    in_kpis = False
    try:
        with open(charter_path) as f:
            for line in f:
                stripped = line.rstrip()
                if re.match(r'^kpis\s*:', stripped):
                    in_kpis = True
                    continue
                if in_kpis:
                    if re.match(r'^\s+-\s+', stripped):
                        kpi = re.sub(r'^\s+-\s+"?', '', stripped).rstrip('"')
                        kpis.append(kpi)
                    elif stripped and not stripped.startswith(' '):
                        in_kpis = False
    except Exception:
        pass
    return kpis

# Load cost ledger
ledger_by_agent = {}
try:
    with open(ledger_path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            entry = json.loads(line)
            ag = entry.get('agent', '')
            ts_str = entry.get('ts', '')
            try:
                ts = datetime.fromisoformat(ts_str.replace('Z', '+00:00'))
            except Exception:
                continue
            if ts >= SEVEN_DAYS_AGO:
                ledger_by_agent.setdefault(ag, []).append(entry)
except Exception:
    pass

results = {}
for agent_id in agent_ids:
    charter_path = os.path.join(projects_dir, agent_id, 'charter.yaml')
    state_path   = os.path.join(projects_dir, agent_id, 'state.json')

    kpis  = load_yaml_kpis(charter_path)
    state = load_json(state_path) or {}
    trail = state.get('trail', [])
    cost_entries = ledger_by_agent.get(agent_id, [])

    # Filter trail to last 7 days
    recent_trail = []
    for entry in trail:
        ts_str = entry.get('ts', '')
        try:
            ts = datetime.fromisoformat(ts_str.replace('Z', '+00:00'))
            if ts >= SEVEN_DAYS_AGO:
                recent_trail.append(entry)
        except Exception:
            pass

    # Idle check: any trail entry in last 48h?
    last_trail_ts = None
    for entry in trail:
        ts_str = entry.get('ts', '')
        try:
            ts = datetime.fromisoformat(ts_str.replace('Z', '+00:00'))
            if last_trail_ts is None or ts > last_trail_ts:
                last_trail_ts = ts
        except Exception:
            pass
    is_idle = (last_trail_ts is None) or (last_trail_ts < FORTYEIGHT_H)

    # Cost in 7 days
    total_cost_7d = sum(e.get('cost_usd', 0) for e in cost_entries)

    # Action types in recent trail
    action_types = {}
    for entry in recent_trail:
        t = entry.get('type', 'unknown')
        action_types[t] = action_types.get(t, 0) + 1

    # Productive actions: find, act, dispatch, verify
    productive_count = sum(
        action_types.get(t, 0)
        for t in ('find', 'act', 'dispatch', 'verify')
    )

    # Finding-to-action ratio
    finds    = action_types.get('find', 0)
    acts     = action_types.get('act', 0) + action_types.get('dispatch', 0)
    f2a_ratio = (acts / finds) if finds > 0 else 0.0

    # Action diversity: number of distinct action types used
    diversity = len([v for v in action_types.values() if v > 0])

    # Trail quality score: blend of diversity and productive ratio
    total_entries = len(recent_trail)
    productive_ratio = (productive_count / total_entries) if total_entries > 0 else 0.0
    diversity_score  = min(1.0, diversity / 5.0)  # 5 types = full score
    trail_quality    = round((productive_ratio * 0.6 + diversity_score * 0.4), 3)

    # Cost per productive action
    cost_per_action = round(total_cost_7d / productive_count, 4) if productive_count > 0 else None

    # KPI achievement (heuristic: trail entries >= 5 per KPI counts as meeting it)
    kpi_count = len(kpis)
    # Simple: if trail has activity, score proportional to entries vs expected
    # 5 entries per KPI considered baseline "meeting" threshold
    kpi_target_entries = kpi_count * 5
    if kpi_count == 0:
        kpi_achievement = 0.5  # unknown
    elif total_entries >= kpi_target_entries:
        kpi_achievement = 1.0
    else:
        kpi_achievement = round(total_entries / max(kpi_target_entries, 1), 3)

    results[agent_id] = {
        'kpi_count':        kpi_count,
        'kpi_achievement':  kpi_achievement,
        'trail_7d':         len(recent_trail),
        'trail_total':      len(trail),
        'productive_count': productive_count,
        'action_types':     action_types,
        'f2a_ratio':        round(f2a_ratio, 3),
        'diversity':        diversity,
        'trail_quality':    trail_quality,
        'cost_7d_usd':      round(total_cost_7d, 4),
        'cost_per_action':  cost_per_action,
        'is_idle':          is_idle,
        'last_trail_ts':    last_trail_ts.isoformat() if last_trail_ts else None,
        'kpis':             kpis,
        'wake_count':       state.get('wake_count', 0),
    }

print(json.dumps(results))
PYEOF
)

# ─── Parse analysis results and identify performers ──────────────────────────
REPORT_CONTENT=$(python3 - "$TODAY" "$BOARD" <<PYEOF2
import sys, json, os

today = sys.argv[1]
board_path = sys.argv[2]
data = json.loads("""$ANALYSIS_JSON""")

# Identify top/under/idle performers
def composite_score(m):
    """Higher is better."""
    return m['kpi_achievement'] * 0.5 + m['trail_quality'] * 0.3 + (0 if m['cost_per_action'] is None else min(1.0, 1.0 / (m['cost_per_action'] + 0.01)) * 0.2)

scored = {aid: composite_score(m) for aid, m in data.items()}
sorted_agents = sorted(scored.items(), key=lambda x: x[1], reverse=True)

idle_agents  = [a for a, m in data.items() if m['is_idle']]
top_agent    = sorted_agents[0][0] if sorted_agents else None
under_agents = [a for a, _ in sorted_agents[-3:] if a not in idle_agents and data[a]['trail_7d'] > 0]
under_agent  = under_agents[-1] if under_agents else (sorted_agents[-1][0] if sorted_agents else None)

lines = []
lines.append(f"# Agent Evolution Report — {today}")
lines.append("")
lines.append("## Fleet Performance Scorecard")
lines.append("")
lines.append("| Agent | KPI Achievement | Trail (7d) | Trail Quality | Cost/Action | Idle | Score |")
lines.append("|-------|:--------------:|:----------:|:-------------:|:-----------:|:----:|:-----:|")

for agent_id, score in sorted_agents:
    m = data[agent_id]
    idle_marker = "YES" if m['is_idle'] else ""
    cost_str = f"${m['cost_per_action']:.4f}" if m['cost_per_action'] is not None else "—"
    lines.append(
        f"| {agent_id} | {m['kpi_achievement']:.0%} | {m['trail_7d']} | {m['trail_quality']:.2f} | {cost_str} | {idle_marker} | {score:.3f} |"
    )

lines.append("")

# Top performer
if top_agent:
    tm = data[top_agent]
    lines.append(f"## Top Performer: {top_agent}")
    lines.append("")
    lines.append(f"- KPI achievement: {tm['kpi_achievement']:.0%}")
    lines.append(f"- Trail entries (7d): {tm['trail_7d']}")
    lines.append(f"- Trail quality: {tm['trail_quality']:.2f}")
    lines.append(f"- Cost/action: {'$'+str(tm['cost_per_action']) if tm['cost_per_action'] else 'no cost data'}")
    lines.append(f"- Action diversity: {tm['diversity']} types used")
    lines.append(f"- What's working: high activity, broad action coverage")
    lines.append("")

# Idle agents
if idle_agents:
    lines.append("## Idle Agents (no trail in 48h+)")
    lines.append("")
    for a in idle_agents:
        m = data[a]
        last = m['last_trail_ts'] or 'never'
        lines.append(f"- **{a}**: last trail entry={last}, wake_count={m['wake_count']}")
    lines.append("")
    lines.append("**Recommendation:** Investigate queue seeding. Agents wake but produce no trail entries — likely cold-start / empty queue. Run `hex-agent status <agent-id>` and seed with a bootstrap queue item.")
    lines.append("")

# Underperformer evolution proposal
if under_agent and under_agent != top_agent:
    um = data[under_agent]
    lines.append(f"## Evolution Proposal: {under_agent}")
    lines.append("")
    lines.append("### What's Not Working (data-backed)")
    lines.append("")
    if um['trail_7d'] == 0:
        lines.append(f"- Zero trail entries in past 7 days (wake_count={um['wake_count']})")
        lines.append("- Agent wakes but produces no output — empty queue or cold-start failure")
    else:
        lines.append(f"- Low KPI achievement: {um['kpi_achievement']:.0%} (trail_7d={um['trail_7d']}, kpi_count={um['kpi_count']})")
        if um['f2a_ratio'] < 0.3:
            lines.append(f"- Low finding-to-action ratio: {um['f2a_ratio']:.2f} — findings not converting to actions")
        if um['diversity'] < 3:
            lines.append(f"- Low action diversity: only {um['diversity']} action types used — narrow task execution")
    lines.append("")
    lines.append("### Hypothesis for Improvement")
    lines.append("")
    if um['trail_7d'] == 0:
        lines.append("Bootstrapping the agent queue with a first task will unblock all downstream work.")
        proposed_change = "Seed queue with initial responsibility task"
        proposed_field  = "queue.active: add bootstrap item for primary responsibility"
    elif um['f2a_ratio'] < 0.3:
        lines.append("Adding a mandatory 'act on every finding' standing order will increase conversion rate.")
        proposed_change = "Add standing order: every find entry must be followed by an act or dispatch"
        proposed_field  = "wake.responsibilities: add explicit action requirement to each responsibility description"
    else:
        lines.append("Increasing wake frequency will surface more work items and improve KPI coverage.")
        proposed_change = "Increase wake interval from 21600s to 14400s for primary responsibility"
        proposed_field  = "wake.responsibilities[0].interval: 21600 → 14400"
    lines.append("")
    lines.append("### Proposed Charter Change")
    lines.append("")
    lines.append(f"```yaml")
    lines.append(f"# {proposed_field}")
    lines.append(f"# Change: {proposed_change}")
    lines.append(f"```")
    lines.append("")
    lines.append("### Experiment")
    lines.append("")
    lines.append(f"```yaml")
    lines.append(f"evolution:")
    lines.append(f"  baseline_date: {today}")
    lines.append(f"  experiments:")
    lines.append(f"    - id: exp-001")
    lines.append(f"      hypothesis: \"{proposed_change} will improve KPI achievement\"")
    lines.append(f"      change: \"{proposed_field}\"")
    lines.append(f"      started: {today}")
    lines.append(f"      metric: kpi_achievement")
    lines.append(f"      baseline: \"{um['kpi_achievement']:.2f}\"")
    lines.append(f"      result: null")
    lines.append(f"      verdict: null")
    lines.append(f"```")
    lines.append("")
    lines.append("**Duration:** 7 days. Measure before/after KPI achievement and trail quality.")
    lines.append("")

# Board update content
board_section = f"""
## Evolution Scores — {today}

| Agent | Score | KPI% | Trail(7d) | Quality | Idle |
|-------|:-----:|:----:|:---------:|:-------:|:----:|
"""
for agent_id, score in sorted_agents:
    m = data[agent_id]
    idle_marker = "YES" if m['is_idle'] else ""
    board_section += f"| {agent_id} | {score:.3f} | {m['kpi_achievement']:.0%} | {m['trail_7d']} | {m['trail_quality']:.2f} | {idle_marker} |\n"

board_section += f"\n_Updated by agent-evolution.sh on {today}_\n"

# Write/update board.md
try:
    existing = ""
    if os.path.exists(board_path):
        with open(board_path) as f:
            existing = f.read()
    # Remove old evolution section if present
    import re
    existing = re.sub(r'\n## Evolution Scores.*?(?=\n## |\Z)', '', existing, flags=re.DOTALL)
    updated = existing.rstrip() + "\n" + board_section
    print("BOARD_UPDATE:" + board_path)
    with open(board_path, 'w') as f:
        f.write(updated)
except Exception as e:
    print(f"BOARD_ERROR:{e}", file=sys.stderr)

report_text = "\n".join(lines)
print(report_text)
PYEOF2
)

# ─── Write evolution report ───────────────────────────────────────────────────
REPORT_TEXT="${REPORT_CONTENT#*BOARD_UPDATE:*}"
# Actually just filter out the BOARD_UPDATE line from report
REPORT_CLEAN=$(echo "$REPORT_CONTENT" | grep -v '^BOARD_UPDATE:')

if [[ "$DRY_RUN" == "true" ]]; then
    echo "=== DRY RUN: would write to $REPORT ==="
    echo "$REPORT_CLEAN"
else
    REPORT_TMP="$(mktemp)"
    echo "$REPORT_CLEAN" > "$REPORT_TMP"
    mv "$REPORT_TMP" "$REPORT"
    echo "Evolution report written: $REPORT"
fi

echo "Done. Report: $REPORT"
