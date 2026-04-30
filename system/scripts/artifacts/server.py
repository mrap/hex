#!/usr/bin/env python3
"""Serve hex artifacts — static HTML files."""
import http.server
import os

PORT = 8897
DIR = os.path.dirname(os.path.abspath(__file__))

class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=DIR, **kwargs)
    def log_message(self, fmt, *args):
        pass

if __name__ == "__main__":
    http.server.ThreadingHTTPServer.allow_reuse_address = True
    server = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"Artifacts server on http://127.0.0.1:{PORT}", flush=True)
    server.serve_forever()
