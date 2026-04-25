"""
hex-router — tiny path-routing reverse proxy for named local services.

Runs on 127.0.0.1:7000. Tailscale Serve fronts it on :443 so users get:
  https://mac-mini.tailbd5748.ts.net/ui        → hex-ui MVP
  https://mac-mini.tailbd5748.ts.net/boi       → BOI live status
  https://mac-mini.tailbd5748.ts.net/visions   → UX vision pitch site
  https://mac-mini.tailbd5748.ts.net/demos     → demos page
  https://mac-mini.tailbd5748.ts.net/          → landing page

To add a new named service, add an entry to ROUTES.
"""

from __future__ import annotations

import http.client
import http.server
import os
import socketserver
import ssl
import sys
from urllib.parse import quote, unquote


# (path_prefix, backend_host, backend_port, backend_scheme, strip_prefix)
# Order matters: more-specific paths first.
# strip_prefix=True: forward path without prefix (backend doesn't expect it)
# strip_prefix=False: forward path AS-IS (backend expects the full path)
ROUTES = [
    ("/api",        "127.0.0.1", 8889, "http",  False),  # hex-ui API (backend has /api/* routes)
    ("/ui/assets",  "127.0.0.1", 8889, "http",  False),  # static assets (preserve full path)
    ("/ui",         "127.0.0.1", 8889, "http",  True),   # hex-ui production build (strip prefix)
    ("/secrets", "127.0.0.1", 9877, "http",  True),
    ("/pulse",   "127.0.0.1", 8896, "http",  True),
    ("/compare", "127.0.0.1", 8895, "http",  True),
    ("/artifacts", "127.0.0.1", 8897, "http",  True),
    ("/boi",     "127.0.0.1", 8891, "https", True),
    ("/visions", "127.0.0.1", 8890, "https", True),
]

# Convenience redirects: /alias  →  /target
REDIRECTS = {
    "/demos": "/visions/demos.html",
}

# Landing page listing the services.
LANDING = ("""<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"/><meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>hex</title>
<style>
body {
  background: #faf7f0; color: #1c1a16;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
  line-height: 1.55; margin: 0; min-height: 100vh;
  display: flex; align-items: center; justify-content: center;
}
main { max-width: 520px; padding: 2.5rem; }
h1 { font-size: 1.4rem; letter-spacing: -0.01em; margin: 0 0 0.35rem; font-weight: 700; }
h1 span { color: #b85c14; }
p { color: #6e695f; font-size: 0.95rem; margin: 0 0 1.5rem; }
.list { display: grid; gap: 0.35rem; border: 1px solid #e8e0d0; border-radius: 6px; overflow: hidden; background: #fff; }
a.row {
  display: grid; grid-template-columns: 8rem 1fr auto;
  gap: 1rem; padding: 0.75rem 1rem;
  text-decoration: none; color: #1c1a16; align-items: baseline;
  border-bottom: 1px solid #e8e0d0; transition: background 0.15s;
}
a.row:last-child { border-bottom: 0; }
a.row:hover { background: #fcf5e5; }
a.row .name {
  font-family: "SF Mono", Menlo, Consolas, monospace; font-size: 0.88rem; color: #b85c14; font-weight: 600;
}
a.row .desc { font-size: 0.88rem; }
a.row .arrow { color: #b85c14; opacity: 0.6; font-size: 0.85rem; }
footer { margin-top: 1.5rem; font-family: "SF Mono", Menlo, Consolas, monospace; font-size: 0.7rem; color: #8a8880; }
</style>
</head><body>
<main>
  <h1><span>▶</span> hex — local services</h1>
  <p>Named paths for the services running on the mac-mini. Add more by editing <code>hex-router/router.py</code>.</p>
  <div class="list">
    <a class="row" href="/ui/">        <span class="name">/ui</span>      <span class="desc">hex-ui MVP (Morning Anchor, conversation)</span> <span class="arrow">→</span></a>
    <a class="row" href="/boi">        <span class="name">/boi</span>     <span class="desc">BOI live status — running specs, progress, queue</span> <span class="arrow">→</span></a>
    <a class="row" href="/visions/">   <span class="name">/visions</span> <span class="desc">UX vision pitch site + principles</span> <span class="arrow">→</span></a>
    <a class="row" href="/demos">      <span class="name">/demos</span>   <span class="desc">Usability demos — interactable sketches</span> <span class="arrow">→</span></a>
    <a class="row" href="/secrets">    <span class="name">/secrets</span><span class="desc">Credential intake — API keys, tokens, PEM files</span> <span class="arrow">→</span></a>
    <a class="row" href="/fleet">      <span class="name">/fleet</span>  <span class="desc">Live agent fleet org chart</span> <span class="arrow">→</span></a>
  </div>
  <footer>hex-router · tailscale-served · light mode</footer>
</main>
</body></html>
""").encode("utf-8")


