#!/usr/bin/env python3
"""hex-asset-discover.py — Auto-discover hex assets and register them."""

import argparse
import importlib.util
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

AGENT_DIR = Path(os.environ.get("AGENT_DIR", Path.home() / "mrap-hex"))

# Import hex-asset module (filename has dash, so use importlib)
_scripts_dir = Path(__file__).parent
_spec_obj = importlib.util.spec_from_file_location("hex_asset", _scripts_dir / "hex-asset.py")
_hex_asset = importlib.util.module_from_spec(_spec_obj)
_spec_obj.loader.exec_module(_hex_asset)


def _now():
    return datetime.now(timezone.utc).isoformat()


def _make_asset(asset_id, title, path, url=None, owner="", metadata=None):
    asset_type, local_id = asset_id.split(":", 1)
    return {
        "id": asset_id,
        "type": asset_type,
        "local_id": local_id,
        "title": title,
        "path": path,
        "url": url,
        "owner": owner,
        "registered_at": _now(),
        "metadata": metadata or {},
    }


def discover_posts():
    """Parse projects/brand/pipeline.md for P-NNN posts."""
    pipeline = AGENT_DIR / "projects" / "brand" / "pipeline.md"
    if not pipeline.exists():
        return []

    lines = pipeline.read_text().splitlines()
    seen = {}

    i = 0
    while i < len(lines):
        m = re.match(r'^### (P-\d+): (.+)$', lines[i])
        if m:
            local_id = m.group(1)
            title = m.group(2).strip()
            status = ""
            platform = ""
            x_post_id = None

            for j in range(i + 1, min(i + 25, len(lines))):
                if re.match(r'^### ', lines[j]):
                    break
                sm = re.match(r'\*\*Status:\*\*\s*(.+)', lines[j])
                if sm:
                    status_line = sm.group(1).strip()
                    status = status_line.split()[0] if status_line else ""
                    pid_m = re.search(r'post ID (\d+)', status_line)
                    if pid_m:
                        x_post_id = pid_m.group(1)
                pm = re.match(r'\*\*Platform:\*\*\s*(.+)', lines[j])
                if pm:
                    platform = pm.group(1).strip()

            meta = {"platform": platform, "status": status}
            if x_post_id:
                meta["x_post_id"] = x_post_id

            seen[f"post:{local_id}"] = _make_asset(
                f"post:{local_id}",
                title,
                "projects/brand/pipeline.md",
                owner="brand",
                metadata=meta,
            )
        i += 1

    return list(seen.values())


def discover_proposals():
    """Scan projects/brand/proposals/*.html."""
    proposals_dir = AGENT_DIR / "projects" / "brand" / "proposals"
    if not proposals_dir.exists():
        return []

    assets = []
    for f in sorted(proposals_dir.glob("*.html")):
        stem = f.stem
        title = stem
        try:
            content = f.read_text(errors="replace")
            tm = re.search(r'<title>([^<]+)</title>', content, re.IGNORECASE)
            if tm:
                title = tm.group(1).strip()
        except Exception:
            pass

        assets.append(_make_asset(
            f"proposal:{stem}",
            title,
            f"projects/brand/proposals/{f.name}",
            url=f"https://mac-mini.tailbd5748.ts.net/proposals/{stem}",
            owner="brand",
        ))

    return assets


def discover_samples():
    """Scan projects/brand/proposals/samples/*.json."""
    samples_dir = AGENT_DIR / "projects" / "brand" / "proposals" / "samples"
    if not samples_dir.exists():
        return []

    assets = []
    for f in sorted(samples_dir.glob("*.json")):
        stem = f.stem
        title = stem
        try:
            data = json.loads(f.read_text())
            if isinstance(data, dict) and "title" in data:
                title = str(data["title"])
        except Exception:
            pass

        assets.append(_make_asset(
            f"sample:{stem}",
            title,
            f"projects/brand/proposals/samples/{f.name}",
            owner="brand",
        ))

    return assets


