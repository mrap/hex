#!/usr/bin/env python3
"""Hex Pulse — live system health dashboard server. Port 8896."""

import json
import os
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone, timedelta
from http.server import BaseHTTPRequestHandler, HTTPServer, ThreadingHTTPServer
import queue as _queue
from threading import Lock, Thread

try:
    import anthropic as _anthropic
    _HAS_SDK = True
except ImportError:
    _HAS_SDK = False

PORT = 8896
_THIS = os.path.abspath(__file__)
HEX_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(_THIS))))
VITALS_SCRIPT = os.path.join(HEX_ROOT, ".hex", "scripts", "hex-vitals.py")
FLEET_BIN = os.path.join(HEX_ROOT, ".hex", "bin", "hex-agent")
INITIATIVE_BIN = os.path.join(HEX_ROOT, ".hex", "scripts", "hex-initiative.py")
EXPERIMENT_BIN = os.path.join(HEX_ROOT, ".hex", "scripts", "hex-experiment.py")
AUDIT_DIR = os.path.expanduser("~/.hex/audit")

_fleet_cache: dict = {"data": None, "ts": 0.0}
_fleet_lock = Lock()
_initiatives_cache: dict = {"data": None, "ts": 0.0}
_initiatives_lock = Lock()
_experiments_cache: dict = {"data": None, "ts": 0.0}
_experiments_lock = Lock()
_sse_counter = 0
_sse_clients: list = []
_sse_clients_lock = Lock()

# ── Data collection ───────────────────────────────────────────────────────────

def collect_vitals() -> dict:
    """Run hex-vitals.py and return its JSON output."""
    try:
        r = subprocess.run(
            ["python3", VITALS_SCRIPT],
            capture_output=True, text=True, timeout=15
        )
        if r.stdout.strip():
            return json.loads(r.stdout)
    except Exception as e:
        pass
    return {"_error": "hex-vitals unavailable", "signals": {}}


def _cutoff_24h() -> float:
    return time.time() - 86400


def _read_jsonl(path: str) -> list[dict]:
    """Read a JSONL file, return list of parsed dicts (silently handle missing/errors)."""
    if not os.path.exists(path):
        return []
    rows = []
    try:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if line:
                    try:
                        rows.append(json.loads(line))
                    except json.JSONDecodeError:
                        pass
    except OSError:
        pass
    return rows


def _ts_to_epoch(ts_str: str) -> float:
    """Parse ISO8601 timestamp to epoch float. Returns 0 on failure."""
    if not ts_str:
        return 0.0
    try:
        dt = datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
        return dt.timestamp()
    except Exception:
        return 0.0


def collect_ownership_metrics() -> dict:
    """Read ownership audit JSONL files and compute summary stats."""
    cutoff = _cutoff_24h()
    result: dict = {}

    # Frustration signals: count entries in last 24h
    path = os.path.join(AUDIT_DIR, "frustration-signals.jsonl")
    rows = [r for r in _read_jsonl(path) if _ts_to_epoch(r.get("ts", "")) >= cutoff]
    result["frustration"] = {
        "count": len(rows),
        "sessions": len(set(r.get("session_id", str(i)) for i, r in enumerate(rows))),
        "available": os.path.exists(path),
    }

    # Memory effectiveness: feedback recurrence (no time filter — cumulative)
    path = os.path.join(AUDIT_DIR, "memory-effectiveness.jsonl")
    rows = _read_jsonl(path)
    high = sum(1 for r in rows if r.get("recurrence_rate", 0) > 0.5)
    result["feedback_recurrence"] = {
        "total_feedback": len(rows),
        "high_recurrence_count": high,
        "available": os.path.exists(path),
    }

    # Loop detections: count in last 24h
    path = os.path.join(AUDIT_DIR, "loop-detections.jsonl")
    rows = [r for r in _read_jsonl(path) if _ts_to_epoch(r.get("ts", "")) >= cutoff]
    result["loops"] = {
        "count": len(rows),
        "available": os.path.exists(path),
    }

    # Done-claim verification: verified_rate
    path = os.path.join(AUDIT_DIR, "done-claim-verification.jsonl")
    rows = [r for r in _read_jsonl(path) if _ts_to_epoch(r.get("ts", "")) >= cutoff]
    if rows:
        verified = sum(1 for r in rows if r.get("verified", False))
        rate = verified / len(rows)
    else:
        rate = 1.0  # no data → assume fully verified
    result["done_claims"] = {
        "total": len(rows),
        "verified_rate": round(rate, 4),
        "available": os.path.exists(path),
    }

    # Session anomalies: duplicate session pairs in last 24h
    path = os.path.join(AUDIT_DIR, "session-anomalies.jsonl")
    rows = [r for r in _read_jsonl(path) if _ts_to_epoch(r.get("ts", "")) >= cutoff]
    result["session_anomalies"] = {
        "count": len(rows),
        "available": os.path.exists(path),
    }

    return result


