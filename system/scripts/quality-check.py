#!/usr/bin/env python3
"""Quality Antagonist — gaming detector for the BOI initiative loop.

Usage:
  python3 quality-check.py --spec q-774
  python3 quality-check.py --sweep
  python3 quality-check.py --kr init-closed-loop-telemetry/kr-1
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone, timedelta
from pathlib import Path
from typing import Optional

BOI_QUEUE = Path(os.path.expanduser("~/.boi/queue"))
WORKSPACE = Path(os.path.expanduser("~/hex"))
INITIATIVES_DIR = WORKSPACE / "initiatives"
EVENTS_DIR = Path(os.path.expanduser("~/.hex-events/events"))
GITHUB_MRAP_BASE = Path(os.path.expanduser("~/github.com/mrap"))

# --- Gaming detection patterns ---

TRIVIAL_METRIC_PATTERNS = [
    re.compile(r'^\s*echo\s+[\d.]+\s*$'),           # echo <constant>
    re.compile(r'echo\s+"?UNMEASURABLE', re.I),      # echo "UNMEASURABLE..."
    re.compile(r'exit\s+1'),                          # bare exit 1
    re.compile(r'^\s*echo\s+0\s*$'),                  # echo 0
    re.compile(r'^\s*echo\s+1\s*$'),                  # echo 1
    re.compile(r'^\s*echo\s+100\s*$'),                # echo 100
]

FILE_EXISTENCE_ONLY_PATTERN = re.compile(
    r'os\.path\.exists|test\s+-[ef]|if.*exists', re.I
)

ADMIN_TITLE_KEYWORDS = [
    "close", "add closed_at", "update status", "mark complete",
    "kr closure", "close kr", "initiative close", "admin", "housekeeping",
]

MANUAL_VERIFICATION_PATTERN = re.compile(
    r'manual.verif|manual.check|echo.*manual', re.I
)


def is_trivially_gameable(cmd: str) -> tuple[bool, str]:
    """Return (is_gamed, reason)."""
    if not cmd:
        return False, ""
    cmd_stripped = cmd.strip()
    for pat in TRIVIAL_METRIC_PATTERNS:
        if pat.search(cmd_stripped):
            return True, f"constant/trivial metric command: {cmd_stripped[:80]!r}"
    if MANUAL_VERIFICATION_PATTERN.search(cmd_stripped):
        return True, "manual verification placeholder — not a runnable metric"
    return False, ""


def is_file_existence_proxy(cmd: str) -> bool:
    lines = [l.strip() for l in cmd.splitlines() if l.strip() and not l.strip().startswith('#')]
    non_trivial = [l for l in lines if not l.startswith('score') and 'print' not in l and 'if' not in l]
    return bool(FILE_EXISTENCE_ONLY_PATTERN.search(cmd)) and len(lines) < 15


def kr_lower_better_math_error(kr: dict) -> bool:
    """Detect: lower_is_better but current > target → cannot be met."""
    direction = kr.get("metric", {}).get("direction", "higher_is_better")
    if direction != "lower_is_better":
        return False
    current = kr.get("current")
    target = kr.get("target")
    status = kr.get("status", "open")
    if status != "met":
        return False
    try:
        return float(current) > float(target)
    except (TypeError, ValueError):
        return False


# --- Spec file parsing ---

def read_spec(spec_id: str) -> Optional[dict]:
    """Read and parse a spec file."""
    spec_path = BOI_QUEUE / f"{spec_id}.spec.md"
    if not spec_path.exists():
        return None
    content = spec_path.read_text()
    return {
        "id": spec_id,
        "content": content,
        "path": str(spec_path),
        "mtime": spec_path.stat().st_mtime,
    }


def read_telemetry(spec_id: str) -> Optional[dict]:
    tele_path = BOI_QUEUE / f"{spec_id}.telemetry.json"
    if tele_path.exists():
        try:
            return json.loads(tele_path.read_text())
        except Exception:
            pass
    return None


def spec_is_drive_kr(title: str) -> bool:
    return bool(re.search(r'Drive KR to Non-Zero|drive.*kr.*non-zero|highest-leverage action for kr', title, re.I))


def extract_metric_command_from_spec(content: str) -> Optional[str]:
    """Extract the metric command embedded in a spec (pre-run value)."""
    m = re.search(r'Metric command:\s*```\s*\n(.*?)\n```', content, re.DOTALL)
    if m:
        return m.group(1).strip()
    return None


def get_verify_command(content: str) -> Optional[str]:
    """Extract the verify command from a spec."""
    m = re.search(r'\*\*Verify:\*\*\s*`([^`]+)`', content)
    if m:
        return m.group(1)
    m = re.search(r'\*\*Verify:\*\*\s*(.*?)(?:\n\n|\Z)', content, re.DOTALL)
    if m:
        return m.group(1).strip()
    return None


def get_spec_initiative(content: str) -> Optional[str]:
    """Extract initiative ID from spec content."""
    m = re.search(r'\*\*Initiative:\*\*\s*(\S+)', content)
    if m:
        return m.group(1)
    m = re.search(r'initiative[:\s]+(\S+)', content, re.I)
    if m:
        return m.group(1).rstrip('/')
    return None


def get_spec_kr(content: str) -> Optional[str]:
    """Extract KR ID from spec content."""
    m = re.search(r'\b(kr-\d+)\b', content, re.I)
    if m:
        return m.group(1).lower()
    return None


def get_exit_time(spec_id: str) -> Optional[float]:
    """Get the mtime of the .exit file (completion time)."""
    exit_path = BOI_QUEUE / f"{spec_id}.exit"
    if exit_path.exists():
        return exit_path.stat().st_mtime
    return None


def get_dispatch_commit(dispatch_time: float, repo_path: Path) -> Optional[str]:
    """Return the git commit hash that was HEAD at dispatch_time (snapshot before spec ran)."""
    try:
        ts = datetime.fromtimestamp(dispatch_time, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        result = subprocess.run(
            ["git", "log", f"--before={ts}", "-1", "--format=%H"],
            cwd=str(repo_path), capture_output=True, text=True, timeout=10
        )
        commit = result.stdout.strip()
        return commit if commit else None
    except Exception:
        return None


def get_untracked_files(repo_path: Path, start_time: Optional[float], exit_time: Optional[float]) -> list[dict]:
    """Detect untracked files modified within the spec execution window (git status --short)."""
    try:
        result = subprocess.run(
            ["git", "status", "--short"],
            cwd=str(repo_path), capture_output=True, text=True, timeout=10
        )
        untracked = []
        for line in result.stdout.splitlines():
            if not line.startswith("?? "):
                continue
            rel_path = line[3:].strip()
            fpath = repo_path / rel_path
            if not fpath.exists() or fpath.is_dir():
                continue
            fmtime = fpath.stat().st_mtime
            if start_time and exit_time:
                window_start = start_time - 60
                window_end = exit_time + 1800
                if not (window_start <= fmtime <= window_end):
                    continue
            untracked.append({"type": "untracked", "file": rel_path, "source": "untracked"})
        return untracked
    except Exception:
        return []


def extract_repos_from_spec(content: str) -> list[Path]:
    """Extract additional git repo paths referenced in spec content or workspace field."""
    repos: list[Path] = []
    seen: set[Path] = set()

    # workspace: field
    m = re.search(r'^workspace:\s*(.+)$', content, re.MULTILINE)
    if m:
        wp = Path(os.path.expanduser(m.group(1).strip()))
        if wp != WORKSPACE and wp not in seen:
            repos.append(wp)
            seen.add(wp)

    # github.com/mrap/<repo> paths anywhere in spec
    for match in re.finditer(r'github\.com/mrap/([a-zA-Z0-9_.-]+)', content):
        repo_name = match.group(1).rstrip('/.,:;')
        rp = GITHUB_MRAP_BASE / repo_name
        if rp not in seen:
            repos.append(rp)
            seen.add(rp)

    # mrap/<repo-name> relative references
    for match in re.finditer(r'\bmrap/([a-zA-Z0-9_-]+)', content):
        rp = GITHUB_MRAP_BASE / match.group(1)
        if rp not in seen:
            repos.append(rp)
            seen.add(rp)

    return [r for r in repos if r.exists() and (r / '.git').exists()]


def scan_github_mrap_repos_in_window(start_time: float, exit_time: Optional[float]) -> list[Path]:
    """Return ~/github.com/mrap/ repos that had commits during the spec execution window."""
    if not GITHUB_MRAP_BASE.exists():
        return []
    since_ts = datetime.fromtimestamp(start_time - 60, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    until_val = exit_time + 1800 if exit_time else start_time + 7200
    until_ts = datetime.fromtimestamp(until_val, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    active: list[Path] = []
    try:
        for repo_dir in GITHUB_MRAP_BASE.iterdir():
            if not repo_dir.is_dir() or not (repo_dir / '.git').exists():
                continue
            try:
                r = subprocess.run(
                    ["git", "log", f"--since={since_ts}", f"--until={until_ts}", "--oneline"],
                    cwd=str(repo_dir), capture_output=True, text=True, timeout=10,
                )
                if r.stdout.strip():
                    active.append(repo_dir)
            except Exception:
                continue
    except Exception:
        pass
    return active


def scan_repo_for_changes(
    repo_path: Path,
    start_time: Optional[float],
    exit_time: Optional[float],
) -> dict:
    """Scan a single repo for code/doc/untracked changes within the spec execution window.

    Returns dict with keys: code_changes (list[dict]), evidence (list[str]).
    """
    label = repo_path.name
    out: dict = {"code_changes": [], "evidence": []}
    try:
        dispatch_commit = get_dispatch_commit(start_time, repo_path) if start_time else None
        git_diff_ref = f"{dispatch_commit}..HEAD" if dispatch_commit else "HEAD"
        if not dispatch_commit:
            out["evidence"].append(f"[{label}] dispatch-time commit unavailable, using HEAD diff")

        diff_r = subprocess.run(
            ["git", "diff", "--name-only", git_diff_ref],
            cwd=str(repo_path), capture_output=True, text=True, timeout=10,
        )
        all_changed = [f.strip() for f in diff_r.stdout.splitlines() if f.strip()]

        files_in_window: list[str] = []
        for f in all_changed:
            fpath = repo_path / f
            if not fpath.exists():
                continue
            fmtime = fpath.stat().st_mtime
            if start_time and exit_time:
                if (start_time - 60) <= fmtime <= (exit_time + 1800):
                    files_in_window.append(f)
            else:
                files_in_window.append(f)

        code_files = [f for f in files_in_window if f.endswith(('.py', '.rs', '.sh', '.js', '.ts'))]
        doc_files = [f for f in files_in_window if f.endswith('.md')]

        if code_files:
            out["evidence"].append(f"[{label}] code files changed: {code_files}")
            out["code_changes"].extend([{"type": "code", "file": f, "repo": label} for f in code_files])
        if doc_files:
            out["evidence"].append(f"[{label}] doc files changed: {doc_files}")
            out["code_changes"].extend([{"type": "doc", "file": f, "repo": label} for f in doc_files])

        untracked = get_untracked_files(repo_path, start_time, exit_time)
        if untracked:
            for u in untracked:
                u["repo"] = label
            out["evidence"].append(f"[{label}] untracked files: {[u['file'] for u in untracked]}")
            out["code_changes"].extend(untracked)

    except subprocess.TimeoutExpired:
        out["evidence"].append(f"[{label}] git diff timed out")
    except Exception as e:
        out["evidence"].append(f"[{label}] scan error: {e}")
    return out


def get_spec_duration_seconds(spec_id: str) -> Optional[float]:
    """Estimate duration from telemetry or file timestamps."""
    tele = read_telemetry(spec_id)
    if tele:
        total = tele.get("total_time_seconds")
        if total and total > 0:
            return total
    # Fall back to mtime difference between prompt and exit
    prompt_path = BOI_QUEUE / f"{spec_id}.prompt.md"
    exit_path = BOI_QUEUE / f"{spec_id}.exit"
    if prompt_path.exists() and exit_path.exists():
        return exit_path.stat().st_mtime - prompt_path.stat().st_mtime
    return None


# --- KR reading ---

def read_initiative(init_id: str) -> Optional[dict]:
    """Read an initiative YAML file."""
    # Normalize: strip 'init-' prefix if used as filename
    name = init_id.replace("init-", "")
    for candidate in [init_id, name, f"init-{name}"]:
        path = INITIATIVES_DIR / f"{candidate}.yaml"
        if path.exists():
            try:
                import re as _re
                content = path.read_text()
                # Parse YAML manually (stdlib only)
                return {"_raw": content, "_path": str(path), "_id": init_id}
            except Exception:
                pass
    return None


def parse_initiative_yaml(raw: str) -> dict:
    """Minimal YAML parser for initiative files (stdlib only)."""
    try:
        # Try stdlib yaml-like parsing via json conversion hack
        # Since we can't import yaml, use a line-based parser
        result = {}
        lines = raw.splitlines()
        current_kr = None
        key_results = []
        in_krs = False
        in_metric = False
        metric_lines = []
        metric_indent = 0
        i = 0
        while i < len(lines):
            line = lines[i]
            stripped = line.strip()
            indent = len(line) - len(line.lstrip())

            if stripped.startswith("id:") and not in_krs:
                result["id"] = stripped[3:].strip().strip("'\"")
            elif stripped.startswith("status:") and not in_krs and not current_kr:
                result["status"] = stripped[7:].strip().strip("'\"")
            elif stripped == "key_results:":
                in_krs = True
            elif in_krs and stripped.startswith("- id:"):
                if current_kr:
                    key_results.append(current_kr)
                current_kr = {"id": stripped[5:].strip().strip("'\""), "metric": {}}
            elif in_krs and current_kr and stripped.startswith("description:"):
                current_kr["description"] = stripped[12:].strip().strip("'\"")
            elif in_krs and current_kr and stripped.startswith("target:"):
                try:
                    current_kr["target"] = float(stripped[7:].strip())
                except ValueError:
                    current_kr["target"] = stripped[7:].strip()
            elif in_krs and current_kr and stripped.startswith("current:"):
                try:
                    current_kr["current"] = float(stripped[8:].strip())
                except ValueError:
                    current_kr["current"] = stripped[8:].strip()
            elif in_krs and current_kr and stripped.startswith("status:"):
                current_kr["status"] = stripped[7:].strip().strip("'\"")
            elif in_krs and current_kr and stripped == "metric:":
                in_metric = True
                metric_indent = indent
                current_kr["metric"] = {}
            elif in_metric and stripped.startswith("command:"):
                # Collect multi-line command
                cmd_start = stripped[8:].strip()
                if cmd_start.startswith("'") or cmd_start.startswith('"'):
                    # Might be multi-line
                    cmd = cmd_start.lstrip("'\"")
                    current_kr["metric"]["command"] = cmd
                else:
                    current_kr["metric"]["command"] = cmd_start
            elif in_metric and stripped.startswith("direction:"):
                current_kr["metric"]["direction"] = stripped[10:].strip().strip("'\"")
                in_metric = False
            i += 1

        if current_kr:
            key_results.append(current_kr)
        result["key_results"] = key_results
        return result
    except Exception as e:
        return {"_parse_error": str(e), "key_results": []}


def find_kr(init_id: str, kr_id: str) -> Optional[dict]:
    """Load a specific KR from an initiative file."""
    init_data = read_initiative(init_id)
    if not init_data:
        return None
    parsed = parse_initiative_yaml(init_data["_raw"])
    for kr in parsed.get("key_results", []):
        if kr.get("id") == kr_id:
            kr["_initiative_id"] = init_id
            kr["_raw_command"] = ""
            # Extract full metric command from raw YAML
            raw = init_data["_raw"]
            # Find the command block for this KR
            kr_block_start = raw.find(f"id: {kr_id}")
            if kr_block_start >= 0:
                block = raw[kr_block_start:]
                # Find command field
                cmd_match = re.search(r'command:\s*(.*?)(?:\n\s+direction:|\n\s*target:|\n\s*current:|\Z)', block, re.DOTALL)
                if cmd_match:
                    cmd_raw = cmd_match.group(1).strip().strip("'\"")
                    kr["_raw_command"] = cmd_raw
            return kr
    return None


# --- Admin spec classification ---

def parse_spec_metadata(content: str) -> dict:
    """Extract title, mode, and context from spec YAML frontmatter."""
    meta = {"title": "", "mode": "", "context": ""}
    lines = content.splitlines()
    in_context = False
    context_lines: list[str] = []
    context_indent: Optional[int] = None

    for line in lines:
        stripped = line.strip()
        if stripped.startswith("title:"):
            meta["title"] = stripped[6:].strip().strip("\"'")
            in_context = False
        elif stripped.startswith("mode:"):
            meta["mode"] = stripped[5:].strip().strip("\"'")
            in_context = False
        elif stripped.startswith("context:") and ("|" in stripped or stripped == "context:"):
            in_context = True
            context_lines = []
            context_indent = None
        elif in_context:
            # Top-level key ends the context block
            if line and not line[0].isspace() and stripped and ":" in stripped:
                meta["context"] = "\n".join(context_lines)
                in_context = False
            else:
                if context_indent is None and line.startswith(" "):
                    context_indent = len(line) - len(line.lstrip())
                if context_indent and line.startswith(" " * context_indent):
                    context_lines.append(line[context_indent:])
                elif not line.startswith(" ") and line.strip() == "":
                    context_lines.append("")
                else:
                    context_lines.append(line)

    if in_context and context_lines:
        meta["context"] = "\n".join(context_lines)

    return meta


def classify_spec_type(spec_metadata: dict) -> tuple[str, str]:
    """Classify spec as 'code', 'admin-closure', or 'unknown'. Returns (type, reason)."""
    title = spec_metadata.get("title", "").lower()
    mode = spec_metadata.get("mode", "").lower()

    for kw in ADMIN_TITLE_KEYWORDS:
        if kw in title:
            return "admin-closure", f"title contains admin keyword: '{kw}'"

    # mode=update/patch with metadata-only title signals
    if mode in ("update", "patch"):
        metadata_signals = ["status", "yaml", "config", "field", "closed_at", "initiative", "kr"]
        for sig in metadata_signals:
            if sig in title:
                return "admin-closure", f"mode={mode!r} with metadata title signal: '{sig}'"

    admin_title_patterns = ["adding closed_at", "updating initiative yaml"]
    for pat in admin_title_patterns:
        if pat in title:
            return "admin-closure", f"title mentions: '{pat}'"

    return "code", "no admin-closure indicators found"


# --- Gaming analysis for a single spec ---

def analyze_spec(spec_id: str) -> dict:
    """Analyze a single spec for gaming patterns."""
    spec = read_spec(spec_id)
    if not spec:
        return {
            "spec_id": spec_id,
            "verdict": "UNKNOWN",
            "evidence": [f"Spec file not found: {BOI_QUEUE / spec_id}.spec.md"],
            "files_changed": [],
            "metric_changes": [],
            "code_changes": [],
        }

    content = spec["content"]
    evidence = []
    metric_changes = []
    code_changes = []
    gaming_signals = 0
    real_signals = 0

    # 0. Admin spec classification — must run before any gaming scoring
    spec_metadata = parse_spec_metadata(content)
    spec_type, classification_reason = classify_spec_type(spec_metadata)
    evidence.append(f"spec classification: {spec_type} ({classification_reason})")

    if spec_type == "admin-closure":
        return {
            "spec_id": spec_id,
            "verdict": "ADMIN",
            "spec_type": "admin-closure",
            "classification_reason": classification_reason,
            "gaming_signals": 0,
            "real_signals": 0,
            "evidence": evidence,
            "files_changed": [],
            "metric_changes": [],
            "code_changes": [],
            "is_drive_kr": False,
            "duration_seconds": None,
        }

    # 1. Is this a "drive KR to non-zero" initiative spec?
    is_drive_kr = spec_is_drive_kr(spec_metadata.get("title", ""))
    if is_drive_kr:
        evidence.append("spec type: Drive KR to Non-Zero (high-risk template)")
        gaming_signals += 1

    # 2. Check the embedded metric command (pre-run value)
    embedded_metric = extract_metric_command_from_spec(content)
    if embedded_metric:
        is_gamed, reason = is_trivially_gameable(embedded_metric)
        if is_gamed:
            evidence.append(f"embedded metric was trivially gameable: {reason}")
            metric_changes.append({"type": "trivial_metric", "command": embedded_metric[:100]})
            gaming_signals += 2

    # 3. Check the verify command — does it just re-run the metric?
    verify_cmd = get_verify_command(content)
    if verify_cmd and embedded_metric:
        # If verify uses the same command as metric, it can be gamed by gaming the metric
        if embedded_metric.strip()[:30] in verify_cmd:
            evidence.append("verify command re-runs same metric — can be gamed by metric rewrite")
            gaming_signals += 1

    # 4. Check if verify is trivially passable
    if verify_cmd:
        is_gamed, reason = is_trivially_gameable(verify_cmd)
        if is_gamed:
            evidence.append(f"verify command is trivially passable: {reason}")
            gaming_signals += 2

    # 5. Duration anomaly
    duration = get_spec_duration_seconds(spec_id)
    if duration is not None:
        if duration < 300 and is_drive_kr:  # <5 minutes for a build task
            evidence.append(f"completed in {duration:.0f}s (<5min) for a build spec")
            gaming_signals += 1
        elif duration > 0:
            evidence.append(f"completion time: {duration:.0f}s")
            if duration > 600:
                real_signals += 1

    # 6. Look at what files changed (git diff anchored to spec dispatch time, cross-repo)
    files_changed = []
    try:
        prompt_path = BOI_QUEUE / f"{spec_id}.prompt.md"
        start_time = prompt_path.stat().st_mtime if prompt_path.exists() else None
        exit_time = get_exit_time(spec_id)

        # --- Primary workspace scan (includes gaming-pattern detection) ---
        dispatch_commit = get_dispatch_commit(start_time, WORKSPACE) if start_time else None
        if dispatch_commit:
            git_diff_ref = f"{dispatch_commit}..HEAD"
        else:
            git_diff_ref = "HEAD"
            evidence.append("warning: dispatch-time commit unavailable, falling back to HEAD diff")

        result = subprocess.run(
            ["git", "diff", "--name-only", git_diff_ref],
            cwd=str(WORKSPACE), capture_output=True, text=True, timeout=10
        )
        all_changed = [f.strip() for f in result.stdout.splitlines() if f.strip()]

        for f in all_changed:
            fpath = WORKSPACE / f
            if fpath.exists():
                fmtime = fpath.stat().st_mtime
                if start_time and exit_time:
                    window_start = start_time - 60
                    window_end = exit_time + 1800
                    if window_start <= fmtime <= window_end:
                        files_changed.append(f)
                else:
                    files_changed.append(f)

        initiative_yaml_changes = [f for f in files_changed if f.startswith('initiatives/') or f.startswith('experiments/')]
        code_files = [f for f in files_changed if f.endswith(('.py', '.rs', '.sh', '.js', '.ts'))]
        doc_files = [f for f in files_changed if f.endswith('.md')]

        if initiative_yaml_changes and not code_files:
            evidence.append(f"only initiative/experiment YAML files changed: {initiative_yaml_changes}")
            metric_changes.extend([{"type": "yaml_only", "file": f} for f in initiative_yaml_changes])
            gaming_signals += 2
        elif code_files:
            evidence.append(f"real code files changed: {code_files}")
            code_changes.extend([{"type": "code", "file": f} for f in code_files])
            real_signals += len(code_files)
        elif doc_files:
            evidence.append(f"doc files changed: {doc_files}")
            real_signals += 1

        # Untracked files in primary workspace
        untracked_changes = get_untracked_files(WORKSPACE, start_time, exit_time)
        if untracked_changes:
            untracked_paths = [u["file"] for u in untracked_changes]
            evidence.append(f"untracked files modified during spec window: {untracked_paths}")
            code_changes.extend(untracked_changes)
            real_signals += len(untracked_changes)

        # --- Cross-repo scanning ---
        # Collect repos: from spec context + ~/github.com/mrap/ fallback sweep
        context_repos = extract_repos_from_spec(content)
        scanned: set[Path] = {WORKSPACE}

        fallback_repos: list[Path] = []
        if start_time:
            fallback_repos = scan_github_mrap_repos_in_window(start_time, exit_time)

        cross_repos: list[Path] = []
        for r in context_repos + fallback_repos:
            if r not in scanned:
                cross_repos.append(r)
                scanned.add(r)

        for repo_path in cross_repos:
            if not repo_path.exists():
                evidence.append(f"cross-repo: {repo_path} not found on disk, skipping")
                continue
            repo_scan = scan_repo_for_changes(repo_path, start_time, exit_time)
            evidence.extend(repo_scan["evidence"])
            if repo_scan["code_changes"]:
                evidence.append(f"cross-repo scan found changes in {repo_path.name}")
                code_changes.extend(repo_scan["code_changes"])
                real_signals += len(repo_scan["code_changes"])

    except subprocess.TimeoutExpired:
        evidence.append("git diff timed out — could not check file changes")
    except Exception as e:
        evidence.append(f"git diff error: {e}")

    # 7. Determine initiative and check KR state
    init_id = get_spec_initiative(content)
    kr_id = get_spec_kr(content)
    if init_id and kr_id:
        kr = find_kr(init_id, kr_id)
        if kr:
            if kr_lower_better_math_error(kr):
                evidence.append(
                    f"MATH ERROR: {init_id}/{kr_id} is lower_is_better but "
                    f"current={kr.get('current')} > target={kr.get('target')} yet status=met"
                )
                gaming_signals += 3
            cmd = kr.get("_raw_command", "") or kr.get("metric", {}).get("command", "")
            is_gamed, reason = is_trivially_gameable(cmd)
            if is_gamed:
                evidence.append(f"current metric command in initiative YAML is trivially gameable: {reason}")
                metric_changes.append({"type": "gamed_metric_in_yaml", "kr": f"{init_id}/{kr_id}", "reason": reason})
                gaming_signals += 2
            elif cmd:
                evidence.append(f"metric command looks non-trivial (may be legitimate)")
                real_signals += 1

    # 8. Final verdict
    if gaming_signals >= 4:
        verdict = "GAMING"
    elif gaming_signals >= 2:
        verdict = "SUSPECT"
    elif real_signals >= 2:
        verdict = "LEGITIMATE"
    else:
        verdict = "UNKNOWN"

    return {
        "spec_id": spec_id,
        "verdict": verdict,
        "gaming_signals": gaming_signals,
        "real_signals": real_signals,
        "evidence": evidence,
        "files_changed": files_changed,
        "metric_changes": metric_changes,
        "code_changes": code_changes,
        "is_drive_kr": is_drive_kr,
        "duration_seconds": duration,
    }


# --- Sweep mode ---

def find_completed_specs_last_24h() -> list[str]:
    """Return spec IDs whose .exit files are newer than 24h ago."""
    cutoff = time.time() - (24 * 3600)
    spec_ids = []
    for exit_file in BOI_QUEUE.glob("*.exit"):
        if exit_file.stat().st_mtime >= cutoff:
            spec_id = exit_file.stem  # q-NNN
            spec_file = BOI_QUEUE / f"{spec_id}.spec.md"
            if spec_file.exists():
                spec_ids.append(spec_id)
    spec_ids.sort()
    return spec_ids


def sweep() -> dict:
    """Sweep all specs completed in last 24h and aggregate results."""
    spec_ids = find_completed_specs_last_24h()
    results = []
    gaming = 0
    suspect = 0
    legitimate = 0
    unknown = 0

    for sid in spec_ids:
        r = analyze_spec(sid)
        results.append(r)
        v = r["verdict"]
        if v == "GAMING":
            gaming += 1
            emit_gaming_event(r)
        elif v == "SUSPECT":
            suspect += 1
        elif v == "LEGITIMATE":
            legitimate += 1
        else:
            unknown += 1

    summary = {
        "total": len(spec_ids),
        "gaming": gaming,
        "suspect": suspect,
        "legitimate": legitimate,
        "unknown": unknown,
        "gaming_rate_pct": round(gaming / len(spec_ids) * 100, 1) if spec_ids else 0,
        "sweep_time": datetime.now(timezone.utc).isoformat(),
        "specs": results,
    }
    return summary


def emit_gaming_event(result: dict):
    """Emit hex.quality.gaming.detected event."""
    EVENTS_DIR.mkdir(parents=True, exist_ok=True)
    ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")
    event_file = EVENTS_DIR / f"quality-gaming-{result['spec_id']}-{ts}.json"
    event = {
        "event": "hex.quality.gaming.detected",
        "ts": datetime.now(timezone.utc).isoformat(),
        "spec_id": result["spec_id"],
        "evidence": result["evidence"],
        "gaming_signals": result["gaming_signals"],
    }
    event_file.write_text(json.dumps(event, indent=2))


# --- KR reality check ---

def reality_check_kr(kr_ref: str) -> dict:
    """Reality-check a specific KR: init-foo/kr-N."""
    parts = kr_ref.strip("/").split("/")
    if len(parts) != 2:
        return {"kr_id": kr_ref, "error": "format must be <init-id>/<kr-id>"}

    init_id, kr_id = parts
    kr = find_kr(init_id, kr_id)
    if not kr:
        return {"kr_id": kr_ref, "error": f"KR not found: {init_id}/{kr_id}"}

    claimed_value = kr.get("current")
    claimed_status = kr.get("status", "open")
    target = kr.get("target")
    direction = kr.get("metric", {}).get("direction", "higher_is_better")
    description = kr.get("description", "")
    metric_cmd = kr.get("_raw_command", "") or kr.get("metric", {}).get("command", "")

    evidence = []
    independent_check_value = None
    match = None
    fraud_detected = False

    # 1. Check metric command for gaming
    is_gamed, reason = is_trivially_gameable(metric_cmd)
    if is_gamed:
        evidence.append(f"metric command is trivially gameable: {reason}")
        fraud_detected = True

    # 2. Math error check
    if kr_lower_better_math_error(kr):
        evidence.append(
            f"MATH ERROR: lower_is_better but current={claimed_value} > target={target}, yet status=met"
        )
        fraud_detected = True
        independent_check_value = claimed_value
        match = False

    # 3. If metric command looks real, try to run it independently
    if not is_gamed and metric_cmd and not fraud_detected:
        try:
            run_result = subprocess.run(
                ["bash", "-c", f"cd {WORKSPACE} && {metric_cmd}"],
                capture_output=True, text=True, timeout=30
            )
            output = run_result.stdout.strip()
            if output:
                try:
                    independent_check_value = float(output.split()[-1])
                    # Compare with claimed
                    tolerance = 0.05  # 5% tolerance
                    if claimed_value is not None:
                        diff = abs(independent_check_value - float(claimed_value))
                        relative_diff = diff / max(abs(float(claimed_value)), 1)
                        match = relative_diff <= tolerance
                        if not match:
                            evidence.append(
                                f"independent measurement {independent_check_value} differs from "
                                f"claimed {claimed_value} by {relative_diff:.1%}"
                            )
                        else:
                            evidence.append(
                                f"independent measurement {independent_check_value} matches claimed {claimed_value}"
                            )
                except (ValueError, IndexError):
                    evidence.append(f"could not parse metric output: {output[:100]!r}")
            else:
                stderr = run_result.stderr.strip()
                evidence.append(f"metric command produced no output (exit={run_result.returncode})")
                if stderr:
                    evidence.append(f"stderr: {stderr[:200]}")
                if run_result.returncode != 0:
                    fraud_detected = True
                    evidence.append("metric command failed — claimed value may be stale/false")
        except subprocess.TimeoutExpired:
            evidence.append("metric command timed out (>30s)")
        except Exception as e:
            evidence.append(f"error running metric: {e}")

    # 4. Is the claimed status consistent with math?
    if claimed_status == "met" and not fraud_detected and independent_check_value is not None:
        try:
            val = float(independent_check_value)
            tgt = float(target)
            if direction == "higher_is_better" and val < tgt:
                evidence.append(f"status=met but independent check {val} < target {tgt}")
                fraud_detected = True
            elif direction == "lower_is_better" and val > tgt:
                evidence.append(f"status=met but independent check {val} > target {tgt}")
                fraud_detected = True
        except (TypeError, ValueError):
            pass

    verdict = "SUSPECT" if fraud_detected else ("VERIFIED" if match else "UNVERIFIED")

    return {
        "kr_id": kr_ref,
        "description": description,
        "claimed_value": claimed_value,
        "claimed_status": claimed_status,
        "target": target,
        "direction": direction,
        "independent_check_value": independent_check_value,
        "match": match,
        "fraud_detected": fraud_detected,
        "verdict": verdict,
        "evidence": evidence,
        "metric_command_preview": metric_cmd[:120] if metric_cmd else None,
    }


# --- CLI ---

def main():
    parser = argparse.ArgumentParser(description="Quality Antagonist — gaming detector")
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--spec", metavar="SPEC_ID", help="Check a specific completed spec")
    group.add_argument("--sweep", action="store_true", help="Sweep all specs completed in last 24h")
    group.add_argument("--kr", metavar="INIT/KR", help="Reality-check a specific KR (e.g. init-foo/kr-1)")
    args = parser.parse_args()

    if args.spec:
        result = analyze_spec(args.spec)
        print(json.dumps(result, indent=2))
    elif args.sweep:
        result = sweep()
        print(json.dumps(result, indent=2))
    elif args.kr:
        result = reality_check_kr(args.kr)
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