def discover_decisions():
    """Scan me/decisions/*.md and projects/*/decisions/*.md."""
    assets = []
    sources = [(AGENT_DIR / "me" / "decisions", "hex-ops")]

    projects_dir = AGENT_DIR / "projects"
    if projects_dir.exists():
        for proj_dir in sorted(projects_dir.iterdir()):
            d = proj_dir / "decisions"
            if d.is_dir():
                sources.append((d, proj_dir.name))

    for decisions_dir, default_owner in sources:
        if not decisions_dir.exists():
            continue
        for f in sorted(decisions_dir.glob("*.md")):
            stem = f.stem
            title = stem
            try:
                content = f.read_text()
                hm = re.search(r'^#\s+Decision:\s*(.+)$', content, re.MULTILINE)
                if hm:
                    title = hm.group(1).strip()
                else:
                    hm2 = re.search(r'^#\s+(.+)$', content, re.MULTILINE)
                    if hm2:
                        title = hm2.group(1).strip()
            except Exception:
                pass

            try:
                rel_path = str(f.relative_to(AGENT_DIR))
            except ValueError:
                rel_path = str(f)

            assets.append(_make_asset(
                f"decision:{stem}",
                title,
                rel_path,
                owner=default_owner,
            ))

    return assets


def discover_projects():
    """Scan projects/*/context.md."""
    projects_dir = AGENT_DIR / "projects"
    if not projects_dir.exists():
        return []

    assets = []
    for proj_dir in sorted(projects_dir.iterdir()):
        if not proj_dir.is_dir():
            continue
        ctx = proj_dir / "context.md"
        if not ctx.exists():
            continue

        folder = proj_dir.name
        title = folder
        try:
            content = ctx.read_text()
            hm = re.search(r'^#\s+(.+)$', content, re.MULTILINE)
            if hm:
                title = hm.group(1).strip()
        except Exception:
            pass

        assets.append(_make_asset(
            f"project:{folder}",
            title,
            f"projects/{folder}/context.md",
            owner=folder,
        ))

    return assets


def discover_experiments():
    """Parse pipeline.md for **Experiment:** EXP-NNN references."""
    pipeline = AGENT_DIR / "projects" / "brand" / "pipeline.md"
    if not pipeline.exists():
        return []

    exp_posts = {}
    current_post = None

    for line in pipeline.read_text().splitlines():
        pm = re.match(r'^### (P-\d+):', line)
        if pm:
            current_post = pm.group(1)

        em = re.search(r'\*\*Experiment:\*\*\s*(EXP-\d+)', line)
        if em:
            exp_id = em.group(1)
            exp_posts.setdefault(exp_id, [])
            if current_post and current_post not in exp_posts[exp_id]:
                exp_posts[exp_id].append(current_post)

    assets = []
    for exp_id, posts in sorted(exp_posts.items()):
        assets.append(_make_asset(
            f"experiment:{exp_id}",
            exp_id,
            "projects/brand/pipeline.md",
            owner="brand",
            metadata={"associated_posts": posts},
        ))

    return assets


DISCOVERERS = {
    "post": discover_posts,
    "proposal": discover_proposals,
    "sample": discover_samples,
    "decision": discover_decisions,
    "project": discover_projects,
    "experiment": discover_experiments,
}


def _upsert(data, asset):
    existing = next((a for a in data["assets"] if a["id"] == asset["id"]), None)
    if existing:
        existing.update(asset)
        return "updated"
    data["assets"].append(asset)
    return "new"


def main():
    parser = argparse.ArgumentParser(
        prog="hex-asset-discover",
        description="Auto-discover hex assets and register them in the registry",
    )
    parser.add_argument("--type", choices=list(DISCOVERERS.keys()), help="Scan only this asset type")
    parser.add_argument("--dry-run", action="store_true", help="Print what would be registered without writing")
    args = parser.parse_args()

    discoverers = {args.type: DISCOVERERS[args.type]} if args.type else DISCOVERERS

    all_assets = []
    for type_name, fn in discoverers.items():
        found = fn()
        print(f"  {type_name}: {len(found)} found")
        all_assets.extend(found)

    if args.dry_run:
        print(f"\n[dry-run] Discovered {len(all_assets)} assets (would register, not writing):")
        for a in all_assets:
            print(f"  {a['id']}  {a['title']}")
        return

    data = _hex_asset.load_registry()
    new_count = 0
    updated_count = 0

    for asset in all_assets:
        result = _upsert(data, asset)
        if result == "new":
            new_count += 1
            _hex_asset.emit_event("hex.asset.registered", asset["id"])
        else:
            updated_count += 1

    _hex_asset.save_registry(data)
    print(f"\nDiscovered {len(all_assets)} assets ({new_count} new, {updated_count} updated)")


if __name__ == "__main__":
    main()
