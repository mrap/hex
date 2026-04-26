#!/usr/bin/env bash
# goal-alignment.sh — Maps agent activity to Mike's OKRs
#
# Reads OKRs from okrs/personal/2026-Q2.md, reads all agent charters,
# maps agents to OKR themes, assesses coverage, and writes a report to
# projects/fleet-lead/goal-alignment-YYYY-MM-DD.md
#
# Usage: goal-alignment.sh [--dry-run]

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
PROJECTS_DIR="$AGENT_DIR/projects"
OKR_FILE="$AGENT_DIR/okrs/personal/2026-Q2.md"
TODAY="$(date +%Y-%m-%d)"
REPORT_DIR="$PROJECTS_DIR/fleet-lead"
REPORT="$REPORT_DIR/goal-alignment-${TODAY}.md"
REPORT_TMP="${REPORT}.tmp"

DRY_RUN=false
if [[ "${1:-}" == "--dry-run" ]]; then
    DRY_RUN=true
fi

mkdir -p "$REPORT_DIR"

echo "[goal-alignment] Running as of $TODAY"
echo "[goal-alignment] OKR file: $OKR_FILE"
echo "[goal-alignment] Projects dir: $PROJECTS_DIR"

# ─── Agent-to-OKR theme mapping ──────────────────────────────────────────────
# Format: THEME:agent1,agent2,...
declare -A THEME_AGENTS=(
    ["Vitality"]="NONE"
    ["Ventures"]="hex-v2-pm,hex-v2-arch,hex-v2-exp,hex-ops"
    ["Relationships"]="NONE"
    ["Job Search"]="career,scout,prep-coach"
    ["Brand"]="brand"
    ["Wealth"]="investments"
    ["System"]="fleet-lead,cos,hex-autonomy,sentinel,system-arch,dreamer,synthesizer,boi-optimizer"
)

