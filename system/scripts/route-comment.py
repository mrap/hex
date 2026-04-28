#!/usr/bin/env python3
"""
Comment router: classifies a new comment against agent charters and routes
to matching agents via hex-agent message.

Usage:
    python3 route-comment.py <comment_id> <asset> <text>
    python3 route-comment.py --help

Environment: same as route-message-llm.py (ROUTE_PROVIDER, OPENROUTER_API_KEY, etc.)
"""

import importlib.util
import json
import subprocess
import sys
import urllib.request
from pathlib import Path

COMMENTS_API = "http://127.0.0.1:8901/api/comments/update"
THRESHOLD = 0.4


def _load_router():
    """Load route-message-llm.py from the same directory."""
    here = Path(__file__).parent
    spec = importlib.util.spec_from_file_location("route_message_llm", here / "route-message-llm.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def update_comment(comment_id: str, status: str, action: str, routed_to: list[str]):
    payload = json.dumps({
        "id": comment_id,
        "status": status,
        "action": action,
        "routed_to": routed_to,
    }).encode()
    req = urllib.request.Request(
        COMMENTS_API,
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            pass
    except Exception as e:
        print(f"Warning: failed to update comment {comment_id}: {e}", file=sys.stderr)


def send_agent_message(agent_id: str, asset: str, text: str):
    hex_agent = str(Path(__file__).parent.parent / "bin" / "hex-agent")
    cmd = [
        hex_agent, "message", "hex-main", agent_id,
        "--subject", f"Comment on {asset}",
        "--body", text,
    ]
    try:
        subprocess.run(cmd, check=False, timeout=30)
    except FileNotFoundError:
        print(f"Warning: hex-agent not found — skipping message to {agent_id}", file=sys.stderr)
    except subprocess.TimeoutExpired:
        print(f"Warning: hex-agent timed out for {agent_id}", file=sys.stderr)


def main():
    args = sys.argv[1:]

    if not args or "--help" in args or "-h" in args:
        print(__doc__.strip())
        sys.exit(0)

    if len(args) < 3:
        print("Usage: route-comment.py <comment_id> <asset> <text>", file=sys.stderr)
        sys.exit(2)

    comment_id, asset, text = args[0], args[1], " ".join(args[2:])
    message = f"Comment on {asset}: {text}"

    router = _load_router()
    result = router.route(message, threshold=THRESHOLD)
    matches = result.get("matches", [])

    if matches:
        agent_ids = [m["agent_id"] for m in matches]
        print(f"Matched agents: {', '.join(agent_ids)}")
        for agent_id in agent_ids:
            send_agent_message(agent_id, asset, text)
        action = f"Routed to: {', '.join(agent_ids)}"
        update_comment(comment_id, "seen", action, agent_ids)
    else:
        print("No agent match — updating to general inbox")
        update_comment(comment_id, "seen", "No agent match — general inbox", [])

    sys.exit(0)


if __name__ == "__main__":
    main()
