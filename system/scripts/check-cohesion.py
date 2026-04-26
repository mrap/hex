#!/usr/bin/env python3
"""
check-cohesion.py — verifies that work traces to active initiatives.

Usage:
  python3 check-cohesion.py --spec <file>   # check one spec
  python3 check-cohesion.py --all            # check all active specs
  python3 check-cohesion.py --map            # show initiative coverage map
"""

import os
import sys
import re
import json
import subprocess
import argparse
from pathlib import Path
from typing import Optional

# --- Paths ---
HEX_ROOT = Path(os.environ.get("HEX_ROOT", Path(os.environ.get("AGENT_DIR", str(Path.home() / "hex")))))
INITIATIVES_DIR = HEX_ROOT / "initiatives"
EXPERIMENTS_DIR = HEX_ROOT / "experiments"
BOI_QUEUE_DIR = Path.home() / ".boi" / "queue"


# --- YAML minimal parser (stdlib only) ---

def parse_yaml_simple(text: str) -> dict:
    """
    Very simple YAML key-value parser — handles top-level scalar keys,
    list items (- value), and nested dicts (key: value under an indented block).
    Good enough for hex initiative/experiment files.
    """
    result = {}
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            i += 1
            continue
        # Top-level key: value
        m = re.match(r'^(\w[\w_-]*):\s*(.*)', line)
        if m:
            key = m.group(1)
            val = m.group(2).strip()
            if val == "" or val == "|" or val == ">":
                # Block scalar or block sequence — collect nested
                i += 1
                children = []
                while i < len(lines):
                    child = lines[i]
                    if child and not child[0].isspace() and not child.startswith(" "):
                        break
                    children.append(child)
                    i += 1
                # Try to parse children as list items
                items = []
                child_dict = {}
                for c in children:
                    lm = re.match(r'\s+-\s+(.*)', c)
                    if lm:
                        items.append(lm.group(1).strip())
                    else:
                        km = re.match(r'\s+(\w[\w_-]*):\s*(.*)', c)
                        if km:
                            child_dict[km.group(1)] = km.group(2).strip()
                if items:
                    result[key] = items
                elif child_dict:
                    result[key] = child_dict
                else:
                    result[key] = " ".join(children).strip()
                continue
            else:
                # Strip quotes
                if (val.startswith('"') and val.endswith('"')) or \
                   (val.startswith("'") and val.endswith("'")):
                    val = val[1:-1]
                result[key] = val
        i += 1
    return result


def load_yaml_file(path: Path) -> dict:
    try:
        text = path.read_text()
        return parse_yaml_simple(text)
    except Exception:
        return {}


# --- Initiative loading ---

def load_initiatives() -> list[dict]:
    initiatives = []
    if not INITIATIVES_DIR.exists():
        return initiatives
    for f in sorted(INITIATIVES_DIR.glob("*.yaml")):
        data = load_yaml_file(f)
        data["_file"] = f
        initiatives.append(data)
    return initiatives


def load_experiments() -> list[dict]:
    exps = []
    if not EXPERIMENTS_DIR.exists():
        return exps
    for f in sorted(EXPERIMENTS_DIR.glob("*.yaml")):
        data = load_yaml_file(f)
        data["_file"] = f
        exps.append(data)
    return exps


# --- Spec loading ---

def load_boi_specs_active() -> list[dict]:
    """Load active specs from boi status --json, or fall back to scanning queue dir."""
    specs = []

    # Try boi status --json
    try:
        result = subprocess.run(
            ["boi", "status", "--json"],
            capture_output=True, text=True, timeout=10
        )
        if result.returncode == 0:
            data = json.loads(result.stdout)
            for entry in data.get("entries", []):
                if entry.get("status") in ("queued", "running", "paused"):
                    specs.append({
                        "id": entry.get("id", "?"),
                        "spec_path": entry.get("spec_path", ""),
                        "status": entry.get("status", ""),
                        "_source": "boi-status",
                    })
            return specs
    except Exception:
        pass

    # Fallback: scan queue dir for spec files with at least one PENDING task
    if BOI_QUEUE_DIR.exists():
        for f in sorted(BOI_QUEUE_DIR.glob("q-*.spec.md")):
            text = f.read_text()
            if "PENDING" in text:
                qid = f.stem.replace(".spec", "")
                specs.append({
                    "id": qid,
                    "spec_path": str(f),
                    "status": "queued",
                    "_source": "scan",
                })

    return specs


def read_spec_text(spec_path: str) -> str:
    try:
        return Path(spec_path).read_text()
    except Exception:
        return ""


# --- Initiative link detection in spec text ---

