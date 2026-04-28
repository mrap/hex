# Hex UI Extensibility — Proposal

> Companion to `docs/extensibility/proposal.md` (2026-04-27).
> Focuses on custom web views, widget extensions, proxy routes, and design system integration.

---

## Executive Summary

Hex's HTTP server already serves the landing page, the comment widget, and SSE streams. UI
extensibility adds four surfaces on top of that foundation:

1. **Custom views** — full-page HTML/JS apps served at `/ext/<name>`
2. **Widget extensions** — embeddable panels injected into the hex landing page
3. **Proxy routes** — pass-through mounts for user's own servers (already partially working)
4. **Design system tokens** — shared CSS custom properties so extensions look native

All four surfaces are declared in `extension.yaml` (see `proposal.md §2.6`) and discovered at
server startup. No binary modification is required.

---

## 1. Custom Views

### 1.1 What a Custom View Is

A custom view is a self-contained directory of static assets (HTML, JS, CSS, images) that the
hex server mounts at a route and serves. The view runs in the browser — not in the hex binary.
It communicates with the hex server via:

- **SSE subscriptions** — `GET /sse?topic=<name>` (existing endpoint)
- **REST calls** — `GET /api/events`, `GET /api/messages`, `GET /api/assets`, etc. (existing)
- **View-specific REST** — `GET /ext/<view-name>/api/...` (optional, extension-declared)

### 1.2 Directory Layout

```
~/.hex/extensions/my-dashboard/
  extension.yaml
  views/
    activity/                     ← served at /ext/activity
      index.html
      app.js
      style.css
      assets/
        logo.png
```

### 1.3 extension.yaml — View Declaration

```yaml
name: my-dashboard
version: "1.0.0"
type: ui_view
engines:
  hex: ">=0.8.0"
views:
  - name: activity
    path: /ext/activity           # URL path (must start with /ext/)
    title: "Activity Dashboard"   # shown in hex landing page nav
    icon: "📊"                   # optional, shown in nav
    entry: views/activity/index.html
    description: "Real-time event activity across all hex topics"
    sse_topics:                   # declare which SSE topics this view reads
      - hex.events
      - hex.messages
```

### 1.4 Server-Side Mounting

At startup, `hex server` scans extension directories, reads each `extension.yaml`, and for every
`views:` entry:

1. Registers a static file handler at the declared `path`
2. Injects the view into the landing page nav (see §4 Design System)
3. Makes SSE topic subscriptions available at the declared path prefix

No build step. The hex server is the CDN — it serves files verbatim.

### 1.5 How Views Access Hex Data

The hex server exposes a stable REST API that views call directly from the browser:

```
GET /api/events?limit=50&topic=<name>      ← recent events
GET /api/messages?since=<iso-timestamp>    ← recent messages
GET /api/assets?prefix=<key>               ← asset listing
GET /api/assets/<key>                      ← asset download
GET /sse?topic=<name>                      ← SSE stream (existing)
GET /sse?topic=hex.events,hex.messages     ← multi-topic subscription
```

These endpoints exist today. UI extensions consume them as-is. Views must include the
`X-Hex-View: <name>` request header so server logs can attribute traffic.

### 1.6 SSE Integration in Views

The JS snippet a view uses to subscribe to SSE:

```js
// hex-sdk.js — provided by the hex server at /ext/hex-sdk.js
// Views import it from the server so they always get the correct version.

import { hexSSE, hexAPI } from '/ext/hex-sdk.js';

// Subscribe to one or more SSE topics
const stream = hexSSE(['hex.events', 'github.pr.opened'], (event) => {
  console.log(event.topic, event.data);
});

// Unsubscribe on cleanup
stream.close();
```

`hex-sdk.js` is a 3 KB module shipped with the hex binary. It wraps `EventSource` with:
- Automatic reconnection with exponential backoff
- Multi-topic demultiplexing (one `EventSource`, many topics via server-sent topic tags)
- `hexAPI(path, options)` — thin `fetch` wrapper that sets required headers

### 1.7 View Sandboxing

Views run in the browser's origin sandbox. The hex server sets:

```
Content-Security-Policy: default-src 'self'; script-src 'self'; connect-src 'self' ws://hex.local
X-Frame-Options: SAMEORIGIN
```

Views cannot load external scripts, make cross-origin API calls, or embed arbitrary iframes.
They can only talk to the hex server. This is enforced at the HTTP layer, not by convention.

---

## 2. Widget Extensions

### 2.1 What a Widget Is

A widget is smaller than a view — it's a panel injected into the hex landing page itself
(alongside the existing comment widget). Widgets are declared in `extension.yaml` and rendered
as `<iframe>` sandboxes or as web components depending on isolation level.

### 2.2 Widget Declaration