# ─── Python: parse OKRs, read charters, read trails, produce report data ─────
ANALYSIS=$(python3 - "$PROJECTS_DIR" "$OKR_FILE" "$TODAY" << 'PYEOF'
import sys, json, os, re
from datetime import datetime, timezone, timedelta

projects_dir = sys.argv[1]
okr_file     = sys.argv[2]
today_str    = sys.argv[3]

NOW = datetime.now(timezone.utc)
SEVEN_DAYS_AGO = NOW - timedelta(days=7)

# ── Parse OKRs ──────────────────────────────────────────────────────────────
def parse_okrs(path):
    """Parse 2026-Q2.md into themes with KR lists."""
    themes = {}
    current_theme = None
    current_kr = None
    try:
        with open(path) as f:
            for line in f:
                line = line.rstrip()
                m_theme = re.match(r'^## Theme \d+: (.+)', line)
                if m_theme:
                    current_theme = m_theme.group(1).strip()
                    themes[current_theme] = {"objective": "", "krs": []}
                    current_kr = None
                    continue
                if current_theme and re.match(r'^\*\*Objective:\*\* (.+)', line):
                    themes[current_theme]["objective"] = re.sub(r'^\*\*Objective:\*\* ', '', line)
                    continue
                m_kr = re.match(r'^### KR (\d+\.\d+) — (.+)', line)
                if m_kr and current_theme:
                    current_kr = {
                        "id": m_kr.group(1),
                        "name": m_kr.group(2).strip(),
                        "target": "",
                        "progress": "",
                    }
                    themes[current_theme]["krs"].append(current_kr)
                    continue
                if current_kr:
                    if re.match(r'^- Target:', line):
                        current_kr["target"] = re.sub(r'^- Target:\s*', '', line)
                    elif re.match(r'^- Progress:', line):
                        current_kr["progress"] = re.sub(r'^- Progress:\s*', '', line)
    except Exception as e:
        print(f"[WARN] Could not parse OKRs: {e}", file=sys.stderr)
    return themes

okr_themes = parse_okrs(okr_file)

# ── Parse agent charters ──────────────────────────────────────────────────────
def parse_charter(path):
    """Extract id, role, objective, kpis from charter.yaml."""
    data = {"id": "", "role": "", "objective": "", "kpis": []}
    in_kpis = False
    try:
        with open(path) as f:
            for line in f:
                stripped = line.rstrip()
                if re.match(r'^id\s*:', stripped):
                    data["id"] = re.sub(r'^id\s*:\s*', '', stripped).strip()
                elif re.match(r'^role\s*:', stripped):
                    data["role"] = re.sub(r'^role\s*:\s*', '', stripped).strip()
                elif re.match(r'^objective\s*:', stripped):
                    data["objective"] = re.sub(r'^objective\s*:\s*[>|]?\s*', '', stripped).strip()
                elif re.match(r'^kpis\s*:', stripped):
                    in_kpis = True
                    continue
                if in_kpis:
                    if re.match(r'^\s+-\s+', stripped):
                        kpi = re.sub(r'^\s+-\s+"?', '', stripped).rstrip('"')
                        data["kpis"].append(kpi)
                    elif stripped and not stripped.startswith(' ') and not stripped.startswith('\t'):
                        in_kpis = False
    except Exception:
        pass
    return data

# ── Read trail entries from state.json ───────────────────────────────────────
def read_trail(agent_id):
    """Return trail entries from last 7 days."""
    state_path = os.path.join(projects_dir, agent_id, "state.json")
    try:
        with open(state_path) as f:
            state = json.load(f)
        trail = state.get("trail", [])
        recent = []
        for entry in trail:
            ts_str = entry.get("ts", "")
            if not ts_str:
                continue
            try:
                ts = datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
                if ts >= SEVEN_DAYS_AGO:
                    recent.append(entry)
            except Exception:
                pass
        return recent
    except Exception:
        return []

# ── Count dispatches/findings/actions in trail ────────────────────────────────
def count_outputs(trail):
    dispatches = sum(1 for e in trail if e.get("type") == "dispatch")
    findings   = sum(1 for e in trail if e.get("type") == "finding")
    actions    = sum(1 for e in trail if e.get("type") in ("action", "write", "file_write"))
    return {"dispatches": dispatches, "findings": findings, "actions": actions, "total": len(trail)}

# ── Theme-agent mapping (mirrors shell script) ────────────────────────────────
THEME_MAP = {
    "Vitality":       [],
    "Ventures":       ["hex-v2-pm", "hex-v2-arch", "hex-v2-exp", "hex-ops"],
    "Relationships":  [],
    "Job Search":     ["career", "scout", "prep-coach"],
    "Brand":          ["brand"],
    "Wealth":         ["investments"],
    "System":         ["fleet-lead", "cos", "hex-autonomy", "sentinel",
                       "system-arch", "dreamer", "synthesizer", "boi-optimizer"],
}

# Map OKR theme names to THEME_MAP keys (handle name variations)
OKR_TO_THEME = {
    "Vitality":       "Vitality",
    "Ventures":       "Ventures",
    "Relationships":  "Relationships",
    "Job Search":     "Job Search",
    "Brand":          "Brand",
    "Wealth":         "Wealth",
    "System":         "System",
}

# ── Build agent data ──────────────────────────────────────────────────────────
all_agent_ids = set()
for agents in THEME_MAP.values():
    all_agent_ids.update(agents)

agent_data = {}
for agent_id in sorted(all_agent_ids):
    charter_path = os.path.join(projects_dir, agent_id, "charter.yaml")
    charter = parse_charter(charter_path) if os.path.exists(charter_path) else {}
    trail   = read_trail(agent_id)
    outputs = count_outputs(trail)
    agent_data[agent_id] = {
        "charter": charter,
        "trail_7d": len(trail),
        "outputs": outputs,
        "charter_exists": os.path.exists(charter_path),
    }

# ── Build output sections ─────────────────────────────────────────────────────
sections = []
gaps = []
for theme_key, agent_ids in THEME_MAP.items():
    # Find matching OKR theme
    okr_theme = okr_themes.get(theme_key, {})
    objective = okr_theme.get("objective", "N/A")
    krs = okr_theme.get("krs", [])

    # Filter out test/placeholder KRs
    real_krs = [kr for kr in krs if not re.search(r'test-e2e|wave4-container', kr["name"])]

    section = [f"## {theme_key}"]
    section.append(f"**Objective:** {objective}")
    section.append("")

    # OKR progress
    if real_krs:
        section.append("### OKR Progress")
        for kr in real_krs:
            section.append(f"- **KR {kr['id']}** {kr['name']}")
            section.append(f"  - Target: {kr['target']}")
            section.append(f"  - Progress: {kr['progress']}")
    else:
        section.append("### OKR Progress")
        section.append("- No KRs defined for this theme")

    section.append("")

    # Agent coverage
    section.append("### Agent Coverage")
    if not agent_ids:
        section.append("- **GAP: No agents assigned to this theme**")
        gaps.append(theme_key)
    else:
        for agent_id in agent_ids:
            d = agent_data.get(agent_id, {})
            if not d.get("charter_exists"):
                section.append(f"- `{agent_id}` — charter not found (gap)")
                continue
            charter = d.get("charter", {})
            trail_7d = d.get("trail_7d", 0)
            outputs = d.get("outputs", {})
            role = charter.get("role", "")
            active_label = "active" if trail_7d > 0 else "IDLE (0 trail entries in 7d)"
            section.append(f"- `{agent_id}` — {role} — {active_label}")
            section.append(f"  - Trail entries (7d): {trail_7d} | Dispatches: {outputs.get('dispatches',0)} | Findings: {outputs.get('findings',0)} | Actions: {outputs.get('actions',0)}")
            if trail_7d == 0:
                gaps.append(f"{theme_key}:{agent_id} (idle)")

    section.append("")

    # Coverage assessment
    if not agent_ids:
        coverage = "UNCOVERED — no agents assigned"
    else:
        active_agents = [
            aid for aid in agent_ids
            if agent_data.get(aid, {}).get("trail_7d", 0) > 0
        ]
        if len(active_agents) == len(agent_ids):
            coverage = f"FULL — all {len(agent_ids)} agent(s) active in last 7 days"
        elif active_agents:
            coverage = f"PARTIAL — {len(active_agents)}/{len(agent_ids)} agents active in last 7 days"
        else:
            coverage = f"STALE — {len(agent_ids)} agent(s) assigned but none active in last 7 days"

    section.append(f"**Coverage:** {coverage}")
    section.append("")
    sections.append("\n".join(section))

# ── Emit JSON for shell to consume ───────────────────────────────────────────
result = {
    "sections": sections,
    "gaps": gaps,
    "agent_summary": {
        aid: {
            "trail_7d": d["trail_7d"],
            "outputs": d["outputs"],
            "charter_exists": d["charter_exists"],
        }
        for aid, d in agent_data.items()
    },
}
print(json.dumps(result))
PYEOF
)