def find_route(path: str):
    for prefix, host, port, scheme, strip in ROUTES:
        if path == prefix or path.startswith(prefix + "/"):
            if strip:
                forwarded = path[len(prefix):] or "/"
            else:
                forwarded = path  # preserve full path (backend expects it)
            return (prefix, host, port, scheme, forwarded)
    return None


class ProxyHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # quiet
        pass

    def _tunnel_websocket(self, host, port, path):
        """Tunnel a WebSocket upgrade request by forwarding raw bytes between client and upstream."""
        import socket as _sock, select as _select, threading as _threading

        # Build the raw HTTP upgrade request to send upstream
        raw_request = f"{self.command} {path} HTTP/1.1\r\n"
        for name, value in self.headers.items():
            if name.lower() == "host":
                raw_request += f"Host: {host}:{port}\r\n"
            else:
                raw_request += f"{name}: {value}\r\n"
        raw_request += "\r\n"

        try:
            upstream = _sock.create_connection((host, port), timeout=10)
            upstream.sendall(raw_request.encode("utf-8"))
        except Exception as exc:
            self.send_response(502)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(f"WebSocket upstream error: {exc}\n".encode())
            return

        # Forward the upstream 101 response back to the client
        client_sock = self.request

        # Relay: upstream → client
        def relay(src, dst):
            try:
                while True:
                    data = src.recv(65536)
                    if not data:
                        break
                    dst.sendall(data)
            except Exception:
                pass
            finally:
                try: src.shutdown(_sock.SHUT_RD)
                except Exception: pass
                try: dst.shutdown(_sock.SHUT_WR)
                except Exception: pass

        t1 = _threading.Thread(target=relay, args=(upstream, client_sock), daemon=True)
        t2 = _threading.Thread(target=relay, args=(client_sock, upstream), daemon=True)
        t1.start()
        t2.start()
        t1.join()
        t2.join()
        try: upstream.close()
        except Exception: pass

    def _handle(self):
        path = self.path

        # WebSocket upgrade — tunnel directly
        if self.headers.get("Upgrade", "").lower() == "websocket":
            route = find_route(path)
            if route:
                prefix, host, port, scheme, stripped = route
                self._tunnel_websocket(host, port, stripped)
                return

        # landing at /
        if path == "/" or path == "":
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(LANDING)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(LANDING)
            return

        # static files served from hex-router/static/
        STATIC_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "static")
        if path.startswith("/fleet"):
            static_path = os.path.join(STATIC_DIR, "fleet.html")
            if os.path.isfile(static_path):
                with open(static_path, "rb") as f:
                    content = f.read()
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(content)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(content)
                return

        # OAuth callback proxy: /auth/callback/<port>?... → http://localhost:<port>/callback?...
        path_only, _, path_query = path.partition("?")
        if path_only.startswith("/auth/callback/"):
            port_str = path_only[len("/auth/callback/"):].split("/")[0]
            if port_str.isdigit():
                target_port = int(port_str)
                target_path = "/callback" + ("?" + path_query if path_query else "")
                content_length = int(self.headers.get("Content-Length", 0) or 0)
                body = self.rfile.read(content_length) if content_length > 0 else None
                fwd_headers = {
                    k: v for k, v in self.headers.items()
                    if k.lower() not in ("host", "connection", "content-length")
                }
                fwd_headers["Host"] = f"127.0.0.1:{target_port}"
                if body is not None:
                    fwd_headers["Content-Length"] = str(len(body))
                conn = None
                try:
                    conn = http.client.HTTPConnection("127.0.0.1", target_port, timeout=30)
                    conn.request(self.command, target_path, body=body, headers=fwd_headers)
                    resp = conn.getresponse()
                    self.send_response(resp.status, resp.reason)
                    for name, value in resp.getheaders():
                        if name.lower() in ("connection", "transfer-encoding", "content-length"):
                            continue
                        self.send_header(name, value)
                    self.send_header("Connection", "close")
                    self.end_headers()
                    while True:
                        chunk = resp.read(4096)
                        if not chunk:
                            break
                        self.wfile.write(chunk)
                except Exception as exc:
                    self.send_response(502)
                    self.send_header("Content-Type", "text/plain")
                    self.send_header("Connection", "close")
                    self.end_headers()
                    self.wfile.write(f"hex-router: OAuth callback error (port {target_port}): {exc!r}\n".encode())
                finally:
                    if conn:
                        try:
                            conn.close()
                        except Exception:
                            pass
                return

        # convenience redirects
        if path in REDIRECTS:
            target = REDIRECTS[path]
            self.send_response(302)
            self.send_header("Location", target)
            self.send_header("Connection", "close")
            self.end_headers()
            return

        route = find_route(path)
        if not route:
            self.send_response(404)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(b"hex-router: no named service matches this path\n")
            self.wfile.write(b"try /, /ui/, /boi, /visions/, /demos, /fleet, /secrets\n")
            return

        # also auto-redirect bare prefix (e.g. /ui) to trailing-slash form
        # so relative paths in the served HTML resolve correctly
        prefix, host, port, scheme, stripped = route
        if path == prefix:
            self.send_response(302)
            self.send_header("Location", prefix + "/")
            self.send_header("Connection", "close")
            self.end_headers()
            return

        # read request body, if any
        content_length = int(self.headers.get("Content-Length", 0) or 0)
        body = self.rfile.read(content_length) if content_length > 0 else None

        # forward headers
        fwd_headers = {}
        for name, value in self.headers.items():
            if name.lower() in ("host", "connection", "content-length"):
                continue
            fwd_headers[name] = value
        fwd_headers["Host"] = f"{host}:{port}"
        fwd_headers["X-Forwarded-Host"] = self.headers.get("Host", "")
        fwd_headers["X-Forwarded-Prefix"] = prefix
        fwd_headers["X-Forwarded-Proto"] = "https"
        if body is not None:
            fwd_headers["Content-Length"] = str(len(body))

        # open connection to backend
        try:
            if scheme == "https":
                ctx = ssl._create_unverified_context()
                conn = http.client.HTTPSConnection(host, port, context=ctx, timeout=300)
            else:
                conn = http.client.HTTPConnection(host, port, timeout=300)
            conn.request(self.command, stripped, body=body, headers=fwd_headers)
            resp = conn.getresponse()
        except Exception as exc:
            self.send_response(502)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(f"hex-router: upstream error: {exc!r}\n".encode("utf-8"))
            return

        # detect SSE
        is_sse = False
        content_type = resp.getheader("Content-Type", "") or ""
        if "text/event-stream" in content_type.lower():
            is_sse = True

        # write response line + headers
        self.send_response(resp.status, resp.reason)
        for name, value in resp.getheaders():
            lname = name.lower()
            if lname in ("connection", "transfer-encoding", "content-length"):
                continue
            self.send_header(name, value)
        # for non-SSE, compute content length or stream without it
        if is_sse:
            self.send_header("X-Accel-Buffering", "no")
            self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.send_header("Transfer-Encoding", "chunked") if is_sse else None
        self.end_headers()

        # stream the body
        try:
            if is_sse:
                # SSE: read line-by-line so small events flush immediately
                while True:
                    line = resp.readline(8192)
                    if not line:
                        break
                    self.wfile.write(f"{len(line):X}\r\n".encode("ascii"))
                    self.wfile.write(line)
                    self.wfile.write(b"\r\n")
                    try:
                        self.wfile.flush()
                    except Exception:
                        break
            else:
                while True:
                    chunk = resp.read(4096)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    try:
                        self.wfile.flush()
                    except Exception:
                        break
            if is_sse:
                self.wfile.write(b"0\r\n\r\n")
                try:
                    self.wfile.flush()
                except Exception:
                    pass
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            try:
                conn.close()
            except Exception:
                pass

    do_GET = _handle
    do_POST = _handle
    do_HEAD = _handle
    do_PUT = _handle
    do_DELETE = _handle
    do_OPTIONS = _handle
    do_PATCH = _handle


class ThreadingServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    port = int(os.environ.get("PORT", "8880"))
    server = ThreadingServer(("127.0.0.1", port), ProxyHandler)
    print(f"hex-router on http://127.0.0.1:{port}", flush=True)
    print("routes:", flush=True)
    for prefix, host, p, scheme, _strip in ROUTES:
        print(f"  {prefix} → {scheme}://{host}:{p}", flush=True)
    for alias, tgt in REDIRECTS.items():
        print(f"  {alias} → 302 → {tgt}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
