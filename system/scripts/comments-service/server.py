#!/usr/bin/env python3
"""hex comments — universal commenting service for all hex web assets.
Port 8901. Storage: .hex/data/comments.json
Any hex page can embed the widget via <script src="/comments/widget.js"></script>

Comments are keyed by asset (type + id). Status tracks what the agent did with the feedback.
"""

import http.server
import json
import os
import socketserver
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

PORT = 8901
HEX_ROOT = Path(os.path.expanduser("~/mrap-hex"))
COMMENTS_FILE = HEX_ROOT / ".hex/data/comments.json"

# ---------------------------------------------------------------------------
# Data layer
# ---------------------------------------------------------------------------

def _load() -> dict:
    if COMMENTS_FILE.exists():
        try:
            return json.loads(COMMENTS_FILE.read_text())
        except (json.JSONDecodeError, OSError):
            pass
    return {"comments": []}


def _save(data: dict):
    COMMENTS_FILE.parent.mkdir(parents=True, exist_ok=True)
    COMMENTS_FILE.write_text(json.dumps(data, indent=2))


def _emit_event(comment: dict):
    """Emit hex-event for LLM-based agent routing."""
    try:
        import subprocess
        payload = json.dumps({
            "comment_id": comment["id"],
            "asset": comment["asset"],
            "text": comment["text"][:200],
            "author": comment.get("author", "mike"),
            "source": "comments-service",
        })
        subprocess.Popen(
            ["python3", os.path.expanduser("~/.hex-events/hex_emit.py"),
             "hex.comment.created", payload],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Widget JS — embeddable on any hex page
# ---------------------------------------------------------------------------

WIDGET_JS = r"""
(function() {
  const COMMENTS_BASE = '/comments';
  let panel = null;
  let isOpen = false;
  let currentAsset = null;

  function getAsset() {
    const el = document.querySelector('[data-comment-asset]');
    if (el) return el.getAttribute('data-comment-asset');
    const path = window.location.pathname.replace(/\/$/, '');
    const parts = path.split('/').filter(Boolean);
    if (parts.length >= 2) return parts.slice(-2).join(':');
    return parts[parts.length - 1] || 'page:home';
  }

  function getAssetLabel() {
    const el = document.querySelector('[data-comment-label]');
    if (el) return el.getAttribute('data-comment-label');
    return currentAsset;
  }

  const statusColors = {
    'new': '#c4553a',
    'seen': '#b85c14',
    'acting': '#8b6f47',
    'done': '#2d7a3a',
    'dismissed': '#a09a90',
  };
  const statusLabels = {
    'new': 'New',
    'seen': 'Seen',
    'acting': 'Working on it',
    'done': 'Done',
    'dismissed': 'Dismissed',
  };

  function esc(s) {
    const d = document.createElement('div');
    d.textContent = s;
    return d.innerHTML;
  }

  function relTime(ts) {
    const ms = Date.now() - new Date(ts).getTime();
    if (isNaN(ms)) return '';
    const mins = Math.floor(ms / 60000);
    if (mins < 1) return 'just now';
    if (mins < 60) return mins + 'm ago';
    const hrs = Math.floor(mins / 60);
    if (hrs < 24) return hrs + 'h ago';
    return Math.floor(hrs / 24) + 'd ago';
  }

  function createPanel() {
    const div = document.createElement('div');
    div.id = 'hex-comments-panel';
    div.innerHTML = `
      <style>
        #hex-comments-panel {
          position: fixed; right: 20px; bottom: 80px; width: 360px; max-height: 70vh;
          background: #faf7f0; border: 1px solid #e8e0d0; border-radius: 12px;
          box-shadow: 0 8px 30px rgba(0,0,0,0.12); z-index: 10000;
          display: none; flex-direction: column; font-family: 'Work Sans', -apple-system, sans-serif;
          overflow: hidden;
        }
        #hex-comments-panel.open { display: flex; }
        .hc-header {
          padding: 14px 18px; border-bottom: 1px solid #e8e0d0;
          display: flex; align-items: center; justify-content: space-between;
        }
        .hc-header h3 {
          font-family: 'Fraunces', serif; font-size: 1rem; font-weight: 700; margin: 0;
        }
        .hc-header .hc-close {
          background: none; border: none; font-size: 1.2rem; cursor: pointer;
          color: #8a8880; padding: 4px;
        }
        .hc-header .hc-close:hover { color: #c4553a; }
        .hc-asset-label {
          padding: 6px 18px; font-size: 0.72rem; color: #8a8880;
          background: #f5f0e8; border-bottom: 1px solid #e8e0d0;
        }
        .hc-list {
          flex: 1; overflow-y: auto; padding: 12px 18px;
          max-height: calc(70vh - 160px);
        }
        .hc-empty {
          color: #a09a90; font-size: 0.85rem; font-style: italic; padding: 20px 0; text-align: center;
        }
        .hc-comment {
          padding: 10px 0; border-bottom: 1px solid #f0ebe3;
        }
        .hc-comment:last-child { border-bottom: none; }
        .hc-comment-text { font-size: 0.88rem; line-height: 1.45; color: #1c1a16; }
        .hc-comment-meta {
          display: flex; align-items: center; gap: 8px; margin-top: 4px;
        }
        .hc-comment-time { font-size: 0.72rem; color: #a09a90; }
        .hc-comment-status {
          font-size: 0.65rem; font-weight: 600; padding: 1px 6px;
          border-radius: 4px; background: #f0ebe3;
        }
        .hc-action-log {
          margin-top: 4px; font-size: 0.72rem; color: #6e695f;
          padding-left: 10px; border-left: 2px solid #e8e0d0;
        }
        .hc-acting-pulse {
          display: inline-block; width: 6px; height: 6px; border-radius: 50%;
          background: #8b6f47; margin-right: 5px; vertical-align: middle;
          animation: hcPulse 1.4s ease-in-out infinite;
        }
        @keyframes hcPulse {
          0%, 100% { opacity: 0.4; transform: scale(0.8); }
          50% { opacity: 1; transform: scale(1.2); }
        }
        .hc-asset-tag {
          display: inline-block; font-size: 0.62rem; padding: 1px 5px;
          background: #eee8dc; border-radius: 3px; color: #6e695f;
          margin-left: 4px; font-weight: 500;
        }
        .hc-input-wrap {
          padding: 12px 18px; border-top: 1px solid #e8e0d0;
          display: flex; gap: 8px;
        }
        .hc-input {
          flex: 1; padding: 8px 12px; border: 1px solid #e8e0d0; border-radius: 8px;
          font-family: inherit; font-size: 0.85rem; background: #fff;
          outline: none; resize: none;
        }
        .hc-input:focus { border-color: #c4553a; }
        .hc-send {
          padding: 8px 14px; background: #1c1a16; color: #faf7f0;
          border: none; border-radius: 8px; font-family: inherit;
          font-size: 0.82rem; font-weight: 500; cursor: pointer;
        }
        .hc-send:hover { background: #c4553a; }
      </style>
      <div class="hc-header">
        <h3>Comments</h3>
        <button class="hc-close" onclick="window._hexComments.toggle()">&times;</button>
      </div>
      <div class="hc-asset-label" id="hc-asset-label"></div>
      <div class="hc-list" id="hc-list"></div>
      <div class="hc-input-wrap">
        <textarea class="hc-input" id="hc-input" rows="2" placeholder="Leave a comment..."></textarea>
        <button class="hc-send" onclick="window._hexComments.send()">Send</button>
      </div>
    `;
    document.body.appendChild(div);
    document.getElementById('hc-input').addEventListener('keydown', function(e) {
      if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        window._hexComments.send();
      }
    });
    return div;
  }

  function createFab() {
    const btn = document.createElement('button');
    btn.id = 'hex-comments-fab';
    btn.innerHTML = '💬';
    btn.title = 'Comments';
    Object.assign(btn.style, {
      position: 'fixed', right: '20px', bottom: '20px', width: '48px', height: '48px',
      borderRadius: '50%', border: '1px solid #e8e0d0', background: '#fff',
      boxShadow: '0 2px 12px rgba(0,0,0,0.1)', cursor: 'pointer', fontSize: '1.3rem',
      zIndex: '10001', display: 'flex', alignItems: 'center', justifyContent: 'center',
      transition: 'transform 0.15s, box-shadow 0.15s',
    });
    btn.addEventListener('mouseenter', () => { btn.style.transform = 'scale(1.1)'; btn.style.boxShadow = '0 4px 16px rgba(0,0,0,0.15)'; });
    btn.addEventListener('mouseleave', () => { btn.style.transform = 'scale(1)'; btn.style.boxShadow = '0 2px 12px rgba(0,0,0,0.1)'; });
    btn.addEventListener('click', () => window._hexComments.toggle());
    document.body.appendChild(btn);

    const badge = document.createElement('span');
    badge.id = 'hex-comments-badge';
    Object.assign(badge.style, {
      position: 'absolute', top: '-2px', right: '-2px', minWidth: '18px', height: '18px',
      background: '#c4553a', color: '#fff', borderRadius: '9px', fontSize: '0.65rem',
      fontWeight: '700', display: 'none', alignItems: 'center', justifyContent: 'center',
      padding: '0 4px',
    });
    btn.style.position = 'fixed';
    btn.appendChild(badge);
    return btn;
  }

  async function loadComments() {
    currentAsset = getAsset();
    document.getElementById('hc-asset-label').textContent = getAssetLabel();
    try {
      const r = await fetch(COMMENTS_BASE + '/api/comments?asset=' + encodeURIComponent(currentAsset));
      if (!r.ok) return;
      const data = await r.json();
      const list = document.getElementById('hc-list');
      if (!data.comments || data.comments.length === 0) {
        list.innerHTML = '<div class="hc-empty">No comments yet. Be the first.</div>';
      } else {
        list.innerHTML = data.comments.map(c => {
          const color = statusColors[c.status] || '#8a8880';
          const label = statusLabels[c.status] || c.status;
          const pulse = c.status === 'acting' ? '<span class="hc-acting-pulse"></span>' : '';
          let actionHtml = '';
          if (c.action_log && c.action_log.length) {
            actionHtml = c.action_log.map(a => {
              let assetTags = '';
              if (a.related_assets && a.related_assets.length) {
                assetTags = a.related_assets.map(ra => '<span class="hc-asset-tag">' + esc(ra) + '</span>').join('');
              }
              return '<div class="hc-action-log">' + esc(a.action) + assetTags + ' <span style="color:#a09a90">' + relTime(a.ts) + '</span></div>';
            }).join('');
          }
          return '<div class="hc-comment">' +
            '<div class="hc-comment-text">' + esc(c.text) + '</div>' +
            '<div class="hc-comment-meta">' +
            '<span class="hc-comment-time">' + relTime(c.created_at) + '</span>' +
            '<span class="hc-comment-status" style="color:' + color + '">' + pulse + label + '</span>' +
            '</div>' + actionHtml + '</div>';
        }).join('');
        list.scrollTop = list.scrollHeight;
      }
      // Update badge
      const newCount = data.comments.filter(c => c.status === 'new').length;
      const badge = document.getElementById('hex-comments-badge');
      if (badge) {
        if (newCount > 0) {
          badge.textContent = newCount;
          badge.style.display = 'flex';
        } else {
          badge.style.display = 'none';
        }
      }
    } catch(e) { console.error('comments load failed', e); }
  }

  window._hexComments = {
    toggle() {
      if (!panel) panel = createPanel();
      isOpen = !isOpen;
      panel.classList.toggle('open', isOpen);
      if (isOpen) loadComments();
    },
    async send() {
      const input = document.getElementById('hc-input');
      const text = input.value.trim();
      if (!text) return;
      input.value = '';
      try {
        await fetch(COMMENTS_BASE + '/api/comments', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ asset: currentAsset, text }),
        });
        loadComments();
      } catch(e) { console.error('comment send failed', e); }
    },
    refresh: loadComments,
  };

  // Create FAB on load
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', createFab);
  } else {
    createFab();
  }

  // Auto-refresh comments every 30s if panel is open
  setInterval(() => { if (isOpen) loadComments(); }, 30000);

  // Listen for asset changes (SPA navigation)
  let lastAsset = getAsset();
  setInterval(() => {
    const now = getAsset();
    if (now !== lastAsset) { lastAsset = now; if (isOpen) loadComments(); }
  }, 1000);
})();
"""


# ---------------------------------------------------------------------------
# HTTP handler
# ---------------------------------------------------------------------------

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass

    def _send(self, code, ctype, body):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, PUT, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()
        self.wfile.write(body)

    def _read_body(self) -> bytes:
        length = int(self.headers.get("Content-Length", 0))
        return self.rfile.read(length) if length else b""

    def do_OPTIONS(self):
        self._send(204, "text/plain", "")

    def do_GET(self):
        path = self.path.split("?")[0].rstrip("/") or "/"
        query = {}
        if "?" in self.path:
            for part in self.path.split("?", 1)[1].split("&"):
                if "=" in part:
                    k, v = part.split("=", 1)
                    from urllib.parse import unquote
                    query[k] = unquote(v)

        if path == "/widget.js":
            self._send(200, "application/javascript", WIDGET_JS)

        elif path == "/api/comments":
            data = _load()
            asset = query.get("asset", "")
            if asset:
                filtered = [c for c in data["comments"] if c.get("asset") == asset]
            else:
                filtered = data["comments"]
            self._send(200, "application/json", json.dumps({"comments": filtered}))

        elif path == "/api/comments/all":
            data = _load()
            self._send(200, "application/json", json.dumps(data))

        elif path == "/api/comments/pending":
            data = _load()
            pending = [c for c in data["comments"] if c.get("status") in ("new", "seen")]
            self._send(200, "application/json", json.dumps({"comments": pending}))

        elif path == "/api/comments/summary":
            data = _load()
            pending = [c for c in data["comments"] if c.get("status") in ("new", "seen")]
            by_surface: dict = {}
            for c in pending:
                asset = c.get("asset", "")
                surface = asset.split(":")[0] if ":" in asset else (asset or "unknown")
                if surface not in by_surface:
                    by_surface[surface] = {"count": 0, "comments": []}
                by_surface[surface]["count"] += 1
                by_surface[surface]["comments"].append(c)
            oldest_unacted = None
            if pending:
                oldest_unacted = min(
                    (c.get("created_at") for c in pending if c.get("created_at")),
                    default=None,
                )
            summary = {
                "total_pending": len(pending),
                "by_surface": by_surface,
                "oldest_unacted": oldest_unacted,
            }
            self._send(200, "application/json", json.dumps(summary))

        else:
            self._send(404, "text/plain", "not found")

    def do_POST(self):
        path = self.path.split("?")[0].rstrip("/") or "/"
        body = self._read_body()

        if path == "/api/comments":
            try:
                req = json.loads(body)
            except json.JSONDecodeError:
                self._send(400, "application/json", '{"error":"invalid json"}')
                return
            asset = req.get("asset", "").strip()
            text = req.get("text", "").strip()
            if not text:
                self._send(400, "application/json", '{"error":"text required"}')
                return
            comment = {
                "id": f"c-{uuid.uuid4().hex[:8]}",
                "asset": asset,
                "text": text,
                "author": req.get("author", "mike"),
                "status": "new",
                "created_at": datetime.now(timezone.utc).isoformat(),
                "action_log": [],
                "routed_to": [],
            }
            data = _load()
            data["comments"].append(comment)
            _save(data)
            _emit_event(comment)
            self._send(201, "application/json", json.dumps(comment))

        elif path == "/api/comments/update":
            try:
                req = json.loads(body)
            except json.JSONDecodeError:
                self._send(400, "application/json", '{"error":"invalid json"}')
                return
            comment_id = req.get("id", "")
            data = _load()
            for c in data["comments"]:
                if c["id"] == comment_id:
                    if "status" in req:
                        c["status"] = req["status"]
                    if "action" in req:
                        entry = {
                            "ts": datetime.now(timezone.utc).isoformat(),
                            "action": req["action"],
                        }
                        if "related_assets" in req:
                            entry["related_assets"] = req["related_assets"]
                        c.setdefault("action_log", []).append(entry)
                    if "routed_to" in req:
                        c["routed_to"] = req["routed_to"]
                    _save(data)
                    self._send(200, "application/json", json.dumps(c))
                    return
            self._send(404, "application/json", '{"error":"not found"}')

        else:
            self._send(404, "text/plain", "not found")


if __name__ == "__main__":
    with socketserver.TCPServer(("127.0.0.1", PORT), Handler) as httpd:
        httpd.allow_reuse_address = True
        print(f"comments-service on http://127.0.0.1:{PORT}", flush=True)
        httpd.serve_forever()