def collect_fleet() -> dict:
    """Run hex-agent fleet and parse table output. Cached for 30s."""
    global _fleet_cache
    with _fleet_lock:
        if time.time() - _fleet_cache["ts"] < 30 and _fleet_cache["data"] is not None:
            return _fleet_cache["data"]

    data: dict = {"agents": [], "total_wakes": 0, "total_cost": 0.0, "active": 0, "_error": None}
    try:
        r = subprocess.run(
            [FLEET_BIN, "fleet"],
            capture_output=True, text=True, timeout=15
        )
        lines = r.stdout.splitlines()
        for line in lines[2:]:  # skip header and separator
            line = line.strip()
            if not line:
                continue
            # Remove ● marker, normalize whitespace
            parts = line.replace("●", "").split()
            # Expect: agent wakes last_wake active blocked $ cost
            if len(parts) < 6:
                continue
            try:
                name = parts[0]
                wakes = int(parts[1])
                active = int(parts[3])
                cost_str = parts[-1]
                cost = float(cost_str)
                data["agents"].append({
                    "name": name, "wakes": wakes, "active": active, "cost": cost
                })
                data["total_wakes"] += wakes
                data["total_cost"] += cost
                data["active"] += active
            except (ValueError, IndexError):
                pass
    except Exception as e:
        data["_error"] = str(e)

    data["total_cost"] = round(data["total_cost"], 4)
    data["agent_count"] = len(data["agents"])

    with _fleet_lock:
        _fleet_cache = {"data": data, "ts": time.time()}
    return data


def collect_initiatives() -> dict:
    """Run hex-initiative.py status --json and return structured data. Cached 60s."""
    global _initiatives_cache
    with _initiatives_lock:
        if time.time() - _initiatives_cache["ts"] < 60 and _initiatives_cache["data"] is not None:
            return _initiatives_cache["data"]

    result: dict = {"initiatives": [], "_error": None}
    try:
        r = subprocess.run(
            ["python3", INITIATIVE_BIN, "status", "--json"],
            capture_output=True, text=True, timeout=10, cwd=HEX_ROOT
        )
        if r.returncode == 0 and r.stdout.strip():
            raw = json.loads(r.stdout)
            initiatives = []
            for d in raw:
                krs = d.get("key_results") or []
                krs_met = sum(1 for kr in krs if kr.get("status") == "met")
                experiment_ids = d.get("experiments") or []
                initiatives.append({
                    "id": d.get("id", ""),
                    "name": d.get("id", ""),
                    "owner": d.get("owner", ""),
                    "status": d.get("status", ""),
                    "horizon": str(d.get("horizon", "") or ""),
                    "krs_met": krs_met,
                    "krs_total": len(krs),
                    "experiment_count": len(experiment_ids),
                    "experiment_ids": experiment_ids,
                })
            result["initiatives"] = initiatives
        else:
            result["_error"] = "unavailable"
    except Exception:
        result["initiatives"] = []
        result["_error"] = "unavailable"

    with _initiatives_lock:
        _initiatives_cache = {"data": result, "ts": time.time()}
    return result


def collect_experiments() -> dict:
    """Run hex-experiment.py list --json and return structured data. Cached 60s."""
    global _experiments_cache
    with _experiments_lock:
        if time.time() - _experiments_cache["ts"] < 60 and _experiments_cache["data"] is not None:
            return _experiments_cache["data"]

    result: dict = {"experiments": [], "_error": None}
    try:
        r = subprocess.run(
            ["python3", EXPERIMENT_BIN, "list", "--json"],
            capture_output=True, text=True, timeout=10, cwd=HEX_ROOT
        )
        if r.returncode == 0 and r.stdout.strip():
            raw = json.loads(r.stdout)
            experiments = []
            for d in raw:
                metrics = d.get("metrics") or {}
                primary = metrics.get("primary") or {}
                primary_name = primary.get("name", "")
                baseline_vals = ((d.get("baseline") or {}).get("values") or {})
                post_vals = ((d.get("post_change") or {}).get("values") or {})
                experiments.append({
                    "id": d.get("id", ""),
                    "title": d.get("title", ""),
                    "status": d.get("state", ""),
                    "owner": d.get("owner", ""),
                    "initiative": d.get("initiative", ""),
                    "primary_metric_baseline": baseline_vals.get(primary_name),
                    "primary_metric_current": post_vals.get(primary_name),
                })
            result["experiments"] = experiments
        else:
            result["_error"] = "unavailable"
    except Exception:
        result["experiments"] = []
        result["_error"] = "unavailable"

    with _experiments_lock:
        _experiments_cache = {"data": result, "ts": time.time()}
    return result


def compute_scores(vitals: dict, ownership: dict, fleet: dict) -> tuple[float, float]:
    """Return (productivity_score 0-100, loop_score 0-100)."""
    signals = vitals.get("signals", {})

    # ── System Productivity ─────────────────────────────────────────
    cr = (signals.get("completion_rate") or {}).get("value")
    tp = (signals.get("task_throughput") or {}).get("value", 0) or 0
    ztf = (signals.get("zero_task_failures") or {}).get("value", 0) or 0

    completion_rate = cr if cr is not None else 0.75  # assume healthy if unknown
    norm_throughput = min(1.0, tp / 50.0)
    norm_failures = min(1.0, ztf / 10.0)

    productivity = min(100.0, (
        completion_rate * 40
        + norm_throughput * 30
        + (1 - norm_failures) * 30
    ))

    # ── Mike-in-the-Loop ────────────────────────────────────────────
    frustration_sessions = ownership.get("frustration", {}).get("sessions", 0)
    recurrence_high = ownership.get("feedback_recurrence", {}).get("high_recurrence_count", 0)
    loop_count = ownership.get("loops", {}).get("count", 0)
    verified_rate = ownership.get("done_claims", {}).get("verified_rate", 1.0)

    loop_score = min(100.0, (
        frustration_sessions * 10
        + recurrence_high * 15
        + loop_count * 20
        + (1 - verified_rate) * 55
    ))

    return round(productivity, 1), round(loop_score, 1)


