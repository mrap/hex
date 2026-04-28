(function() {
  const MESSAGES_BASE = '/messages/';
  const SSE_URL = (window._hexSSE || '') + '/events/stream?topics=content.messages';
  let panel = null;
  let isOpen = false;
  let currentAsset = null;
  let evtSource = null;

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
    'new': 'var(--hc-status-new, #c4553a)',
    'seen': 'var(--hc-status-seen, #b85c14)',
    'acting': 'var(--hc-status-acting, #8b6f47)',
    'done': 'var(--hc-status-done, #2d7a3a)',
    'dismissed': 'var(--hc-status-dismissed, #a09a90)',
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

  function updateBadge() {
    fetch(MESSAGES_BASE + 'api/messages?type=comment&anchor=' + encodeURIComponent(getAsset()))
      .then(r => r.ok ? r.json() : null)
      .then(data => {
        if (!data) return;
        const newCount = (data.messages || []).filter(c => c.status === 'new').length;
        const badge = document.getElementById('hex-comments-badge');
        if (!badge) return;
        if (newCount > 0) {
          badge.textContent = newCount;
          badge.style.display = 'flex';
        } else {
          badge.style.display = 'none';
        }
      })
      .catch(() => {});
  }

  function connectSSE() {
    if (evtSource) evtSource.close();
    evtSource = new EventSource(SSE_URL);
    evtSource.onmessage = function(e) {
      try {
        const evt = JSON.parse(e.data);
        if (evt.topic === 'content.messages') {
          if (!currentAsset || !evt.payload.anchor || evt.payload.anchor === currentAsset) {
            loadComments();
          }
          if (evt.type === 'created') updateBadge();
        }
      } catch(err) {}
    };
    evtSource.onerror = function() { setTimeout(connectSSE, 5000); };
  }

  function createPanel() {
    const div = document.createElement('div');
    div.id = 'hex-comments-panel';
    div.innerHTML = `
      <style>
        :root {
          --hc-bg: #faf7f0;
          --hc-border: #e8e0d0;
          --hc-header-font: 'Fraunces', serif;
          --hc-body-font: 'Work Sans', -apple-system, sans-serif;
          --hc-text: #1c1a16;
          --hc-text-muted: #8a8880;
          --hc-text-dim: #a09a90;
          --hc-text-secondary: #6e695f;
          --hc-accent: #c4553a;
          --hc-bg-alt: #f5f0e8;
          --hc-bg-item: #f0ebe3;
          --hc-bg-tag: #eee8dc;
          --hc-btn-bg: #1c1a16;
          --hc-btn-fg: #faf7f0;
          --hc-status-new: #c4553a;
          --hc-status-seen: #b85c14;
          --hc-status-acting: #8b6f47;
          --hc-status-done: #2d7a3a;
          --hc-status-dismissed: #a09a90;
        }
        #hex-comments-panel {
          position: fixed; right: 20px; bottom: 80px; width: 360px; max-height: 70vh;
          background: var(--hc-bg); border: 1px solid var(--hc-border); border-radius: 12px;
          box-shadow: 0 8px 30px rgba(0,0,0,0.12); z-index: 10000;
          display: none; flex-direction: column; font-family: var(--hc-body-font);
          overflow: hidden;
        }
        #hex-comments-panel.open { display: flex; }
        .hc-header {
          padding: 14px 18px; border-bottom: 1px solid var(--hc-border);
          display: flex; align-items: center; justify-content: space-between;
        }
        .hc-header h3 {
          font-family: var(--hc-header-font); font-size: 1rem; font-weight: 700; margin: 0;
        }
        .hc-header .hc-close {
          background: none; border: none; font-size: 1.2rem; cursor: pointer;
          color: var(--hc-text-muted); padding: 4px;
        }
        .hc-header .hc-close:hover { color: var(--hc-accent); }
        .hc-asset-label {
          padding: 6px 18px; font-size: 0.72rem; color: var(--hc-text-muted);
          background: var(--hc-bg-alt); border-bottom: 1px solid var(--hc-border);
        }
        .hc-list {
          flex: 1; overflow-y: auto; padding: 12px 18px;
          max-height: calc(70vh - 160px);
        }
        .hc-empty {
          color: var(--hc-text-dim); font-size: 0.85rem; font-style: italic; padding: 20px 0; text-align: center;
        }
        .hc-comment {
          padding: 10px 0; border-bottom: 1px solid var(--hc-bg-item);
        }
        .hc-comment:last-child { border-bottom: none; }
        .hc-comment-text { font-size: 0.88rem; line-height: 1.45; color: var(--hc-text); }
        .hc-comment-meta {
          display: flex; align-items: center; gap: 8px; margin-top: 4px;
        }
        .hc-comment-time { font-size: 0.72rem; color: var(--hc-text-dim); }
        .hc-comment-status {
          font-size: 0.65rem; font-weight: 600; padding: 1px 6px;
          border-radius: 4px; background: var(--hc-bg-item);
        }
        .hc-action-log {
          margin-top: 4px; font-size: 0.72rem; color: var(--hc-text-secondary);
          padding-left: 10px; border-left: 2px solid var(--hc-border);
        }
        .hc-acting-pulse {
          display: inline-block; width: 6px; height: 6px; border-radius: 50%;
          background: var(--hc-status-acting); margin-right: 5px; vertical-align: middle;
          animation: hcPulse 1.4s ease-in-out infinite;
        }
        @keyframes hcPulse {
          0%, 100% { opacity: 0.4; transform: scale(0.8); }
          50% { opacity: 1; transform: scale(1.2); }
        }
        .hc-asset-tag {
          display: inline-block; font-size: 0.62rem; padding: 1px 5px;
          background: var(--hc-bg-tag); border-radius: 3px; color: var(--hc-text-secondary);
          margin-left: 4px; font-weight: 500;
        }
        .hc-input-wrap {
          padding: 12px 18px; border-top: 1px solid var(--hc-border);
          display: flex; gap: 8px;
        }
        .hc-input {
          flex: 1; padding: 8px 12px; border: 1px solid var(--hc-border); border-radius: 8px;
          font-family: var(--hc-body-font); font-size: 0.85rem; background: #fff;
          outline: none; resize: none;
        }
        .hc-input:focus { border-color: var(--hc-accent); }
        .hc-send {
          padding: 8px 14px; background: var(--hc-btn-bg); color: var(--hc-btn-fg);
          border: none; border-radius: 8px; font-family: var(--hc-body-font);
          font-size: 0.82rem; font-weight: 500; cursor: pointer;
        }
        .hc-send:hover { background: var(--hc-accent); }
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
    btn.innerHTML = '&#128172;';
    btn.title = 'Comments';
    Object.assign(btn.style, {
      position: 'fixed', right: '20px', bottom: '20px', width: '48px', height: '48px',
      borderRadius: '50%', border: '1px solid var(--hc-border, #e8e0d0)', background: '#fff',
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
      background: 'var(--hc-status-new, #c4553a)', color: '#fff', borderRadius: '9px', fontSize: '0.65rem',
      fontWeight: '700', display: 'none', alignItems: 'center', justifyContent: 'center',
      padding: '0 4px',
    });
    btn.style.position = 'fixed';
    btn.appendChild(badge);

    connectSSE();
    updateBadge();
    return btn;
  }

  async function loadComments() {
    currentAsset = getAsset();
    document.getElementById('hc-asset-label').textContent = getAssetLabel();
    try {
      const r = await fetch(MESSAGES_BASE + 'api/messages?type=comment&anchor=' + encodeURIComponent(currentAsset));
      if (!r.ok) return;
      const data = await r.json();
      const list = document.getElementById('hc-list');
      if (!data.messages || data.messages.length === 0) {
        list.innerHTML = '<div class="hc-empty">No comments yet. Be the first.</div>';
      } else {
        list.innerHTML = data.messages.map(c => {
          const color = statusColors[c.status] || 'var(--hc-text-muted, #8a8880)';
          const label = statusLabels[c.status] || c.status;
          const pulse = c.status === 'acting' ? '<span class="hc-acting-pulse"></span>' : '';
          let actionHtml = '';
          if (c.action_log && c.action_log.length) {
            actionHtml = c.action_log.map(a => {
              let assetTags = '';
              if (a.related_assets && a.related_assets.length) {
                assetTags = a.related_assets.map(ra => '<span class="hc-asset-tag">' + esc(ra) + '</span>').join('');
              }
              return '<div class="hc-action-log">' + esc(a.action) + assetTags + ' <span style="color:var(--hc-text-dim,#a09a90)">' + relTime(a.ts) + '</span></div>';
            }).join('');
          }
          return '<div class="hc-comment">' +
            '<div class="hc-comment-text">' + esc(c.content) + '</div>' +
            '<div class="hc-comment-meta">' +
            '<span class="hc-comment-time">' + relTime(c.created_at) + '</span>' +
            '<span class="hc-comment-status" style="color:' + color + '">' + pulse + label + '</span>' +
            '</div>' + actionHtml + '</div>';
        }).join('');
        list.scrollTop = list.scrollHeight;
      }
      updateBadge();
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
        await fetch(MESSAGES_BASE + 'api/messages', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ msg_type: 'comment', from: 'mike', to: [], content: text, anchor: currentAsset }),
        });
        loadComments();
      } catch(e) { console.error('comment send failed', e); }
    },
    refresh: loadComments,
  };

  // Create FAB on load (also starts SSE connection)
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', createFab);
  } else {
    createFab();
  }

  // Listen for asset changes (SPA navigation)
  let lastAsset = getAsset();
  setInterval(() => {
    const now = getAsset();
    if (now !== lastAsset) { lastAsset = now; if (isOpen) loadComments(); }
  }, 1000);
})();