if [[ $? -ne 0 ]]; then
    echo "[goal-alignment] ERROR: Python analysis failed" >&2
    exit 1
fi

# ─── Parse results ────────────────────────────────────────────────────────────
SECTIONS=$(python3 -c "
import json, sys
d = json.loads(sys.argv[1])
for s in d['sections']:
    print(s)
    print()
" "$ANALYSIS")

GAPS=$(python3 -c "
import json, sys
d = json.loads(sys.argv[1])
gaps = d['gaps']
if gaps:
    print('### Gaps Requiring Attention')
    for g in gaps:
        print(f'- {g}')
else:
    print('### No Coverage Gaps Detected')
" "$ANALYSIS")

AGENT_TABLE=$(python3 -c "
import json, sys
d = json.loads(sys.argv[1])
rows = []
for aid, info in sorted(d['agent_summary'].items()):
    status = 'OK' if info['trail_7d'] > 0 else 'IDLE'
    charter = 'yes' if info['charter_exists'] else 'NO'
    rows.append(f\"| {aid:<22} | {charter:<7} | {info['trail_7d']:>8} | {info['outputs']['dispatches']:>9} | {info['outputs']['findings']:>8} | {info['outputs']['actions']:>7} | {status:<4} |\")
header = '| Agent                  | Charter | Trail 7d | Dispatches | Findings | Actions | Status |'
sep    = '|------------------------|---------|----------|------------|----------|---------|--------|'
print(header)
print(sep)
for r in rows:
    print(r)
" "$ANALYSIS")

# ─── Write report ─────────────────────────────────────────────────────────────
cat > "$REPORT_TMP" << EOF
# Goal Alignment Report — $TODAY

**Generated:** $TODAY
**Horizon:** 2026-Q2 (2026-04-20 → 2026-05-04)
**Purpose:** Map agent fleet activity to Mike's OKRs. Identify coverage gaps. Answer: are we getting scary good at achieving goals?

---

$SECTIONS

---

## Fleet Summary

$AGENT_TABLE

---

## Coverage Gaps

$GAPS

---

## Recommendations

$(python3 -c "
import json, sys
d = json.loads(sys.argv[1])
gaps = d['gaps']
recs = []
if 'Vitality' in gaps:
    recs.append('- **Create a Vitality agent** — No agent is tracking exercise/sleep. This is a core life OKR with zero coverage. Consider a lightweight daily-nudge agent.')
if 'Relationships' in gaps:
    recs.append('- **Create a Relationships agent** — No agent tracks Whitney activities, friend outreach, or gathering planning. Low-weight agent could track and nudge.')
idle_agents = [aid for aid, info in d['agent_summary'].items() if info['trail_7d'] == 0 and info['charter_exists']]
if idle_agents:
    recs.append(f\"- **Investigate idle agents:** {', '.join(idle_agents)} — 0 trail entries in 7 days. Are they wired to hex-events? Are they halted?\")
if not recs:
    recs.append('- No critical gaps detected. System is operating with full coverage.')
print('\n'.join(recs))
" "$ANALYSIS")

EOF

mv "$REPORT_TMP" "$REPORT"
echo "[goal-alignment] Report written: $REPORT"

# ─── Print summary to stdout (consumed by hex-events for Slack post) ──────────
python3 -c "
import json, sys
d = json.loads(sys.argv[1])
total = len(d['agent_summary'])
active = sum(1 for info in d['agent_summary'].values() if info['trail_7d'] > 0)
gaps = d['gaps']
gap_themes = [g for g in gaps if ':' not in g]
idle = [g.split(':')[1] for g in gaps if ':' in g]
print(f'Goal Alignment Report — $TODAY')
print(f'Agents: {active}/{total} active in last 7 days')
if gap_themes:
    print(f'Uncovered OKR themes: {\", \".join(gap_themes)}')
if idle:
    print(f'Idle agents: {\", \".join(idle)}')
print(f'Full report: projects/fleet-lead/goal-alignment-$TODAY.md')
" "$ANALYSIS"