def get_all_metrics() -> dict:
    """Collect all data and compute composite scores."""
    vitals = collect_vitals()
    ownership = collect_ownership_metrics()
    fleet = collect_fleet()
    initiatives = collect_initiatives()
    experiments = collect_experiments()

    productivity_score, loop_score = compute_scores(vitals, ownership, fleet)

    return {
        "ts": datetime.now(timezone.utc).isoformat(),
        "productivity_score": productivity_score,
        "loop_score": loop_score,
        "productivity": vitals,
        "user_experience": ownership,
        "fleet": fleet,
        "initiatives": initiatives,
        "experiments": experiments,
    }


# ── Message handling ─────────────────────────────────────────────────────────

def _push_response(response: dict):
    """Push a response event to all connected SSE clients."""
    payload = f"event: response\ndata: {json.dumps(response)}\n\n"
    with _sse_clients_lock:
        for q in list(_sse_clients):
            try:
                q.put_nowait(payload)
            except _queue.Full:
                pass


# ── Dashboard Context (Approach E: Dashboard-as-Memory) ───────────────────────

_PULSE_ADDENDUM = (
    '\n\nYou are responding via the Pulse dashboard surface. '
    'Respond ONLY with a JSON object (no prose, no markdown, just raw JSON) with these keys: '
    '{"effect_type": "highlight|annotate|prose|action", '
    '"target": "metric-id or null", '
    '"text": "response text under 80 words", '
    '"action": "command to run or null"}. '
    'Metric IDs: d-cr (completion rate), d-tp (throughput), d-ztf (failures), '
    'd-fr (frustration), d-frc (feedback recurrence), d-lp (loops), d-dc (done-claims), '
    'd-sa (session anomalies).'
)

# Persistent hex session for Pulse surface
_hex_session_id = None


class DashboardContext:
    """Pulse is a surface. Hex is the brain.

    Messages route through claude -p with full CLAUDE.md context,
    same as cc-connect does for Slack. Dashboard state is injected
    as context alongside the user's message.
    """

    MAX_EFFECTS = 5

    def __init__(self):
        self._recent_effects: list[dict] = []

    def handle(self, text: str, dashboard_state: dict) -> dict:
        global _hex_session_id
        state_summary = self._summarize_state(dashboard_state)
        recent = self._format_recent()

        prompt_parts = [
            f"[Pulse dashboard context]\n{state_summary}",
        ]
        if recent:
            prompt_parts.append(f"[Recent interactions]\n{recent}")
        prompt_parts.append(f"[User message]\n{text}")
        prompt_parts.append(_PULSE_ADDENDUM)
        full_prompt = "\n\n".join(prompt_parts)

        response = self._call_hex(full_prompt)
        self._recent_effects.append({
            "query": text,
            "effect_type": response.get("effect_type"),
            "target": response.get("target"),
            "text": response.get("text"),
        })
        if len(self._recent_effects) > self.MAX_EFFECTS:
            self._recent_effects.pop(0)
        return response

    def _summarize_state(self, state: dict) -> str:
        p = state.get("productivity", {})
        sigs = p.get("signals", {})
        ux = state.get("user_experience", {})
        fleet = state.get("fleet", {})
        lines = [
            f"Productivity score: {state.get('productivity_score', '?')}/100",
            f"Loop score: {state.get('loop_score', '?')}/100 (lower=better)",
            f"Completion rate: {sigs.get('completion_rate', {}).get('value', '?')}",
            f"Task throughput: {sigs.get('task_throughput', {}).get('value', '?')}",
            f"Zero-task failures: {sigs.get('zero_task_failures', {}).get('value', '?')}",
            f"Frustration signals: {ux.get('frustration', {}).get('count', '?')}",
            f"Feedback recurrence (high): {ux.get('feedback_recurrence', {}).get('high_recurrence_count', '?')}",
            f"Active loops: {ux.get('loops', {}).get('count', '?')}",
            f"Fleet: {fleet.get('agent_count', '?')} agents, {fleet.get('total_wakes', '?')} wakes, ${fleet.get('total_cost', '?'):.2f}" if isinstance(fleet.get('total_cost'), (int, float)) else f"Fleet: {fleet.get('agent_count', '?')} agents",
        ]
        return "\n".join(lines)

    def _format_recent(self) -> str:
        if not self._recent_effects:
            return ""
        return "\n".join(
            f"  - [{e.get('effect_type','?')}] {e.get('query','')} → {e.get('text','')}"
            for e in self._recent_effects
        )

    def _parse_raw(self, raw: str) -> dict:
        raw = raw.strip()
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            m = re.search(r'\{[^{}]*"effect_type"[^{}]*\}', raw, re.DOTALL)
            if m:
                try:
                    return json.loads(m.group())
                except json.JSONDecodeError:
                    pass
        return {"effect_type": "prose", "target": None, "text": raw[:200], "action": None}

    def _call_hex(self, prompt: str) -> dict:
        global _hex_session_id
        claude_bin = shutil.which("claude") or os.path.expanduser("~/.local/bin/claude")
        if not os.path.exists(claude_bin):
            return {"effect_type": "prose", "text": "hex not available (claude not on PATH).", "target": None, "action": None}

        cmd = [claude_bin, '-p', prompt, '--output-format', 'json', '--dangerously-skip-permissions']
        if _hex_session_id:
            cmd.extend(['--resume', _hex_session_id])

        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=120, cwd=HEX_ROOT)
        except subprocess.TimeoutExpired:
            return {"effect_type": "prose", "text": "Response timed out.", "target": None, "action": None}

        import sys
        if r.returncode != 0 or not r.stdout.strip():
            print(f"[pulse] claude exit={r.returncode} stderr={r.stderr[:300]}", file=sys.stderr, flush=True)
            return {"effect_type": "prose", "text": f"hex error (exit {r.returncode}): {r.stderr[:100]}", "target": None, "action": None}
        try:
            outer = json.loads(r.stdout)
            if not _hex_session_id and outer.get("session_id"):
                _hex_session_id = outer["session_id"]
            inner = outer.get('result') or outer.get('content') or ''
            if isinstance(inner, str):
                return self._parse_raw(inner)
            elif isinstance(inner, dict):
                return inner
        except (json.JSONDecodeError, AttributeError) as e:
            print(f"[pulse] parse error: {e}, stdout={r.stdout[:300]}", file=sys.stderr, flush=True)
        return {"effect_type": "prose", "text": "Could not parse hex response.", "target": None, "action": None}