INITIATIVE_PATTERNS = [
    re.compile(r'^\*?\*?[Ii]nitiative\*?\*?:\s*(.+)', re.MULTILINE),
    re.compile(r'^\*?\*?[Dd]rives\*?\*?:\s*(.+)', re.MULTILINE),
    re.compile(r'^##\s+[Ii]nitiative\b.*', re.MULTILINE),
    re.compile(r'\b(init-[\w-]+)\b'),
]

def detect_initiative_link(text: str) -> Optional[str]:
    """Return the first initiative reference found, or None."""
    for pat in INITIATIVE_PATTERNS:
        m = pat.search(text)
        if m:
            # Return the captured group if available, else the full match
            if m.lastindex and m.lastindex >= 1:
                return m.group(1).strip()
            return m.group(0).strip()
    return None


# --- KR keyword overlap ---

def kr_keyword_overlap(spec_text: str, kr_desc: str) -> bool:
    """Very soft check: do any significant words from kr_desc appear in spec_text?"""
    stopwords = {"a", "an", "the", "of", "in", "for", "to", "and", "or", "is",
                 "are", "be", "with", "that", "this", "at", "by", "from", "on",
                 "as", "its", "it", "has", "have", "not", "no", "per", "than",
                 "into", "each", "any", "all", "zero", ">=", "<=", "target"}
    words = {w.lower() for w in re.findall(r'\b[a-zA-Z]{4,}\b', kr_desc)
             if w.lower() not in stopwords}
    spec_lower = spec_text.lower()
    matched = [w for w in words if w in spec_lower]
    return len(matched) >= max(1, len(words) // 3)


# --- Check one spec ---

def check_spec(spec_path: str, spec_id: Optional[str] = None) -> dict:
    """
    Returns a dict with:
      initiative_link: str or None
      initiative_exists: bool
      initiative_active: bool
      kr_match: bool
      issues: list[str]
    """
    text = read_spec_text(spec_path)
    issues = []
    result = {
        "id": spec_id or Path(spec_path).stem,
        "path": spec_path,
        "initiative_link": None,
        "initiative_exists": False,
        "initiative_active": False,
        "kr_match": False,
        "issues": issues,
    }

    # 1. SPEC ALIGNMENT
    link = detect_initiative_link(text)
    result["initiative_link"] = link
    if not link:
        issues.append("No initiative link found (missing initiative:, drives:, or ## Initiative section)")
        return result

    # 2. INITIATIVE EXISTENCE
    initiative_file = None
    for f in INITIATIVES_DIR.glob("*.yaml"):
        if link in f.read_text() or link in f.name:
            initiative_file = f
            break

    if not initiative_file:
        issues.append(f"Initiative '{link}' not found in initiatives/")
        return result

    result["initiative_exists"] = True
    init_data = load_yaml_file(initiative_file)
    status = init_data.get("status", "")
    if status != "active":
        issues.append(f"Initiative '{link}' status is '{status}', not active")
    else:
        result["initiative_active"] = True

    # 3. KR TRACEABILITY
    krs = []
    init_text = initiative_file.read_text()
    kr_blocks = re.findall(r'description:\s*["\']?([^"\'\n]+)["\']?', init_text)
    for desc in kr_blocks:
        if kr_keyword_overlap(text, desc):
            krs.append(desc)

    if krs:
        result["kr_match"] = True
    else:
        issues.append("No KR keyword overlap found between spec tasks and initiative KRs (soft check)")

    return result


# --- --spec mode ---

def cmd_spec(args):
    spec_path = args.spec
    if not Path(spec_path).exists():
        print(f"ERROR: spec file not found: {spec_path}")
        sys.exit(1)

    r = check_spec(spec_path)
    print(f"SPEC: {spec_path}")
    print(f"  Initiative link : {r['initiative_link'] or '(none)'}")
    print(f"  Initiative exists: {'yes' if r['initiative_exists'] else 'no'}")
    print(f"  Initiative active: {'yes' if r['initiative_active'] else 'no'}")
    print(f"  KR keyword match : {'yes' if r['kr_match'] else 'no (soft check)'}")
    if r["issues"]:
        print(f"  ISSUES ({len(r['issues'])}):")
        for issue in r["issues"]:
            print(f"    - {issue}")
        print("VERDICT: FAIL")
        sys.exit(1)
    else:
        print("VERDICT: PASS")


# --- --all mode ---

def cmd_all(args):
    specs = load_boi_specs_active()
    if not specs:
        print("No active specs found.")
        return

    orphans = []
    issues_found = []

    for s in specs:
        r = check_spec(s["spec_path"], s["id"])
        if not r["initiative_link"]:
            orphans.append(s["id"])
        if r["issues"]:
            issues_found.append((s["id"], r["issues"]))

    print(f"COHESION CHECK — {len(specs)} active specs")
    print()

    if orphans:
        print(f"ORPHAN SPECS ({len(orphans)}, no initiative link):")
        for qid in orphans:
            print(f"  {qid}")
    else:
        print("ORPHAN SPECS: none")

    if issues_found:
        print()
        print(f"SPECS WITH ISSUES:")
        for qid, issues in issues_found:
            print(f"  {qid}:")
            for iss in issues:
                print(f"    - {iss}")
        sys.exit(1)
    else:
        print()
        print("All specs have valid initiative links.")


# --- --map mode ---

def cmd_map(args):
    initiatives = load_initiatives()
    experiments = load_experiments()
    active_specs = load_boi_specs_active()

    # Index experiments by initiative reference
    exp_by_init: dict[str, list] = {}
    for exp in experiments:
        exp_id = exp.get("id", str(exp["_file"].stem))
        exp_text = exp["_file"].read_text()
        for init in initiatives:
            init_id = init.get("id", "")
            if init_id and init_id in exp_text:
                exp_by_init.setdefault(init_id, []).append((exp_id, exp.get("state", "?")))

    # Index active specs by initiative reference
    spec_by_init: dict[str, list] = {}
    orphan_specs = []
    for s in active_specs:
        text = read_spec_text(s["spec_path"])
        link = detect_initiative_link(text)
        if link:
            spec_by_init.setdefault(link, []).append((s["id"], s["status"]))
        else:
            orphan_specs.append(s["id"])

    print("INITIATIVE MAP")
    print()

    for init in initiatives:
        init_id = init.get("id", "?")
        init_status = init.get("status", "?")
        horizon = init.get("horizon", "?")
        print(f"{init_id} ({init_status}, horizon {horizon})")

        # KRs
        init_text = init["_file"].read_text()
        kr_blocks = re.finditer(
            r'- id:\s*(kr-\d+)\s*\n\s*description:\s*["\']?([^"\'\n]+)',
            init_text
        )

        has_active_work = False
        kr_list = list(kr_blocks)

        if not kr_list:
            print("  (no KRs defined)")
        else:
            for kr_m in kr_list:
                kr_id = kr_m.group(1)
                kr_desc = kr_m.group(2).strip().strip('"').strip("'")

                # Find experiments that mention this KR
                kr_exps = []
                for exp in experiments:
                    exp_text = exp["_file"].read_text()
                    if kr_id in exp_text and init_id in exp_text:
                        state = exp.get("state", "?")
                        kr_exps.append(f"{exp.get('id', '?')} ({state})")

                # Find specs that mention this KR
                kr_specs = []
                for s in active_specs:
                    spec_text = read_spec_text(s["spec_path"])
                    if kr_id in spec_text and init_id in spec_text:
                        kr_specs.append(f"{s['id']} ({s['status']})")

                if kr_exps or kr_specs:
                    has_active_work = True
                    work = ", ".join(kr_exps + kr_specs)
                    print(f"  {kr_id}: {kr_desc[:60]} — {work}")
                else:
                    print(f"  {kr_id}: {kr_desc[:60]} — NO ACTIVE WORK")

        # Check init-level experiments
        init_exps = exp_by_init.get(init_id, [])
        if init_exps:
            has_active_work = True

        # Check init-level specs
        init_specs = spec_by_init.get(init_id, [])
        if init_specs:
            has_active_work = True

        if not has_active_work:
            print(f"  WARNING: no active specs or experiments for this initiative")

        print()

    if orphan_specs:
        print("ORPHAN SPECS (no initiative link):")
        for s in active_specs:
            if s["id"] in orphan_specs:
                title = _spec_title(s["spec_path"])
                print(f"  {s['id']}: {title}")
    else:
        print("ORPHAN SPECS: none")


def _spec_title(spec_path: str) -> str:
    try:
        text = Path(spec_path).read_text()
        m = re.search(r'^#\s+(.+)', text, re.MULTILINE)
        if m:
            return m.group(1).strip()[:60]
    except Exception:
        pass
    return Path(spec_path).stem


# --- Main ---

def main():
    parser = argparse.ArgumentParser(
        description="check-cohesion: verify that work traces to active initiatives"
    )
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--spec", metavar="FILE", help="Check a single spec file")
    group.add_argument("--all", action="store_true", help="Check all active specs")
    group.add_argument("--map", action="store_true", help="Show initiative coverage map")

    args = parser.parse_args()

    if args.spec:
        cmd_spec(args)
    elif args.all:
        cmd_all(args)
    elif args.map:
        cmd_map(args)


if __name__ == "__main__":
    main()
