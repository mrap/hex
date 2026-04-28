#!/usr/bin/env python3
"""hex-asset.py — Asset registry CLI for hex."""

import argparse
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

AGENT_DIR = Path(os.environ.get("AGENT_DIR", Path.home() / "mrap-hex"))
REGISTRY = AGENT_DIR / ".hex" / "data" / "assets.json"
EVENTS_LOG = AGENT_DIR / ".hex" / "logs" / "hex-events.log"

VALID_TYPES = {"post", "proposal", "sample", "spec", "experiment", "decision", "demo", "project"}


def load_registry():
    if not REGISTRY.exists():
        return {"version": 1, "assets": []}
    with open(REGISTRY) as f:
        return json.load(f)


def save_registry(data):
    REGISTRY.parent.mkdir(parents=True, exist_ok=True)
    tmp = str(REGISTRY) + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    os.replace(tmp, REGISTRY)


def emit_event(event_type, asset_id):
    try:
        EVENTS_LOG.parent.mkdir(parents=True, exist_ok=True)
        ts = datetime.now(timezone.utc).isoformat()
        with open(EVENTS_LOG, "a") as f:
            f.write(json.dumps({"event": event_type, "asset_id": asset_id, "ts": ts}) + "\n")
    except Exception:
        pass


def cmd_register(args):
    asset_id = args.id
    if ":" not in asset_id:
        print(f"Error: id must be type:local_id, got '{asset_id}'", file=sys.stderr)
        sys.exit(1)
    asset_type, local_id = asset_id.split(":", 1)
    if asset_type not in VALID_TYPES:
        print(f"Warning: unknown type '{asset_type}' (valid: {', '.join(sorted(VALID_TYPES))})", file=sys.stderr)

    meta = {}
    for kv in (args.meta or []):
        if "=" not in kv:
            print(f"Error: --meta must be key=value, got '{kv}'", file=sys.stderr)
            sys.exit(1)
        k, v = kv.split("=", 1)
        meta[k] = v

    data = load_registry()
    existing = next((a for a in data["assets"] if a["id"] == asset_id), None)
    is_new = existing is None

    now = datetime.now(timezone.utc).isoformat()
    if existing:
        if args.title is not None:
            existing["title"] = args.title
        if args.path is not None:
            existing["path"] = args.path
        if args.url is not None:
            existing["url"] = args.url
        if args.owner is not None:
            existing["owner"] = args.owner
        if meta:
            existing["metadata"].update(meta)
        existing["registered_at"] = now
    else:
        data["assets"].append({
            "id": asset_id,
            "type": asset_type,
            "local_id": local_id,
            "title": args.title or "",
            "path": args.path or "",
            "url": args.url,
            "owner": args.owner or "",
            "registered_at": now,
            "metadata": meta,
        })

    save_registry(data)
    if is_new:
        emit_event("hex.asset.registered", asset_id)
        print(f"Registered: {asset_id}")
    else:
        print(f"Updated: {asset_id}")


def cmd_resolve(args):
    asset_id = args.id
    data = load_registry()
    asset = next((a for a in data["assets"] if a["id"] == asset_id), None)
    if not asset:
        print(f"Not found: {asset_id}", file=sys.stderr)
        sys.exit(1)
    out = dict(asset)
    if out.get("path"):
        out["path_absolute"] = str(AGENT_DIR / out["path"])
    print(json.dumps(out, indent=2))


def cmd_list(args):
    data = load_registry()
    assets = data["assets"]
    if args.type:
        assets = [a for a in assets if a["type"] == args.type]
    if args.owner:
        assets = [a for a in assets if a.get("owner") == args.owner]
    if args.count:
        print(f"{len(assets)} assets")
        return
    for a in assets:
        print(f"{a['id']}  {a.get('title', '')}  {a.get('path', '')}")


def cmd_search(args):
    q = args.query.lower()
    data = load_registry()
    for a in data["assets"]:
        if (q in a["id"].lower() or
                q in a.get("title", "").lower() or
                q in a.get("path", "").lower()):
            print(f"{a['id']}  {a.get('title', '')}  {a.get('path', '')}")


def cmd_remove(args):
    asset_id = args.id
    data = load_registry()
    before = len(data["assets"])
    data["assets"] = [a for a in data["assets"] if a["id"] != asset_id]
    if len(data["assets"]) == before:
        print(f"Not found: {asset_id}", file=sys.stderr)
        sys.exit(1)
    save_registry(data)
    print(f"Removed: {asset_id}")


def cmd_types(args):
    data = load_registry()
    counts = {}
    for a in data["assets"]:
        counts[a["type"]] = counts.get(a["type"], 0) + 1
    if not counts:
        print("No assets registered.")
        return
    for t in sorted(counts):
        print(f"{t}: {counts[t]}")


def main():
    parser = argparse.ArgumentParser(prog="hex-asset", description="Hex asset registry CLI")
    sub = parser.add_subparsers(dest="cmd")

    # register
    p_reg = sub.add_parser("register", help="Register or update an asset")
    p_reg.add_argument("id", help="type:local_id")
    p_reg.add_argument("--title", help="Asset title")
    p_reg.add_argument("--path", help="Path relative to AGENT_DIR")
    p_reg.add_argument("--url", help="Optional URL")
    p_reg.add_argument("--owner", help="Owner agent ID")
    p_reg.add_argument("--meta", nargs="*", metavar="key=value", help="Metadata key=value pairs")

    # resolve
    p_res = sub.add_parser("resolve", help="Resolve an asset to its metadata")
    p_res.add_argument("id", help="type:local_id")

    # list
    p_list = sub.add_parser("list", help="List assets")
    p_list.add_argument("--type", help="Filter by type")
    p_list.add_argument("--owner", help="Filter by owner")
    p_list.add_argument("--count", action="store_true", help="Print count only")

    # search
    p_search = sub.add_parser("search", help="Search assets by substring")
    p_search.add_argument("query", help="Search query")

    # remove
    p_rm = sub.add_parser("remove", help="Remove an asset")
    p_rm.add_argument("id", help="type:local_id")

    # types
    sub.add_parser("types", help="List types with counts")

    args = parser.parse_args()
    if not args.cmd:
        parser.print_help()
        sys.exit(0)

    dispatch = {
        "register": cmd_register,
        "resolve": cmd_resolve,
        "list": cmd_list,
        "search": cmd_search,
        "remove": cmd_remove,
        "types": cmd_types,
    }
    dispatch[args.cmd](args)


if __name__ == "__main__":
    main()