_ctx = DashboardContext()


def _prewarm_hex():
    """Prime the CLAUDE.md cache on startup so first real message is fast."""
    import sys
    print("[pulse] pre-warming hex session...", file=sys.stderr, flush=True)
    try:
        response = _ctx._call_hex("Respond with exactly: {\"effect_type\":\"prose\",\"text\":\"ready\",\"target\":null,\"action\":null}")
        print(f"[pulse] hex pre-warmed, session={_hex_session_id}, response={response.get('text','?')}", file=sys.stderr, flush=True)
    except Exception as e:
        print(f"[pulse] pre-warm failed: {e}", file=sys.stderr, flush=True)


Thread(target=_prewarm_hex, daemon=True).start()


def _handle_message(text: str):
    """Route message through DashboardContext and push structured response to SSE clients."""
    fallback = {"effect_type": "prose", "text": "Couldn't process that right now.", "target": None}
    try:
        metrics = get_all_metrics()
        response = _ctx.handle(text, metrics)

        # Action whitelist enforcement
        if response.get('effect_type') == 'action' and response.get('action'):
            action = response['action']
            cmd = None
            if action.startswith('hex-agent wake ') and len(action.split()) == 3:
                cmd = [FLEET_BIN, 'wake', action.split()[2]]
            elif action == 'hex-agent fleet':
                cmd = [FLEET_BIN, 'fleet']
            elif action == 'bash .hex/scripts/metrics/run-all.sh':
                cmd = ['bash', os.path.join(HEX_ROOT, '.hex', 'scripts', 'metrics', 'run-all.sh')]

            if cmd:
                try:
                    ar = subprocess.run(cmd, capture_output=True, text=True, timeout=15, cwd=HEX_ROOT)
                    response['action_result'] = (ar.stdout.strip() or ar.stderr.strip())[:200]
                except Exception as e:
                    response['action_result'] = f'Error: {e}'
            else:
                response['action'] = None
                response['text'] = (response.get('text') or '') + ' (action not permitted)'

        _push_response(response)
    except Exception:
        _push_response(fallback)


# ── Dashboard HTML ────────────────────────────────────────────────────────────

