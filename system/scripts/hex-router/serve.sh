#!/usr/bin/env bash
# Start the hex-router reverse proxy on 127.0.0.1:7000.
# Tailscale Serve fronts this on :443 so named paths like /ui, /boi, /visions work.

set -uo pipefail
cd "$(dirname "$0")"
export PORT="${PORT:-7000}"
exec python3 router.py