```yaml
name: pr-status-widget
version: "1.0.0"
type: ui_view
engines:
  hex: ">=0.8.0"
widgets:
  - name: pr-status
    title: "Open PRs"
    entry: widgets/pr-status/widget.html
    placement: sidebar        # sidebar | main | footer
    size: compact             # compact (1 col) | wide (2 col) | full (full width)
    sse_topics:
      - github.pr.opened
      - github.pr.closed
```

### 2.3 Widget Registration

The hex landing page loads at startup with an auto-generated registry of installed widgets:

```html
<!-- injected by hex server into landing page <head> -->
<script src="/ext/widget-registry.js"></script>
```

`widget-registry.js` is generated at server start from the discovered `extension.yaml` files.
It tells the landing page which widgets to mount and where:

```js
// auto-generated — do not edit
window.__HEX_WIDGETS__ = [
  { name: 'pr-status', placement: 'sidebar', src: '/ext/pr-status-widget/widgets/pr-status/widget.html' },
];
```

The landing page iterates `__HEX_WIDGETS__` and creates an `<iframe>` per widget. Each iframe
loads the widget HTML, which imports `hex-sdk.js` from the parent origin.

### 2.4 Widget API

Widgets communicate with the landing page via `postMessage`:

```js
// widget.html (inside iframe)
import { hexSSE, hexAPI, hexWidget } from '/ext/hex-sdk.js';

// Resize the iframe (landing page adjusts height)
hexWidget.resize(240);

// Notify the landing page of a badge count
hexWidget.badge(3);

// Navigate to a full view
hexWidget.navigate('/ext/activity');
```

The landing page listens for these messages and reacts — resizing iframes, updating nav badges,
triggering navigation. This keeps widget code isolated from landing page internals.

### 2.5 Existing Comment Widget Compatibility

The existing `widget.js` (the comment widget) is unchanged. Widget extensions use the same
CSS custom properties (see §4) so they inherit hex's visual style automatically.

---

## 3. Proxy Extensions

### 3.1 What a Proxy Route Is

A proxy route tells the hex server to forward requests for `/ext/<name>/...` to a user-owned
server (e.g., a Python experiment, a local dashboard, a dev server). The hex server acts as a
reverse proxy — the user's app is invisible to the browser, which only sees hex origin URLs.

This already works partially (hex proxies to Python experiments). This formalizes the contract.

### 3.2 Proxy Declaration in extension.yaml

```yaml
name: my-experiment
version: "1.0.0"
type: proxy
engines:
  hex: ">=0.8.0"
proxies:
  - name: experiment
    path: /ext/experiment         # hex serves this path
    upstream: http://localhost:8080  # forwards to this origin
    title: "ML Experiment"        # shown in landing page nav
    icon: "🧪"
    health_check: /health         # optional: hex polls this to show up/down in nav
    strip_prefix: false           # if true, /ext/experiment/foo → /foo at upstream
```

### 3.3 How the Proxy Works

At startup, hex registers a reverse-proxy handler for each declared proxy route. On each
request to `/ext/experiment/...`:

1. Hex checks if the upstream is reachable (health check result)
2. If healthy: forwards request, streams response back
3. If unhealthy: serves a 502 page with a "start your server" hint

The hex landing page shows proxy routes in the nav with a status indicator (green/grey dot)
derived from the last health check result.

### 3.4 Startup and Lifecycle

Proxy routes are **passive** — hex does not start the upstream process. That's the user's job.
The user runs their server (e.g., `python3 my_experiment.py`) and hex routes to it.

Optional: a proxy extension can declare a `start_command` for `hex extension start <name>`:

```yaml
proxies:
  - name: experiment
    upstream: http://localhost:8080
    start_command: "python3 experiments/my_experiment.py"
    stop_signal: SIGTERM
```

`hex extension start experiment` launches the command as a background process managed by hex,
with output piped to `~/.hex/audit/ext-experiment.log`.

---

## 4. Design System

### 4.1 CSS Custom Properties (Already in widget.js)

The hex landing page and comment widget already define a set of CSS custom properties. Extensions
inherit these automatically by linking to `/ext/hex-sdk.css`:

```html
<!-- in view index.html or widget.html -->
<link rel="stylesheet" href="/ext/hex-sdk.css">
```

`hex-sdk.css` exposes the canonical token set:

```css
:root {
  /* Color */
  --hex-color-bg:          #0d1117;
  --hex-color-surface:     #161b22;
  --hex-color-border:      #30363d;
  --hex-color-text:        #e6edf3;
  --hex-color-text-muted:  #7d8590;
  --hex-color-accent:      #58a6ff;
  --hex-color-success:     #3fb950;
  --hex-color-warning:     #d29922;
  --hex-color-error:       #f85149;

  /* Typography */
  --hex-font-mono:         'JetBrains Mono', 'Fira Code', monospace;
  --hex-font-sans:         system-ui, -apple-system, sans-serif;
  --hex-font-size-sm:      0.75rem;
  --hex-font-size-base:    0.875rem;
  --hex-font-size-lg:      1rem;

  /* Spacing */
  --hex-space-xs:   4px;
  --hex-space-sm:   8px;
  --hex-space-md:   16px;
  --hex-space-lg:   24px;
  --hex-space-xl:   32px;

  /* Radius */
  --hex-radius-sm:  4px;
  --hex-radius-md:  6px;
  --hex-radius-lg:  12px;
}
```