DASHBOARD_HTML = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8"/><meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>hex pulse</title>
<style>
*{box-sizing:border-box;margin:0;padding:0}
body{background:#faf8f5;color:#1a1a1a;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;min-height:100vh;display:flex;flex-direction:column}
header{background:#1a1a1a;color:#faf8f5;padding:11px 24px;display:flex;align-items:center;justify-content:space-between}
header h1{font-size:.9rem;font-weight:500;letter-spacing:.07em}
.hdr-r{display:flex;align-items:center;gap:10px;font-size:.75rem;color:#888}
.pdot{width:8px;height:8px;border-radius:50%;background:#555;flex-shrink:0}
@keyframes pf{0%{opacity:1}100%{opacity:.3}}
.pdot.on{background:#2d8a4e;animation:pf .8s ease-out forwards}
.rc{color:#e6a817;display:none}
.rc.vis{display:inline}
main{flex:1;padding:28px 24px 16px;max-width:860px;margin:0 auto;width:100%;display:flex;flex-direction:column;gap:28px}
.heroes{display:grid;grid-template-columns:1fr 1fr;gap:20px}
.hero{text-align:center;padding:28px 12px}
.hlabel{font-size:.7rem;text-transform:uppercase;letter-spacing:.1em;color:#888;margin-bottom:6px}
.hscore{font-size:4.5rem;font-weight:700;line-height:1;color:#ccc;transition:color .4s}
.hq{font-size:.7rem;color:#aaa;margin-top:5px}
.green{color:#2d8a4e}.amber{color:#e6a817}.red{color:#c62828}.muted{color:#bbb}
.signals{display:grid;grid-template-columns:1fr 1fr;gap:20px}
.scol h3{font-size:.7rem;text-transform:uppercase;letter-spacing:.1em;color:#999;margin-bottom:10px}
.srow{display:flex;align-items:center;justify-content:space-between;padding:7px 0;border-bottom:1px solid #ede8e0}
.srow:last-child{border-bottom:none}
.sname{font-size:.82rem;color:#666}
.sright{display:flex;align-items:center;gap:7px}
.dot{width:7px;height:7px;border-radius:50%;background:#ddd;flex-shrink:0}
.sval{font-size:.88rem;font-weight:600;min-width:36px;text-align:right;color:#bbb;transition:color .3s}
footer{background:#1a1a1a;color:#666;padding:9px 24px;font-size:.78rem;display:flex;gap:20px;align-items:center;flex-wrap:wrap}
footer b{color:#faf8f5;font-weight:500}
@media(max-width:560px){.heroes,.signals{grid-template-columns:1fr}.hscore{font-size:3.2rem}}
/* prompt pill */
.pm-pill{position:fixed;bottom:24px;right:24px;width:140px;height:36px;border-radius:18px;background:#1a1a1a;color:#888;font-size:.82rem;display:flex;align-items:center;justify-content:center;cursor:pointer;transition:box-shadow .2s,opacity .25s;z-index:100;user-select:none}
.pm-pill:hover{box-shadow:0 4px 14px rgba(0,0,0,.35)}
/* stark state */
.pm-overlay{position:fixed;inset:0;z-index:99;display:none}
.pm-overlay.active{display:block}
.pm-stark{position:fixed;left:50%;top:40%;transform:translate(-50%,-50%);width:min(680px,calc(100vw - 48px));z-index:101;display:none;flex-direction:column;gap:6px}
.pm-stark.active{display:flex}
.pm-stark input{width:100%;height:48px;border:1px solid #ddd;border-radius:6px;padding:0 16px;font-size:1rem;background:#fff;outline:none;box-shadow:0 2px 20px rgba(0,0,0,.1)}
.pm-hint{font-size:.72rem;color:#aaa;text-align:right}
body.pm-open main,body.pm-open footer{opacity:.4;pointer-events:none;transition:opacity .25s}
@media(max-width:560px){.pm-pill{right:auto;left:50%;transform:translateX(-50%);bottom:20px}.pm-stark{width:calc(100vw - 24px)}.pm-stark input{border-radius:4px}}
/* response overlays */
.hex-prose{position:fixed;left:50%;top:50%;transform:translate(-50%,-50%);max-width:480px;width:calc(100vw - 48px);background:#fff;box-shadow:0 4px 24px rgba(0,0,0,.15);padding:16px 20px;border-radius:8px;font-size:.9rem;line-height:1.5;z-index:200;opacity:0;transition:opacity .3s;pointer-events:none}
.hex-prose.vis{opacity:1;pointer-events:auto}
.hx-close{float:right;cursor:pointer;color:#aaa;margin-left:8px;font-size:1rem}
.hex-toast{position:fixed;top:20px;right:20px;background:#2d8a4e;color:#fff;padding:10px 16px;border-radius:6px;font-size:.85rem;z-index:300;opacity:0;transition:opacity .3s;pointer-events:none;max-width:320px;white-space:pre-wrap}
.hex-toast.vis{opacity:1}
.hex-annotate{font-size:.72rem;color:#888;margin-top:2px;display:block}
@keyframes hexGlow{0%,66%{box-shadow:none}33%,100%{box-shadow:0 0 12px rgba(45,138,78,.4)}}
.hex-hl{animation:hexGlow 2s ease forwards}
@media(max-width:560px){.hex-prose{left:12px;right:12px;width:auto;max-width:none;transform:none;top:auto;bottom:80px}.hex-toast{left:12px;right:12px;max-width:none}}
.init-row{display:flex;align-items:center;gap:8px;padding:7px 0;border-bottom:1px solid #ede8e0}.init-row:last-child{border-bottom:none}.init-name{font-size:.82rem;flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.init-owner{font-size:.75rem;color:#aaa;min-width:80px}.init-dots{letter-spacing:2px;min-width:64px;font-size:.75rem}.init-frac{font-size:.75rem;color:#888;min-width:40px;text-align:right}.init-exp{font-size:.72rem;color:#888;min-width:80px;text-align:right}
.exp-toggle{font-size:.8rem;color:#888;padding:8px 0;cursor:pointer;user-select:none}.exp-toggle:hover{color:#444}.exp-list{display:none}.exp-list.open{display:block}
.exp-row{display:flex;align-items:center;gap:8px;padding:6px 0;border-bottom:1px solid #ede8e0;font-size:.8rem}.exp-row:last-child{border-bottom:none}.exp-id{color:#aaa;min-width:56px;font-size:.72rem}.exp-title{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:#555}.exp-st{min-width:80px;font-size:.7rem;font-weight:600;text-align:right}.exp-met{min-width:80px;font-size:.72rem;color:#888;text-align:right}
@media(max-width:560px){.init-owner,.init-dots{display:none}}
</style>
</head>
<body>
<header>
  <h1>hex pulse</h1>
  <div class="hdr-r">
    <span class="rc" id="rc">reconnecting…</span>
    <span id="age">—</span>
    <div class="pdot" id="pdot"></div>
  </div>
</header>

<main>
  <section class="heroes">
    <div class="hero">
      <div class="hlabel">System Productivity</div>
      <div class="hscore muted" id="productivity_score">—</div>
      <div class="hq">≥80 healthy</div>
    </div>
    <div class="hero">
      <div class="hlabel">Mike in the Loop</div>
      <div class="hscore muted" id="loop_score">—</div>
      <div class="hq">≤20 healthy · lower is better</div>
    </div>
  </section>

  <section class="signals">
    <div class="scol">
      <h3>Productivity</h3>
      <div class="srow"><span class="sname">Completion rate</span>
        <div class="sright"><div class="dot" id="d-cr"></div><span class="sval" id="v-cr">—</span></div></div>
      <div class="srow"><span class="sname">Task throughput (24h)</span>
        <div class="sright"><div class="dot" id="d-tp"></div><span class="sval" id="v-tp">—</span></div></div>
      <div class="srow"><span class="sname">Zero-task failures</span>
        <div class="sright"><div class="dot" id="d-ztf"></div><span class="sval" id="v-ztf">—</span></div></div>
    </div>
    <div class="scol">
      <h3>Experience</h3>
      <div class="srow"><span class="sname">Frustration signals</span>
        <div class="sright"><div class="dot" id="d-fr"></div><span class="sval" id="v-fr">—</span></div></div>
      <div class="srow"><span class="sname">Feedback recurrence</span>
        <div class="sright"><div class="dot" id="d-frc"></div><span class="sval" id="v-frc">—</span></div></div>
      <div class="srow"><span class="sname">Active loops</span>
        <div class="sright"><div class="dot" id="d-lp"></div><span class="sval" id="v-lp">—</span></div></div>
      <div class="srow"><span class="sname">Unverified done-claims</span>
        <div class="sright"><div class="dot" id="d-dc"></div><span class="sval" id="v-dc">—</span></div></div>
      <div class="srow"><span class="sname">Duplicate sessions</span>
        <div class="sright"><div class="dot" id="d-sa"></div><span class="sval" id="v-sa">—</span></div></div>
    </div>
  </section>

  <section id="init-section">
    <div style="font-size:.7rem;text-transform:uppercase;letter-spacing:.1em;color:#999;margin-bottom:10px">Initiatives</div>
    <div id="init-rows">—</div>
    <div class="exp-toggle" id="exp-toggle">▸ Experiments (— total)</div>
    <div class="exp-list" id="exp-list"></div>
  </section>
</main>

<footer>
  Agents: <b id="f-agents">—</b>
  &nbsp;·&nbsp; Wakes/24h: <b id="f-wakes">—</b>
  &nbsp;·&nbsp; Cost/24h: $<b id="f-cost">—</b>
</footer>

<div class="pm-pill" id="pm-pill">Ask hex…</div>
<div class="pm-overlay" id="pm-overlay"></div>
<div class="pm-stark" id="pm-stark">
  <input type="text" id="pm-input" maxlength="500" placeholder="Ask hex anything…" autocomplete="off"/>
  <div class="pm-hint" id="pm-hint">Enter to send · Esc to close</div>
</div>

<script>
var lastTs = null;
var C = {green:'#2d8a4e', amber:'#e6a817', red:'#c62828', muted:'#bbb'};

function col(k){ return C[k]||'#1a1a1a'; }

function setVal(id, v, k){
  var el=document.getElementById(id); if(!el) return;
  el.textContent = (v===null||v===undefined)?'—':v;
  el.style.color = (v===null||v===undefined)?C.muted:col(k||'');
}
function setDot(id, k){
  var el=document.getElementById(id); if(el) el.style.background=col(k);
}
function ps(s){ return s>=80?'green':s>=60?'amber':'red'; }
function ls(s){ return s<=20?'green':s<=50?'amber':'red'; }

var _expOpen=false,_exps=[];
document.getElementById('exp-toggle').addEventListener('click',function(){_expOpen=!_expOpen;document.getElementById('exp-list').classList.toggle('open',_expOpen);renderExpHdr(_exps);});
var ESC={ACTIVE:'#2d8a4e',MEASURING:'#e6a817',VERDICT_PASS:'#2d8a4e',VERDICT_FAIL:'#c62828',DRAFT:'#aaa',BASELINE:'#4a90d9'};
function renderExpHdr(list){var a=(list||[]).filter(function(e){return e.status==='ACTIVE';}).length,dr=(list||[]).filter(function(e){return e.status==='DRAFT';}).length,ar=_expOpen?'▾':'▸';document.getElementById('exp-toggle').textContent=ar+' Experiments ('+(list||[]).length+' total: '+a+' active, '+dr+' draft)';}
function renderExps(list){var h='';(list||[]).forEach(function(e){var c=ESC[e.status]||'#aaa',t=(e.title||e.id||'').substring(0,45),m=[e.primary_metric_baseline!=null?'baseline: '+e.primary_metric_baseline:'',e.primary_metric_current!=null?'current: '+e.primary_metric_current:''].filter(Boolean).join(' · ');h+='<div class="exp-row"><span class="exp-id">'+(e.id||'')+'</span><span class="exp-title" title="'+(e.title||'').replace(/"/g,'&#34;')+'">'+ t+'</span><span class="exp-st" style="color:'+c+'">'+(e.status||'')+'</span><span class="exp-met">'+m+'</span></div>';});document.getElementById('exp-list').innerHTML=h||'<div style="padding:8px 0;font-size:.8rem;color:#aaa">No experiments</div>';}
function iScore(i){var n=Date.now(),h=i.horizon?new Date(i.horizon).getTime():null,p=i.krs_total>0?i.krs_met/i.krs_total:0;if(h&&(h-n)<14*86400*1000&&p<0.5)return 0;if(i._ae>0)return 1;if(p>0.5)return 2;return 3;}
var ICL=['#c62828','#e6a817','#2d8a4e','#bbb'];
function renderInits(data,exps){var list=(data||{}).initiatives||[],eb={};(exps||[]).forEach(function(e){if(e.initiative){if(!eb[e.initiative])eb[e.initiative]={a:0,t:0};eb[e.initiative].t++;if(e.status==='ACTIVE')eb[e.initiative].a++;}});list=list.map(function(i){var ec=eb[i.id]||{a:0,t:0};return Object.assign({},i,{_ae:ec.a,_et:ec.t});});list.sort(function(a,b){return iScore(a)-iScore(b);});var h='';list.slice(0,10).forEach(function(i){var c=ICL[iScore(i)],d='';for(var k=0;k<i.krs_total;k++)d+=k<i.krs_met?'●':'○';h+='<div class="init-row"><span class="init-name" style="color:'+c+'" title="'+(i.name||i.id||'').replace(/"/g,'&#34;')+'">'+(i.name||i.id||'').substring(0,35)+'</span><span class="init-owner">'+(i.owner||'')+'</span><span class="init-dots">'+d+'</span><span class="init-frac">'+i.krs_met+'/'+i.krs_total+' KRs</span><span class="init-exp">'+(i._et>0?(i._ae+' active'):'no exp')+'</span></div>';});if(list.length>10)h+='<div style="font-size:.75rem;color:#aaa;padding:6px 0">+ '+(list.length-10)+' more</div>';document.getElementById('init-rows').innerHTML=h||'<div style="font-size:.8rem;color:#aaa;padding:8px 0">No initiatives</div>';}

function update(d){
  lastTs=new Date();
  // Hero scores
  var p=d.productivity_score, l=d.loop_score;
  var pEl=document.getElementById('productivity_score');
  pEl.textContent=(p!=null)?Math.round(p):'—';
  pEl.style.color=(p!=null)?col(ps(p)):C.muted;
  var lEl=document.getElementById('loop_score');
  lEl.textContent=(l!=null)?Math.round(l):'—';
  lEl.style.color=(l!=null)?col(ls(l)):C.muted;

  // Productivity sub-signals
  var sig=((d.productivity||{}).signals)||{};
  var cr=(sig.completion_rate||{}).value;
  if(cr!=null){var ck=cr>=.8?'green':cr>=.6?'amber':'red';setVal('v-cr',Math.round(cr*100)+'%',ck);setDot('d-cr',ck);}
  var tp=(sig.task_throughput||{}).value;
  if(tp!=null){var tk=tp>=20?'green':tp>=5?'amber':'red';setVal('v-tp',tp,tk);setDot('d-tp',tk);}
  var ztf=(sig.zero_task_failures||{}).value;
  if(ztf!=null){var zk=ztf===0?'green':ztf<=2?'amber':'red';setVal('v-ztf',ztf,zk);setDot('d-ztf',zk);}

  // Experience sub-signals
  var ue=d.user_experience||{};
  var fr=(ue.frustration||{}).count;
  if(fr!=null){var fk=fr===0?'green':fr<=3?'amber':'red';setVal('v-fr',fr,fk);setDot('d-fr',fk);}
  var frc=(ue.feedback_recurrence||{}).high_recurrence_count;
  if(frc!=null){var rk=frc===0?'green':frc<=2?'amber':'red';setVal('v-frc',frc,rk);setDot('d-frc',rk);}
  var lp=(ue.loops||{}).count;
  if(lp!=null){var lk=lp===0?'green':lp<=3?'amber':'red';setVal('v-lp',lp,lk);setDot('d-lp',lk);}
  var dc=ue.done_claims||{};
  if(dc.total!=null){var u=dc.total?Math.round((1-dc.verified_rate)*dc.total):0;var dk=u===0?'green':u<=2?'amber':'red';setVal('v-dc',u,dk);setDot('d-dc',dk);}
  var sa=(ue.session_anomalies||{}).count;
  if(sa!=null){var sk=sa===0?'green':sa<=2?'amber':'red';setVal('v-sa',sa,sk);setDot('d-sa',sk);}

  // Fleet
  var fl=d.fleet||{};
  document.getElementById('f-agents').textContent=fl.agent_count!=null?fl.agent_count:'—';
  document.getElementById('f-wakes').textContent=fl.total_wakes!=null?fl.total_wakes:'—';
  document.getElementById('f-cost').textContent=fl.total_cost!=null?fl.total_cost.toFixed(2):'—';

  // Initiatives & experiments
  _exps=(d.experiments||{}).experiments||[];renderInits(d.initiatives,_exps);renderExpHdr(_exps);renderExps(_exps);

  // Pulse dot blink
  var dot=document.getElementById('pdot');
  dot.classList.remove('on'); void dot.offsetWidth; dot.classList.add('on');
}

var base=location.pathname.replace(/[/]$/,'');
var es=new EventSource(base+'/api/stream');
es.onmessage=function(e){
  try{update(JSON.parse(e.data));}catch(_){}
  document.getElementById('rc').classList.remove('vis');
};
es.onerror=function(){ document.getElementById('rc').classList.add('vis'); };

setInterval(function(){
  if(!lastTs) return;
  var s=Math.round((Date.now()-lastTs)/1000);
  document.getElementById('age').textContent='Updated '+s+'s ago';
},1000);

// Prompt stark/parked state machine (CP-11)
var pmOpen=false;
function openStark(){
  if(pmOpen)return;
  pmOpen=true;
  document.body.classList.add('pm-open');
  var pill=document.getElementById('pm-pill');
  pill.style.opacity='0';pill.style.pointerEvents='none';
  document.getElementById('pm-overlay').classList.add('active');
  document.getElementById('pm-stark').classList.add('active');
  document.getElementById('pm-input').focus();
}
function closeStark(){
  if(!pmOpen)return;
  pmOpen=false;
  document.body.classList.remove('pm-open');
  var pill=document.getElementById('pm-pill');
  pill.style.opacity='';pill.style.pointerEvents='';
  document.getElementById('pm-overlay').classList.remove('active');
  document.getElementById('pm-stark').classList.remove('active');
  document.getElementById('pm-input').value='';
  document.getElementById('pm-input').blur();
  document.getElementById('pm-hint').textContent='Enter to send · Esc to close';
}
function sendMessage(){
  var txt=document.getElementById('pm-input').value.trim();
  if(!txt)return;
  closeStark();
  fetch(base+'/api/message',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({text:txt})}).catch(function(){});
}
document.getElementById('pm-pill').addEventListener('click',openStark);
document.getElementById('pm-overlay').addEventListener('click',closeStark);
document.getElementById('pm-input').addEventListener('keydown',function(e){
  if(e.key==='Enter'){sendMessage();}
  if(e.key==='Escape'){closeStark();}
});
document.getElementById('pm-input').addEventListener('input',function(){
  var n=this.value.length;
  document.getElementById('pm-hint').textContent=n?((500-n)+' chars · Enter to send · Esc to close'):'Enter to send · Esc to close';
});
document.addEventListener('keydown',function(e){
  if(!pmOpen&&e.key==='/'&&document.activeElement.tagName!=='INPUT'){e.preventDefault();openStark();}
  if(pmOpen&&e.key==='Escape'){closeStark();}
});
// Response effect renderer (CP-12/13/14, CP-17)
var MNAMES=['Completion rate','Task throughput','Zero-task failures','Frustration signals','Feedback recurrence','Active loops','Unverified done-claims','Duplicate sessions'];
function boldN(t){var s=t.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');MNAMES.forEach(function(n){s=s.replace(new RegExp('('+n+')','gi'),'<b>$1</b>');});return s;}
function mkEl(id,cls){var e=document.getElementById(id);if(!e){e=document.createElement('div');e.id=id;document.body.appendChild(e);}e.className=cls;return e;}
function showProse(txt){var e=mkEl('hx-prose','hex-prose');e.innerHTML=boldN(txt||'');e._p=false;clearTimeout(e._t);e.classList.add('vis');e._t=setTimeout(function(){if(!e._p)e.classList.remove('vis');},6000);e.onclick=function(){if(!e._p){e._p=true;clearTimeout(e._t);var x=document.createElement('span');x.className='hx-close';x.innerHTML='&times;';x.onclick=function(ev){ev.stopPropagation();e.classList.remove('vis');e._p=false;};e.appendChild(x);}};}
function getRow(id){var el=document.getElementById(id);return el?el.closest('.srow'):null;}
function showHL(tid,txt){var row=getRow(tid);if(!row){if(txt)showProse(txt);return;}row.classList.remove('hex-hl');void row.offsetWidth;row.classList.add('hex-hl');setTimeout(function(){row.classList.remove('hex-hl');},2100);if(txt)showAnn(tid,txt);}
function showAnn(tid,txt){var old=document.getElementById('ann-'+tid);if(old)old.remove();var row=getRow(tid);if(!row)return;var a=document.createElement('span');a.className='hex-annotate';a.id='ann-'+tid;a.textContent=txt||'';row.appendChild(a);}
function showToast(txt,res){var e=mkEl('hx-toast','hex-toast');clearTimeout(e._t);e.textContent='✓ '+(txt||'')+(res?' '+res:'');e.classList.add('vis');e._t=setTimeout(function(){e.classList.remove('vis');},4000);}
es.addEventListener('response',function(e){try{var d=JSON.parse(e.data),f=d.effect_type;if(f==='prose')showProse(d.text);else if(f==='highlight')showHL(d.target,d.text);else if(f==='annotate')showAnn(d.target,d.text);else if(f==='action')showToast(d.text,d.action_result);else showProse(d.text);}catch(_){}});
var _ou=update;update=function(d){_ou(d);document.querySelectorAll('.hex-annotate').forEach(function(a){a.remove();});};
</script>
</body>
</html>"""


# ── HTTP Handler ──────────────────────────────────────────────────────────────

class PulseHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass  # silence access log

    def do_POST(self):
        path = self.path.split("?")[0]
        if path == "/api/message":
            self._message_endpoint()
        else:
            self._send(404, "text/plain", b"Not found")

    def _message_endpoint(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)
        try:
            data = json.loads(body)
            text = (data.get("text") or "").strip()
        except (json.JSONDecodeError, AttributeError):
            self._send(400, "application/json", b'{"error":"Invalid JSON"}')
            return
        if not text or len(text) > 500:
            self._send(400, "application/json", b'{"error":"Text must be 1-500 chars"}')
            return
        Thread(target=_handle_message, args=(text,), daemon=True).start()
        self._send(202, "application/json", b'{"status":"processing"}')

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/":
            self._send(200, "text/html; charset=utf-8", DASHBOARD_HTML.encode())
        elif path == "/api/vitals":
            data = get_all_metrics()
            body = json.dumps(data, indent=2).encode()
            self._send(200, "application/json", body)
        elif path == "/api/context":
            body = json.dumps({"recent_effects": _ctx._recent_effects, "count": len(_ctx._recent_effects)}).encode()
            self._send(200, "application/json", body)
        elif path == "/api/stream":
            self._serve_sse()
        else:
            self._send(404, "text/plain", b"Not found")

    def _send(self, code: int, ctype: str, body: bytes):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _serve_sse(self):
        global _sse_counter
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        q: _queue.Queue = _queue.Queue(maxsize=20)
        with _sse_clients_lock:
            _sse_clients.append(q)
        try:
            while True:
                try:
                    event = q.get(timeout=5)
                    self.wfile.write(event.encode())
                    self.wfile.flush()
                except _queue.Empty:
                    _sse_counter += 1
                    data = get_all_metrics()
                    payload = f"id: {_sse_counter}\ndata: {json.dumps(data)}\n\n"
                    self.wfile.write(payload.encode())
                    self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            with _sse_clients_lock:
                _sse_clients.remove(q)


# ── Entry point ───────────────────────────────────────────────────────────────

if __name__ == "__main__":
    if "--test" in sys.argv:
        data = get_all_metrics()
        print(json.dumps(data))
        sys.exit(0)

    print(f"hex pulse listening on http://127.0.0.1:{PORT}", flush=True)
    ThreadingHTTPServer.allow_reuse_address = True
    server = ThreadingHTTPServer(("127.0.0.1", PORT), PulseHandler)
    server.daemon_threads = True
    server.serve_forever()
