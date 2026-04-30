#!/usr/bin/env python3
"""hex pulse dashboard — at-a-glance view of what's being driven."""

import http.server
import json
import os
import re
import subprocess
import urllib.request
from pathlib import Path

PORT = 8896
HEX_ROOT = Path(os.path.expanduser("${HEX_DIR}"))
BOI_DB = Path(os.path.expanduser("~/.boi/boi.db"))
AUDIT_LOG = HEX_ROOT / ".hex" / "audit" / "actions.jsonl"
TELEMETRY_DIR = HEX_ROOT / ".hex" / "telemetry"


def _emit(event_type: str, payload: dict) -> None:
    try:
        import sys as _sys
        _sys.path.insert(0, str(TELEMETRY_DIR))
        from emit import emit
        emit(event_type, payload, source="pulse-server")
    except Exception as exc:
        import sys as _sys
        print(f"[pulse-server] telemetry emit failed: {exc}", file=_sys.stderr)

HEX_UI_URL = "http://localhost:8889"
HEX_UI_TOKEN = ""
try:
    env_path = os.path.expanduser("~/github.com/mrap/hex-ui/frontend/.env.local")
    with open(env_path) as f:
        for line in f:
            if "BEARER" in line and "=" in line:
                HEX_UI_TOKEN = line.split("=", 1)[1].strip()
except Exception:
    pass

HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>hex pulse</title>
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
    background: #faf8f5; color: #1a1a1a;
  }
  .header {
    background: #1a1a1a; color: #faf8f5;
    padding: 14px 24px;
    display: flex; align-items: center; justify-content: space-between;
    position: sticky; top: 0; z-index: 100;
  }
  .header h1 { font-size: 16px; font-weight: 600; letter-spacing: 0.5px; }
  .header-right { display: flex; gap: 12px; align-items: center; }
  .header .ts { font-size: 11px; color: #888; }
  .refresh {
    background: none; border: 1px solid #555; color: #ccc;
    padding: 4px 12px; border-radius: 4px; cursor: pointer; font-size: 11px;
  }
  .refresh:hover { border-color: #aaa; color: #fff; }

  .summary-bar {
    display: flex; gap: 0; border-bottom: 1px solid #e0dcd6;
    background: #fff;
  }
  .summary-stat {
    flex: 1; padding: 14px 20px; text-align: center;
    border-right: 1px solid #e0dcd6;
  }
  .summary-stat:last-child { border-right: none; }
  .summary-num { font-size: 28px; font-weight: 700; line-height: 1; }
  .summary-label { font-size: 10px; text-transform: uppercase; letter-spacing: 0.8px; color: #888; margin-top: 4px; }
  .num-active { color: #2d8a4e; }
  .num-notstarted { color: #888; }
  .num-blocked { color: #c62828; }
  .num-done { color: #1a1a1a; }
  .num-ratio-ok { color: #2d8a4e; }
  .num-ratio-alert { color: #c62828; }

  .columns {
    display: grid;
    grid-template-columns: 1fr 1fr 1fr;
    gap: 0;
    min-height: calc(100vh - 120px);
  }
  @media (max-width: 900px) { .columns { grid-template-columns: 1fr; } }

  .column {
    border-right: 1px solid #e0dcd6;
    padding: 16px 20px;
  }
  .column:last-child { border-right: none; }
  .col-header {
    font-size: 11px; font-weight: 700;
    text-transform: uppercase; letter-spacing: 1px;
    padding-bottom: 12px; margin-bottom: 4px;
    border-bottom: 2px solid;
  }
  .col-active .col-header { color: #2d8a4e; border-color: #2d8a4e; }
  .col-notstarted .col-header { color: #888; border-color: #ccc; }
  .col-blocked .col-header { color: #c62828; border-color: #c62828; }

  .stream {
    padding: 12px 0;
    border-bottom: 1px solid #f0ece5;
  }
  .stream:last-child { border-bottom: none; }
  .stream-name {
    font-size: 13px; font-weight: 600; line-height: 1.3;
  }
  .stream-owner {
    font-size: 10px; color: #888; margin-top: 2px;
    text-transform: uppercase; letter-spacing: 0.5px;
  }
  .stream-detail {
    font-size: 12px; color: #555; margin-top: 6px; line-height: 1.5;
  }
  .stream-specs {
    margin-top: 6px;
  }
  .spec-pill {
    display: inline-block;
    font-size: 10px; padding: 2px 8px; border-radius: 10px;
    margin: 2px 4px 2px 0; font-weight: 500;
  }
  .pill-running { background: #e8f5e9; color: #2d8a4e; }
  .pill-completed { background: #e8f5e9; color: #1b5e20; }
  .pill-failed { background: #fce4ec; color: #c62828; }
  .pill-pending { background: #f0ece5; color: #888; }

  .blocker-reason {
    font-size: 11px; color: #c62828; margin-top: 4px;
    font-weight: 500;
  }

  .kr-row {
    display: flex; justify-content: space-between; align-items: center;
    font-size: 11px; color: #888; margin-top: 4px;
  }
  .kr-bar {
    flex: 1; height: 3px; background: #e0dcd6; border-radius: 2px;
    margin-left: 8px; overflow: hidden;
  }
  .kr-fill { height: 100%; background: #2d8a4e; border-radius: 2px; }

  .empty-col { color: #ccc; font-size: 12px; padding: 30px 0; text-align: center; }

  .section-divider {
    font-size: 10px; color: #aaa; text-transform: uppercase; letter-spacing: 1px;
    margin-top: 16px; margin-bottom: 8px; padding-top: 8px;
    border-top: 1px dashed #e0dcd6;
  }

  .init-tag {
    display: inline-block; font-size: 9px; padding: 1px 6px;
    border-radius: 3px; background: #e3f2fd; color: #1565c0;
    font-weight: 600; letter-spacing: 0.3px; margin-top: 3px;
    text-transform: uppercase;
  }
  .owner-tag {
    display: inline-block; font-size: 9px; padding: 1px 6px;
    border-radius: 3px; background: #f0ece5; color: #888;
    margin-left: 4px; text-transform: uppercase;
  }
  .severity-red {
    border-left: 3px solid #c62828;
    padding-left: 10px;
  }
  .severity-yellow {
    border-left: 3px solid #f9a825;
    padding-left: 10px;
  }
</style>
</head>
<body>
<div class="header">
  <h1>hex pulse</h1>
  <div class="header-right">
    <span class="ts" id="timestamp"></span>
    <button class="refresh" onclick="load()">Refresh</button>
  </div>
</div>
<div class="summary-bar" id="summary">
  <div class="summary-stat"><div class="summary-num">-</div><div class="summary-label">Loading</div></div>
</div>
<div class="columns" id="content">
  <div class="column"><div class="empty-col">Loading...</div></div>
  <div class="column"></div>
  <div class="column"></div>
</div>

<div class="chat-panel" id="chatPanel">
  <div class="chat-toggle" onclick="toggleChat()">
    <span id="chatToggleLabel">Chat</span>
  </div>
  <div class="chat-body" id="chatBody">
    <div class="chat-messages" id="chatMessages">
      <div class="empty-col">No messages yet</div>
    </div>
    <div class="chat-input-row">
      <input type="text" id="captureInput" placeholder="Tell hex anything..." autocomplete="off" />
      <button id="captureBtn" onclick="sendCapture()">Send</button>
    </div>
  </div>
</div>

<style>
  .chat-panel {
    position: fixed; bottom: 0; right: 20px;
    width: 400px; max-width: calc(100vw - 40px);
    background: #fff; border: 1px solid #e0dcd6;
    border-bottom: none; border-radius: 12px 12px 0 0;
    box-shadow: 0 -4px 20px rgba(0,0,0,0.08);
    z-index: 200;
    display: flex; flex-direction: column;
  }
  .chat-toggle {
    padding: 10px 16px;
    font-size: 13px; font-weight: 600;
    cursor: pointer; user-select: none;
    border-bottom: 1px solid #e0dcd6;
    display: flex; justify-content: space-between; align-items: center;
  }
  .chat-body {
    display: flex; flex-direction: column;
    height: 350px;
    transition: height 0.2s ease;
  }
  .chat-body.collapsed { height: 0; overflow: hidden; }

  .chat-messages {
    flex: 1; overflow-y: auto;
    padding: 12px 16px;
    display: flex; flex-direction: column; gap: 8px;
  }

  .chat-msg {
    padding: 8px 12px; border-radius: 8px;
    font-size: 13px; line-height: 1.5;
    max-width: 85%; word-wrap: break-word;
    animation: fadeIn 0.2s ease;
  }
  @keyframes fadeIn { from { opacity: 0; transform: translateY(4px); } to { opacity: 1; transform: translateY(0); } }
  .chat-msg.user {
    background: #1a1a1a; color: #faf8f5;
    align-self: flex-end; border-bottom-right-radius: 2px;
  }
  .chat-msg.hex {
    background: #f0ece5; color: #1a1a1a;
    align-self: flex-start; border-bottom-left-radius: 2px;
  }
  .chat-msg.status {
    background: none; color: #888;
    font-size: 11px; align-self: center;
    font-style: italic;
  }
  .msg-time {
    font-size: 10px; color: #aaa; margin-top: 2px;
  }
  .chat-msg.user .msg-time { color: #888; }

  .chat-input-row {
    display: flex; padding: 10px 12px;
    gap: 8px; border-top: 1px solid #e0dcd6;
  }
  .chat-input-row input {
    flex: 1; padding: 8px 12px; border: 1px solid #e0dcd6;
    border-radius: 8px; font-size: 13px; font-family: inherit;
    background: #faf8f5; outline: none;
  }
  .chat-input-row input:focus { border-color: #1a1a1a; }
  .chat-input-row button {
    padding: 8px 16px; background: #1a1a1a; color: #faf8f5;
    border: none; border-radius: 8px; font-size: 12px; font-weight: 600;
    cursor: pointer;
  }
  .chat-input-row button:hover { background: #333; }
  .chat-input-row button:disabled { background: #ccc; cursor: default; }
</style>
<script>
function renderStream(s) {
  const severityCls = (s.blocker || '').includes('RED') ? 'severity-red' :
                      (s.blocker || '').includes('YELLOW') ? 'severity-yellow' : '';
  let html = `<div class="stream ${severityCls}">`;
  html += `<div class="stream-name">${esc(s.name)}</div>`;
  if (s.initiative) html += `<span class="init-tag">${esc(s.initiative)}</span>`;
  if (s.owner) html += `<span class="owner-tag">${esc(s.owner)}</span>`;
  if (s.detail) html += `<div class="stream-detail">${esc(s.detail)}</div>`;
  if (s.blocker) {
    const sev = (s.blocker || '').replace(/→.*/, '').trim();
    html += `<div class="blocker-reason">${esc(sev)}</div>`;
  }
  if (s.specs && s.specs.length) {
    html += '<div class="stream-specs">';
    s.specs.forEach(sp => {
      const cls = sp.status === 'running' ? 'pill-running' : sp.status === 'completed' ? 'pill-completed' : sp.status === 'failed' ? 'pill-failed' : 'pill-pending';
      html += `<span class="spec-pill ${cls}">${esc(sp.id)} ${sp.progress || ''} ${sp.status}</span>`;
    });
    html += '</div>';
  }
  if (s.krs_total > 0) {
    const pct = s.krs_total > 0 ? Math.round(100 * (s.krs_met || 0) / s.krs_total) : 0;
    html += `<div class="kr-row"><span>${s.krs_met || 0}/${s.krs_total} KRs</span><div class="kr-bar"><div class="kr-fill" style="width:${pct}%"></div></div></div>`;
  }
  html += '</div>';
  return html;
}

function esc(s) {
  if (!s) return '';
  const d = document.createElement('div');
  d.textContent = s;
  return d.innerHTML;
}

function renderColumn(streams, emptyMsg) {
  if (!streams || !streams.length) return `<div class="empty-col">${emptyMsg || 'None'}</div>`;
  return streams.map(renderStream).join('');
}

async function load() {
  try {
    const [pulseResp, ratioResp] = await Promise.all([
      fetch('./api/pulse'),
      fetch('./api/telemetry-ratio'),
    ]);
    const data = await pulseResp.json();
    let ratioData = null;
    try { ratioData = await ratioResp.json(); } catch(e) {}

    document.getElementById('timestamp').textContent = new Date().toLocaleTimeString();

    const active = data.active || [];
    const notStarted = data.not_started || [];
    const blocked = (data.blocked_on_mike || []).sort((a, b) => {
      const sev = s => (s.blocker||'').includes('RED') ? 0 : (s.blocker||'').includes('YELLOW') ? 1 : 2;
      return sev(a) - sev(b);
    });
    const recentDone = data.recently_done || [];

    let ratioHtml = '';
    if (ratioData && ratioData.overall) {
      const pct = ratioData.overall.ratio_pct;
      const isAlert = ratioData.overall.status === 'ALERT';
      const cls = isAlert ? 'num-ratio-alert' : 'num-ratio-ok';
      ratioHtml = `<div class="summary-stat"><div class="summary-num ${cls}">${pct}%</div><div class="summary-label">Acted On</div></div>`;
    }

    document.getElementById('summary').innerHTML = ratioHtml + `
      <div class="summary-stat"><div class="summary-num num-active">${active.length}</div><div class="summary-label">Active</div></div>
      <div class="summary-stat"><div class="summary-num num-notstarted">${notStarted.length}</div><div class="summary-label">Not Started</div></div>
      <div class="summary-stat"><div class="summary-num num-blocked">${blocked.length}</div><div class="summary-label">Blocked on Mike</div></div>
      <div class="summary-stat"><div class="summary-num num-done">${recentDone.length}</div><div class="summary-label">Done (24h)</div></div>
      <div class="summary-stat"><div class="summary-num">${data.boi_workers_busy || 0}/${data.boi_workers_total || 5}</div><div class="summary-label">BOI Workers</div></div>
      <div class="summary-stat"><div class="summary-num">${data.agents_productive || 0}/${data.agents_total || 0}</div><div class="summary-label">Agents Active</div></div>
    `;

    let activeHtml = renderColumn(active, 'Nothing actively running');
    if (recentDone.length) {
      activeHtml += `<div class="section-divider">Completed (last 24h)</div>`;
      activeHtml += renderColumn(recentDone, '');
    }

    document.getElementById('content').innerHTML = `
      <div class="column col-active"><div class="col-header">Active</div>${activeHtml}</div>
      <div class="column col-notstarted"><div class="col-header">Not Started</div>${renderColumn(notStarted, 'Everything has been started')}</div>
      <div class="column col-blocked"><div class="col-header">Blocked on Mike</div>${renderColumn(blocked, 'Nothing blocked')}</div>
    `;
  } catch(e) {
    document.getElementById('content').innerHTML = `<div class="column" style="grid-column:1/-1"><div class="empty-col">Error: ${e.message}</div></div>`;
  }
}

load();
setInterval(load, 15000);

// --- Chat System ---
const MIKE_USER_ID = 'U0AQACA26NS';
let chatOpen = true;
let lastMsgCount = 0;

captureInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); sendCapture(); }
});

function toggleChat() {
  chatOpen = !chatOpen;
  document.getElementById('chatBody').classList.toggle('collapsed', !chatOpen);
}

async function sendCapture() {
  const text = captureInput.value.trim();
  if (!text) return;
  captureInput.value = '';
  captureBtn.disabled = true;

  // Optimistic: show user message immediately
  appendMsg('user', text);
  appendMsg('status', 'Sending to hex...');

  try {
    const resp = await fetch('./api/capture', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text }),
    });
    const data = await resp.json();
    // Remove "sending" status, show routing
    removeLastStatus();
    appendMsg('status', 'Routed to #hex-pulse — waiting for hex to respond...');
  } catch (e) {
    removeLastStatus();
    appendMsg('status', 'Error: ' + e.message);
  }
  captureBtn.disabled = false;
}

function appendMsg(type, text) {
  const el = document.getElementById('chatMessages');
  const empty = el.querySelector('.empty-col');
  if (empty) empty.remove();

  const div = document.createElement('div');
  div.className = 'chat-msg ' + type;
  div.textContent = text;
  el.appendChild(div);
  el.scrollTop = el.scrollHeight;
}

function removeLastStatus() {
  const el = document.getElementById('chatMessages');
  const statuses = el.querySelectorAll('.chat-msg.status');
  if (statuses.length) statuses[statuses.length - 1].remove();
}

async function loadMessages() {
  try {
    const resp = await fetch('./api/messages');
    const msgs = await resp.json();
    if (msgs.length === lastMsgCount) return;
    lastMsgCount = msgs.length;

    const el = document.getElementById('chatMessages');
    el.innerHTML = '';
    msgs.forEach(m => {
      const div = document.createElement('div');
      if (m.user === MIKE_USER_ID) {
        div.className = 'chat-msg user';
      } else if (m.bot) {
        div.className = 'chat-msg hex';
      } else {
        div.className = 'chat-msg hex';
      }
      div.textContent = m.text;
      el.appendChild(div);
    });
    el.scrollTop = el.scrollHeight;
  } catch(e) {}
}

loadMessages();
setInterval(loadMessages, 3000);
</script>
</body>
</html>"""


def get_pulse_data():
    import yaml
    import sqlite3
    from datetime import datetime, timedelta

    result = {
        "active": [],
        "not_started": [],
        "blocked_on_mike": [],
        "recently_done": [],
        "boi_workers_busy": 0,
        "boi_workers_total": 5,
        "agents_productive": 0,
        "agents_total": 0,
    }

    # --- Read initiatives ---
    init_dir = HEX_ROOT / "initiatives"
    initiatives = {}
    if init_dir.exists():
        for f in sorted(init_dir.glob("*.yaml")):
            try:
                data = yaml.safe_load(f.read_text())
                iid = data.get("id", f.stem)
                krs = data.get("key_results", [])
                met = sum(1 for kr in krs if kr.get("status") == "met")
                initiatives[iid] = {
                    "name": data.get("goal", f.stem)[:100],
                    "owner": data.get("owner", ""),
                    "horizon": str(data.get("horizon", "")),
                    "krs_total": len(krs),
                    "krs_met": met,
                    "specs": [],
                    "detail": "",
                    "blocker": "",
                }
            except Exception:
                continue

    # --- Read BOI queue for active/recent specs ---
    boi_specs = []
    recent = []
    if BOI_DB.exists():
        try:
            db = sqlite3.connect(str(BOI_DB))
            db.row_factory = sqlite3.Row
            cutoff = (datetime.now(tz=None) - timedelta(hours=24)).isoformat()

            for row in db.execute(
                "SELECT spec_id, title, status, tasks_done, tasks_total "
                "FROM specs WHERE status IN ('running', 'dispatched') "
                "ORDER BY submitted_at DESC LIMIT 20"
            ):
                boi_specs.append(dict(row))
            result["boi_workers_busy"] = len([s for s in boi_specs if s["status"] == "running"])

            for row in db.execute(
                "SELECT spec_id, title, status, tasks_done, tasks_total, completed_at "
                "FROM specs WHERE status IN ('completed', 'failed') AND completed_at > ? "
                "ORDER BY completed_at DESC LIMIT 20", (cutoff,)
            ):
                recent.append(dict(row))

            db.close()
        except Exception:
            pass
    else:
        try:
            out = subprocess.check_output(
                ["bash", os.path.expanduser("~/.boi/boi"), "status"],
                timeout=10, stderr=subprocess.STDOUT
            ).decode()
            for line in out.splitlines():
                if "busy" in line:
                    m = re.search(r"(\d+)/(\d+)\s*busy", line)
                    if m:
                        result["boi_workers_busy"] = int(m.group(1))
                        result["boi_workers_total"] = int(m.group(2))
        except Exception:
            pass

    # --- Read agent audit log ---
    agent_stats = {}
    if AUDIT_LOG.exists():
        try:
            cutoff_dt = datetime.now(tz=None) - timedelta(hours=24)
            for line in AUDIT_LOG.read_text().splitlines():
                try:
                    d = json.loads(line)
                    agent = d.get("agent", "")
                    action = d.get("action", "")
                    ts = d.get("ts", "")
                    if agent not in agent_stats:
                        agent_stats[agent] = {"last_action": "", "last_ts": "", "productive": False}
                    agent_stats[agent]["last_action"] = action
                    agent_stats[agent]["last_ts"] = ts
                    if action not in ("wake-start", "wake-skip", "wake-end"):
                        agent_stats[agent]["productive"] = True
                except Exception:
                    continue
        except Exception:
            pass

    result["agents_total"] = len(agent_stats)
    result["agents_productive"] = sum(1 for a in agent_stats.values() if a["productive"])

    # --- Read CoS board for blocker data ---
    board_path = HEX_ROOT / "projects" / "cos" / "board.md"
    blockers_raw = []
    if board_path.exists():
        try:
            board_text = board_path.read_text()
            in_blockers = False
            for line in board_text.splitlines():
                if "Active / Escalated" in line or "Blockers" in line:
                    in_blockers = True
                    continue
                if in_blockers and line.startswith("### "):
                    in_blockers = False
                if in_blockers and "|" in line and "Mike" in line:
                    parts = [p.strip() for p in line.split("|") if p.strip()]
                    if len(parts) >= 3 and "RESOLVED" not in parts[1].upper():
                        blockers_raw.append({
                            "workstream": parts[0],
                            "blocker": re.sub(r"\*\*", "", parts[1])[:200],
                            "severity": parts[-1] if len(parts) >= 4 else "",
                        })
        except Exception:
            pass

    # --- Read active workstreams from board ---
    workstreams = {}
    if board_path.exists():
        try:
            board_text = board_path.read_text()
            in_ws = False
            for line in board_text.splitlines():
                if "## Active Workstreams" in line:
                    in_ws = True
                    continue
                if in_ws and line.startswith("## "):
                    break
                if in_ws and "|" in line and not line.startswith("|---") and "Workstream" not in line:
                    parts = [p.strip() for p in line.split("|") if p.strip()]
                    if len(parts) >= 2:
                        ws_name = parts[0]
                        ws_status = re.sub(r"\*\*", "", parts[1])[:200]
                        ws_touch = parts[2] if len(parts) >= 3 else ""
                        workstreams[ws_name] = {
                            "status_text": ws_status,
                            "last_touch": ws_touch,
                        }
        except Exception:
            pass

    # --- Categorize streams ---

    # --- Blocked on Mike: read from COS board dynamically ---
    mike_blocked = []
    if board_path.exists():
        try:
            board_text = board_path.read_text()
            in_escalated = False
            for line in board_text.splitlines():
                if "Active / Escalated" in line or "Escalation" in line:
                    in_escalated = True
                    continue
                if in_escalated and line.startswith("## "):
                    in_escalated = False
                if in_escalated and "|" in line and "Mike" in line:
                    parts = [p.strip() for p in line.split("|") if p.strip()]
                    if len(parts) >= 3 and "RESOLVED" not in parts[-1].upper():
                        ws_name = re.sub(r"\*\*", "", parts[0])
                        blocker = re.sub(r"\*\*", "", parts[1])[:200]
                        severity = parts[-1] if len(parts) >= 4 else ""
                        # Find linked initiative
                        initiative = ""
                        ws_key = ws_name.lower().replace(" ", "-").replace("_", "-")
                        for iid in initiatives:
                            if ws_key in iid or iid.replace("init-", "") in ws_key:
                                initiative = iid
                                break
                        mike_blocked.append({
                            "name": ws_name,
                            "owner": parts[2].strip() if len(parts) >= 3 else "",
                            "detail": blocker,
                            "blocker": severity,
                            "initiative": initiative,
                        })
        except Exception:
            pass

    # Also check todo.md for items explicitly waiting on Mike
    todo_path = HEX_ROOT / "todo.md"
    if todo_path.exists():
        try:
            todo_text = todo_path.read_text()
            in_now = False
            for line in todo_text.splitlines():
                if line.strip() == "## Now":
                    in_now = True
                    continue
                if in_now and line.startswith("## "):
                    break
                if in_now and line.startswith("- [ ]") and any(
                    kw in line.lower() for kw in ["waiting", "mike", "review", "approval", "manual", "get ", "respond"]
                ):
                    title = re.sub(r"^- \[ \] \*\*", "", line).split("**")[0].strip()
                    if title and not any(b["name"] == title for b in mike_blocked):
                        mike_blocked.append({
                            "name": title[:80],
                            "owner": "",
                            "detail": line[6:120].strip(),
                            "blocker": "From todo.md",
                            "initiative": "",
                        })
        except Exception:
            pass

    # --- Active: running BOI specs + initiatives with active experiments ---
    active_streams = []
    for spec in boi_specs:
        # Link to initiative if title matches
        initiative = ""
        title_lower = spec.get("title", "").lower()
        for iid in initiatives:
            if iid.replace("init-", "").replace("-", " ")[:15] in title_lower:
                initiative = iid
                break
        active_streams.append({
            "name": spec.get("title", spec.get("spec_id", "unknown")),
            "owner": "",
            "initiative": initiative,
            "detail": f"BOI spec {spec.get('spec_id', '')}",
            "specs": [{
                "id": spec.get("spec_id", ""),
                "status": spec.get("status", ""),
                "progress": f"{spec.get('tasks_done', 0)}/{spec.get('tasks_total', '?')}",
            }],
        })

    # Add initiatives that have recent agent activity
    for iid, init in initiatives.items():
        if not any(s.get("initiative") == iid for s in active_streams):
            # Check if any agent with this initiative had recent productive wakes
            owner = init.get("owner", "")
            if owner in agent_stats and agent_stats[owner].get("productive"):
                active_streams.append({
                    "name": init["name"],
                    "owner": owner,
                    "initiative": iid,
                    "krs_total": init["krs_total"],
                    "krs_met": init["krs_met"],
                    "detail": f"Horizon: {init['horizon']}",
                })

    # --- Not started: initiatives with no progress AND no active work ---
    not_started = []
    active_init_ids = {s.get("initiative", "") for s in active_streams if s.get("initiative")}
    for iid, init in initiatives.items():
        if iid not in active_init_ids and init["krs_met"] == 0:
            not_started.append({
                **init,
                "initiative": iid,
                "detail": f"Owner: {init['owner']} — Horizon: {init['horizon']}",
            })

    # --- Recently done ---
    done_streams = []
    for spec in recent:
        done_streams.append({
            "name": spec.get("title", spec.get("spec_id", "unknown")),
            "owner": "",
            "detail": f"{spec.get('tasks_done', 0)}/{spec.get('tasks_total', '?')} tasks",
            "specs": [{
                "id": spec.get("spec_id", ""),
                "status": spec.get("status", ""),
                "progress": f"{spec.get('tasks_done', 0)}/{spec.get('tasks_total', '?')}",
            }],
        })

    result["active"] = active_streams
    result["not_started"] = not_started
    result["blocked_on_mike"] = mike_blocked
    result["recently_done"] = done_streams

    return result


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/" or self.path == "/index.html":
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(HTML.encode())

        elif self.path == "/api/pulse":
            self.send_response(200)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.end_headers()
            data = get_pulse_data()
            self.wfile.write(json.dumps(data).encode())

        elif self.path == "/api/telemetry-ratio":
            self.send_response(200)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.end_headers()
            ratio_script = str(HEX_ROOT / ".hex" / "scripts" / "telemetry-ratio.py")
            try:
                result = subprocess.run(
                    ["python3", ratio_script, "--json"],
                    capture_output=True, text=True, timeout=10,
                )
                payload = json.loads(result.stdout) if result.stdout.strip() else {}
            except Exception as exc:
                payload = {"error": str(exc)}
            self.wfile.write(json.dumps(payload).encode())

        elif self.path == "/api/messages":
            self.send_response(200)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.end_headers()

            slack_token = os.environ.get("HEX_SLACK_BOT_TOKEN", "")
            messages = []
            if slack_token:
                try:
                    req = urllib.request.Request(
                        "https://slack.com/api/conversations.history?channel=C0AUYHWBBFU&limit=30",
                        headers={"Authorization": f"Bearer {slack_token}"},
                    )
                    with urllib.request.urlopen(req, timeout=5) as resp:
                        data = json.loads(resp.read())
                    if data.get("ok"):
                        for msg in reversed(data.get("messages", [])):
                            messages.append({
                                "ts": msg.get("ts", ""),
                                "text": msg.get("text", ""),
                                "user": msg.get("user", ""),
                                "bot": "bot_id" in msg or "app_id" in msg,
                            })
                except Exception:
                    pass

            self.wfile.write(json.dumps(messages).encode())

        elif self.path.startswith("/api/message-status"):
            self.send_response(200)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.end_headers()
            msg_id = self.path.split("id=")[-1] if "id=" in self.path else ""
            try:
                req = urllib.request.Request(
                    f"{HEX_UI_URL}/api/messages?limit=5",
                    headers={"Authorization": f"Bearer {HEX_UI_TOKEN}"},
                )
                with urllib.request.urlopen(req, timeout=5) as resp:
                    messages = json.loads(resp.read())
                found = None
                for m in messages:
                    if m.get("message_id") == msg_id:
                        found = m
                        break
                if found:
                    self.wfile.write(json.dumps({
                        "status": found.get("status", "processing"),
                        "response_preview": (found.get("response", "") or "")[:200],
                        "actions": found.get("actions", []),
                    }).encode())
                else:
                    self.wfile.write(json.dumps({"status": "processing", "preview": "Working..."}).encode())
            except Exception as e:
                self.wfile.write(json.dumps({"status": "processing", "preview": str(e)[:100]}).encode())

        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        if self.path == "/api/capture":
            content_len = int(self.headers.get("Content-Length", 0))
            body = json.loads(self.rfile.read(content_len)) if content_len else {}
            text = body.get("text", "")

            self.send_response(200)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.end_headers()

            import uuid
            from datetime import datetime

            message_id = str(uuid.uuid4())
            now = datetime.now()
            timestamp = now.isoformat()

            # 1. Write to captures (guaranteed persistence)
            ts = now.strftime("%Y-%m-%d_%H-%M-%S")
            capture_dir = HEX_ROOT / "raw" / "captures"
            capture_dir.mkdir(parents=True, exist_ok=True)
            capture_file = capture_dir / f"{ts}.md"
            capture_file.write_text(
                f"---\ncaptured: {timestamp}\nsource: pulse\ntriaged: false\nmessage_id: {message_id}\n---\n\n{text}\n"
            )

            _emit("pulse.message.received", {
                "message_id": message_id,
                "text": text[:500],
                "ts": timestamp,
                "capture_file": str(capture_file),
            })

            # 2. Post to #hex-pulse via Slack — cc-connect routes this to a
            #    Claude Code session that can actually act on it
            slack_token = os.environ.get("HEX_SLACK_BOT_TOKEN", "")
            slack_ts = None
            if slack_token:
                try:
                    slack_data = json.dumps({
                        "channel": "C0AUYHWBBFU",
                        "text": text,
                    }).encode()
                    slack_req = urllib.request.Request(
                        "https://slack.com/api/chat.postMessage",
                        data=slack_data,
                        headers={
                            "Authorization": f"Bearer {slack_token}",
                            "Content-Type": "application/json; charset=utf-8",
                        },
                        method="POST",
                    )
                    with urllib.request.urlopen(slack_req, timeout=5) as resp:
                        sr = json.loads(resp.read())
                        slack_ts = sr.get("ts")
                    if slack_ts:
                        _emit("pulse.message.routed", {
                            "message_id": message_id,
                            "channel": "C0AUYHWBBFU",
                            "slack_ts": slack_ts,
                        })
                except Exception:
                    pass

            self.wfile.write(json.dumps({
                "captured": True,
                "message_id": message_id,
                "file": str(capture_file),
                "slack_ts": slack_ts,
                "channel": "C0AUYHWBBFU",
            }).encode())
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, format, *args):
        pass


if __name__ == "__main__":
    _server_py = Path(__file__)
    _code_mtime = _server_py.stat().st_mtime if _server_py.exists() else 0
    _emit("pulse.server.started", {
        "pid": os.getpid(),
        "code_mtime": _code_mtime,
        "port": PORT,
    })
    server = http.server.HTTPServer(("0.0.0.0", PORT), Handler)
    print(f"hex pulse running at http://localhost:{PORT}")
    server.serve_forever()