### 4.2 No Shared Component Library

Extensions do not share a React/Svelte/Vue component library. Reasons:

1. **Framework lock-in** — a shared library forces all extensions to use the same framework
2. **Version conflicts** — React 18 vs React 19 in the same page is a nightmare
3. **Bundle size** — every view would download the full component library
4. **Unnecessary** — design tokens + plain CSS cover 90% of consistency needs

Extensions that want components bring their own, bounded inside their iframe sandbox.

### 4.3 Landing Page Auto-Discovery

The hex landing page renders a nav section for all installed extensions. The nav is generated
server-side from the discovered `extension.yaml` files and injected into the landing page HTML.
Each entry shows the extension's `title`, `icon`, and status (online/offline for proxies).

```html
<!-- auto-generated nav section in landing page -->
<nav class="hex-ext-nav">
  <a href="/ext/activity" class="hex-ext-nav-item">
    <span class="hex-ext-icon">📊</span>
    <span class="hex-ext-title">Activity Dashboard</span>
  </a>
  <a href="/ext/experiment" class="hex-ext-nav-item hex-ext-status-online">
    <span class="hex-ext-icon">🧪</span>
    <span class="hex-ext-title">ML Experiment</span>
    <span class="hex-ext-dot"></span>
  </a>
</nav>
```

No JavaScript required for discovery — the server renders the nav at request time.

---

## 5. Extension Lifecycle (UI-Specific)

### 5.1 Install

```bash
hex extension install /path/to/my-dashboard/   # local path
hex extension install github:user/my-dashboard  # GitHub source
```

Copies assets to `~/.hex/extensions/<name>/`. Server picks up changes on next restart (or hot-reload
if `hex server --dev` is running).

### 5.2 Hot Reload (Dev Mode)

```bash
hex server --dev
```

In dev mode, the hex server watches `~/.hex/extensions/` and `<repo>/.hex/extensions/` for
file changes and reloads:
- Static file handlers (views, widgets) — no restart needed, browser refresh is sufficient
- `extension.yaml` changes — server reloads the registry and pushes an SSE event
  (`hex.extensions.changed`) that landing page JS listens to for auto-refresh

### 5.3 Remove

```bash
hex extension remove my-dashboard
```

Deletes `~/.hex/extensions/my-dashboard/`. Server deregisters routes on next restart.

### 5.4 List

```bash
hex extension list
```

```
EXTENSIONS
  my-dashboard   v1.0.0   ui_view   2 views, 1 widget   active
  my-experiment  v0.3.0   proxy     /ext/experiment      online (127.0.0.1:8080)
  gh-policy      v1.1.0   policy    github.pr.*          active
```

---

## 6. Contract Summary

| Surface | Declared in | Served at | Accesses hex via | Isolation |
|---------|------------|-----------|-----------------|-----------|
| Custom view | `extension.yaml views:` | `/ext/<name>` | REST + SSE | Browser CSP |
| Widget | `extension.yaml widgets:` | `/ext/<bundle>/widgets/<name>` | REST + SSE + postMessage | iframe |
| Proxy route | `extension.yaml proxies:` | `/ext/<name>/...` → upstream | N/A (upstream is standalone) | Process boundary |
| SDK | Shipped by hex | `/ext/hex-sdk.js`, `/ext/hex-sdk.css` | N/A | N/A |

---

## 7. What Hex Does Not Provide

To keep scope small and avoid framework lock-in, hex UI extensibility intentionally omits:

- **Server-side rendering** — views are static files only; no templating engine
- **WebSocket support** — SSE covers all real-time needs; WS adds complexity for no gain
- **Extension-to-extension communication** — extensions are isolated; they only talk to hex
- **Hot-module replacement** — dev mode watches files; HMR is too framework-specific
- **Shared component library** — design tokens cover consistency; components stay per-extension
- **Extension marketplace UI** — `hex extension list/install/remove` is the UX; no web UI

---

## 8. Implementation Phases

| Phase | What ships | Effort |
|-------|-----------|--------|
| **Phase 1** | Static view mounting from `extension.yaml views:`; landing page nav | ~2 days |
| **Phase 2** | `hex-sdk.js` + `hex-sdk.css`; SSE multi-topic demux | ~1 day |
| **Phase 3** | Widget injection (iframe + postMessage API) | ~2 days |
| **Phase 4** | Proxy route formalization + health checks + `hex extension start` | ~1 day |
| **Phase 5** | Dev mode hot-reload (`--dev` flag + file watcher) | ~1 day |

Total: ~7 days of implementation. No changes to the Rust binary's core event system.
