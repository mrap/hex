#!/usr/bin/env python3
"""
verify-spec-claims.py — Verify every concrete claim in a BOI spec against the actual codebase.

Usage:
  python3 .hex/scripts/verify-spec-claims.py <spec_file> [--workspace DIR] [--verbose]
  python3 .hex/scripts/verify-spec-claims.py --help

Exit codes:
  0 = all claims verified
  1 = failures found (missing or critical unverified claims)
"""

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path


# ──────────────────────────────────────────────────────────────────────────────
# Patterns for claim extraction
# ──────────────────────────────────────────────────────────────────────────────

# File reference patterns: backtick paths, inline code with /, YAML-style paths
FILE_PATH_RE = re.compile(
    r'`([^`]*(?:/[^`]+)+)`'           # `path/to/file` in backticks
    r'|(?<!["\w])(\.[/\w.-]+/[\w./:-]+)'   # .hex/scripts/foo.py  (relative)
    r'|(~(?:/[\w.-]+)+)'               # ~/paths
    r'|(?:^|\s)((?:projects|initiatives|experiments|specs|me|docs|\.hex)/[\w./-]+)',  # known dirs
    re.MULTILINE,
)

FILE_EXTENSIONS = {'.py', '.sh', '.yaml', '.yml', '.md', '.json', '.txt', '.toml'}

# CLI command patterns: `boi X`, `hex X`, `hex-agent X`, `hex-events X`
CLI_CMD_RE = re.compile(
    r'`((?:boi|hex|hex-agent|hex-events|hex-agent-spawn\.sh)\s+[\w-]+(?:\s+[\w-]+)?)`'
)

# Schema field claim patterns: "has field X", "field Y", "has 'X' field"
SCHEMA_FIELD_RE = re.compile(
    r"(?:has|contains|with)\s+['\"]?([\w_-]+)['\"]?\s+field"
    r"|field\s+['\"]?([\w_-]+)['\"]?"
    r"|['\"]?([\w_-]+)['\"]?\s+(?:key|attribute|property)\s+in",
    re.IGNORECASE,
)

# Dependency claim patterns: "this script ... does X", "script already exists and does X"
DEP_CLAIM_RE = re.compile(
    r'`([^`]+\.(?:sh|py))`[^`]*(?:already exists|which|that)\s+(?:does|generates|creates|handles|runs)',
    re.IGNORECASE,
)

# Known repeat-offense patterns loaded from learnings.md
REPEAT_OFFENSE_MARKERS = [
    ("markdown tables in Slack context", re.compile(r'\|.*\|.*\|', re.MULTILINE)),
    ("bare python3 without venv path", re.compile(r'(?<![/\w])python3\s+(?!~|/)[^\s]')),
    ("pytest without venv", re.compile(r'`pytest\b(?!\s+-h|\s+--help)')),
    ("boi research command (never built)", re.compile(r'`boi\s+research\b')),
]


# ──────────────────────────────────────────────────────────────────────────────
# Helpers
# ──────────────────────────────────────────────────────────────────────────────

def expand(path: str, workspace: str) -> str:
    """Expand ~ and resolve relative paths from workspace."""
    path = os.path.expanduser(path)
    if not os.path.isabs(path):
        path = os.path.join(workspace, path)
    return path


def file_exists(path: str, workspace: str) -> bool:
    return os.path.exists(expand(path, workspace))


def cmd_exists(binary: str) -> bool:
    """Check if a binary is on PATH or is an absolute/relative executable."""
    if os.path.isabs(binary) or binary.startswith('.'):
        return os.path.isfile(binary) and os.access(binary, os.X_OK)
    for d in os.environ.get('PATH', '').split(':'):
        candidate = os.path.join(d, binary)
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return True
    return False


def subcommand_exists(binary: str, subcommand: str) -> bool:
    """Run `<binary> --help` and check if subcommand appears in output."""
    try:
        result = subprocess.run(
            [binary, '--help'],
            capture_output=True, text=True, timeout=5,
        )
        out = (result.stdout + result.stderr).lower()
        return subcommand.lower() in out
    except Exception:
        return False


def looks_like_file(candidate: str) -> bool:
    """True if a string looks like an intentional file path (not just any slash)."""
    if not candidate:
        return False
    # Must have an extension OR end in known directory structures
    has_ext = any(candidate.endswith(ext) for ext in FILE_EXTENSIONS)
    has_dir_pattern = any(candidate.startswith(p) for p in (
        '.hex/', 'projects/', 'initiatives/', 'experiments/', 'specs/',
        'me/', 'docs/', '~/', '~/.hex', '~/.boi',
    ))
    return has_ext or has_dir_pattern


def find_failure_patterns_in_learnings(learnings_path: str) -> list[tuple[str, str]]:
    """Read learnings.md and return a list of (pattern_name, excerpt) tuples."""
    if not os.path.exists(learnings_path):
        return []
    try:
        with open(learnings_path) as f:
            content = f.read()
        # Find the Agent Failure Patterns section
        m = re.search(r'## Agent Failure Patterns\n(.*?)(?=\n## |\Z)', content, re.DOTALL)
        if not m:
            return []
        section = m.group(1)
        # Each bullet is a pattern
        bullets = re.findall(r'^- (.+?)(?=\n- |\Z)', section, re.DOTALL | re.MULTILINE)
        return [(b[:100].strip(), b) for b in bullets]
    except Exception:
        return []


# ──────────────────────────────────────────────────────────────────────────────
# Claim extractors
# ──────────────────────────────────────────────────────────────────────────────

def extract_file_claims(text: str) -> list[tuple[str, int]]:
    """Return list of (path, line_number) for file path claims."""
    lines = text.splitlines()
    found = []
    seen = set()
    for lineno, line in enumerate(lines, 1):
        # backtick paths
        for m in re.finditer(r'`([^`]*(?:/[^`\s]+)+)`', line):
            candidate = m.group(1).strip()
            if looks_like_file(candidate) and candidate not in seen:
                found.append((candidate, lineno))
                seen.add(candidate)
        # bare relative paths
        for m in re.finditer(r'(?<![`"\w])((?:~\/|\.\/|\.hex\/|projects\/|initiatives\/|experiments\/|specs\/|me\/|docs\/)[\w./-]+)', line):
            candidate = m.group(1).rstrip('.,)')
            if looks_like_file(candidate) and candidate not in seen:
                found.append((candidate, lineno))
                seen.add(candidate)
    return found


def extract_cli_claims(text: str) -> list[tuple[str, str, int]]:
    """Return list of (binary, subcommand, line_number)."""
    lines = text.splitlines()
    found = []
    seen = set()
    cli_re = re.compile(r'`((?:boi|hex|hex-agent|hex-events)\s+([\w-]+)(?:\s+[\w-]+)?)`')
    for lineno, line in enumerate(lines, 1):
        for m in cli_re.finditer(line):
            full_cmd = m.group(1)
            parts = full_cmd.split()
            binary = parts[0]
            subcommand = parts[1] if len(parts) > 1 else ''
            key = (binary, subcommand)
            if key not in seen:
                found.append((binary, subcommand, lineno))
                seen.add(key)
    return found


def extract_schema_claims(text: str) -> list[tuple[str, str, int]]:
    """Return list of (yaml_file_hint, field_name, line_number)."""
    lines = text.splitlines()
    found = []
    yaml_context_re = re.compile(r'([\w.-]+\.ya?ml)')
    field_re = re.compile(
        r"(?:has|contains|with)\s+['\"]?([\w_-]+)['\"]?\s+field"
        r"|['\"]?([\w_-]+)['\"]?\s+(?:key|attribute|property)"
        r"|([\w_-]+):\s*field\s+(?:is|should|must)",
        re.IGNORECASE,
    )
    for lineno, line in enumerate(lines, 1):
        yaml_files = yaml_context_re.findall(line)
        for m in field_re.finditer(line):
            field = next((g for g in m.groups() if g), None)
            if field and len(field) > 1:
                hint = yaml_files[0] if yaml_files else 'unknown.yaml'
                found.append((hint, field, lineno))
    return found


def extract_dep_claims(text: str) -> list[tuple[str, str, int]]:
    """Return list of (script_path, claimed_behavior, line_number)."""
    lines = text.splitlines()
    found = []
    pattern = re.compile(
        r'`([^`]+\.(?:sh|py))`[^`\n]*(?:already exists|which|that)\s+(does|generates|creates|handles|runs|[^.]+)',
        re.IGNORECASE,
    )
    for lineno, line in enumerate(lines, 1):
        for m in pattern.finditer(line):
            script = m.group(1)
            behavior = m.group(2)[:60]
            found.append((script, behavior, lineno))
    return found


# ──────────────────────────────────────────────────────────────────────────────
# Verifiers
# ──────────────────────────────────────────────────────────────────────────────

def verify_file_claims(claims, workspace: str, verbose: bool) -> tuple[list, list]:
    verified, missing = [], []
    for path, lineno in claims:
        exists = file_exists(path, workspace)
        label = f"{path} (line {lineno})"
        if exists:
            verified.append(label)
            if verbose:
                print(f"  CLAIM: {path} exists → VERIFIED")
        else:
            missing.append(label)
            if verbose:
                print(f"  CLAIM: {path} exists → MISSING")
    return verified, missing


def verify_cli_claims(claims, verbose: bool) -> tuple[list, list, list]:
    verified, missing, unverified = [], [], []
    for binary, subcommand, lineno in claims:
        label = f'"{binary} {subcommand}" command (line {lineno})'
        if not cmd_exists(binary):
            missing.append(label)
            if verbose:
                print(f"  CLAIM: {binary} binary exists → MISSING")
            continue
        if subcommand and not subcommand_exists(binary, subcommand):
            unverified.append(f'"{binary} {subcommand}" subcommand unconfirmed (line {lineno})')
            if verbose:
                print(f"  CLAIM: {binary} {subcommand} subcommand → UNVERIFIED")
        else:
            verified.append(label)
            if verbose:
                print(f"  CLAIM: {binary} {subcommand} command → VERIFIED")
    return verified, missing, unverified


def verify_schema_claims(claims, workspace: str, verbose: bool) -> tuple[list, list]:
    verified, unverified = [], []
    for yaml_hint, field, lineno in claims:
        label = f'{yaml_hint} has \'{field}\' field (line {lineno})'
        # Try to find the yaml file in the workspace
        found_path = None
        for root, dirs, files in os.walk(workspace):
            # Skip hidden heavy dirs
            dirs[:] = [d for d in dirs if d not in ('.git', 'node_modules', '__pycache__', '.playwright-mcp')]
            for fname in files:
                if fname == yaml_hint or fname == os.path.basename(yaml_hint):
                    found_path = os.path.join(root, fname)
                    break
            if found_path:
                break
        if not found_path:
            unverified.append(f"{label} (yaml file not found to check)")
            if verbose:
                print(f"  CLAIM: {label} → UNVERIFIED (file not found)")
            continue
        try:
            with open(found_path) as f:
                content = f.read()
            if re.search(rf'(?:^|\s){re.escape(field)}\s*:', content, re.MULTILINE):
                verified.append(label)
                if verbose:
                    print(f"  CLAIM: {label} → VERIFIED")
            else:
                unverified.append(label)
                if verbose:
                    print(f"  CLAIM: {label} → UNVERIFIED")
        except Exception:
            unverified.append(f"{label} (could not read file)")
    return verified, unverified


def verify_dep_claims(claims, workspace: str, verbose: bool) -> tuple[list, list]:
    verified, unverified = [], []
    for script, behavior, lineno in claims:
        label = f'{script} "{behavior}" (line {lineno})'
        expanded = expand(script, workspace)
        if not os.path.exists(expanded):
            unverified.append(f"{label} (script not found)")
            if verbose:
                print(f"  CLAIM: {label} → UNVERIFIED (script missing)")
            continue
        # Grep for a keyword from the behavior claim
        keyword = re.sub(r'\W+', ' ', behavior).split()
        keyword = keyword[0] if keyword else None
        if keyword:
            try:
                result = subprocess.run(
                    ['grep', '-qi', keyword, expanded],
                    capture_output=True, timeout=5,
                )
                if result.returncode == 0:
                    verified.append(label)
                    if verbose:
                        print(f"  CLAIM: {label} → VERIFIED")
                else:
                    unverified.append(label)
                    if verbose:
                        print(f"  CLAIM: {label} → UNVERIFIED (keyword not found in script)")
            except Exception:
                unverified.append(f"{label} (grep failed)")
        else:
            verified.append(label)
    return verified, unverified


def check_repeat_offenses(text: str, learnings_path: str) -> list[str]:
    """Return list of repeat offense descriptions found in the spec."""
    offenses = []
    # Built-in patterns
    for name, pattern in REPEAT_OFFENSE_MARKERS:
        if pattern.search(text):
            offenses.append(f"Repeat offense: {name}")
    # Dynamic patterns from learnings.md
    patterns_from_learnings = find_failure_patterns_in_learnings(learnings_path)
    # Check a few high-signal keywords from each learning entry
    for short, full in patterns_from_learnings:
        # Extract key phrases from the learning
        keywords = re.findall(r'(?:markdown table|boi research|silent (?:error|fail|skip)|bare python3|untested|hallucinated)', full, re.IGNORECASE)
        for kw in keywords:
            if re.search(re.escape(kw), text, re.IGNORECASE):
                offenses.append(f"Matches learning pattern: '{kw}'")
                break
    return list(dict.fromkeys(offenses))  # deduplicate preserving order


# ──────────────────────────────────────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description='Verify every concrete claim in a BOI spec against the actual codebase.',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog='Exit 0 = all verified. Exit 1 = failures found.',
    )
    parser.add_argument('spec_file', nargs='?', help='Path to the spec .md or .yaml file')
    parser.add_argument('--workspace', default=os.getcwd(),
                        help='Root of the workspace (default: cwd)')
    parser.add_argument('--verbose', action='store_true',
                        help='Print per-claim results')
    args = parser.parse_args()

    if not args.spec_file:
        parser.print_help()
        sys.exit(0)

    spec_path = os.path.expanduser(args.spec_file)
    if not os.path.exists(spec_path):
        print(f"ERROR: spec file not found: {spec_path}", file=sys.stderr)
        sys.exit(1)

    with open(spec_path) as f:
        text = f.read()

    workspace = os.path.expanduser(args.workspace)
    learnings_path = os.path.join(workspace, 'me', 'learnings.md')

    print(f"SPEC: {spec_path}")

    # ── Extract claims ──────────────────────────────────────────────────────
    file_claims = extract_file_claims(text)
    cli_claims = extract_cli_claims(text)
    schema_claims = extract_schema_claims(text)
    dep_claims = extract_dep_claims(text)
    total_claims = len(file_claims) + len(cli_claims) + len(schema_claims) + len(dep_claims)
    print(f"CLAIMS: {total_claims} found")

    if args.verbose:
        print("\n── File References ──")

    f_verified, f_missing = verify_file_claims(file_claims, workspace, args.verbose)

    if args.verbose:
        print("\n── CLI Commands ──")

    c_verified, c_missing, c_unverified = verify_cli_claims(cli_claims, args.verbose)

    if args.verbose:
        print("\n── Schema Fields ──")

    s_verified, s_unverified = verify_schema_claims(schema_claims, workspace, args.verbose)

    if args.verbose:
        print("\n── Dependency Claims ──")

    d_verified, d_unverified = verify_dep_claims(dep_claims, workspace, args.verbose)

    if args.verbose:
        print("\n── Repeat Offense Check ──")

    repeat_offenses = check_repeat_offenses(text, learnings_path)

    # ── Aggregate ───────────────────────────────────────────────────────────
    all_verified = f_verified + c_verified + s_verified + d_verified
    all_missing = f_missing + c_missing
    all_unverified = c_unverified + s_unverified + d_unverified

    print(f"VERIFIED: {len(all_verified)}")

    if all_missing:
        print(f"MISSING: {len(all_missing)}")
        for item in all_missing:
            print(f"  - {item}")
    else:
        print("MISSING: 0")

    if all_unverified:
        print(f"UNVERIFIED: {len(all_unverified)}")
        for item in all_unverified:
            print(f"  - {item}")
    else:
        print("UNVERIFIED: 0")

    if repeat_offenses:
        print(f"REPEAT OFFENSES: {len(repeat_offenses)}")
        for item in repeat_offenses:
            print(f"  - {item}")

    # ── Verdict ─────────────────────────────────────────────────────────────
    if all_missing or repeat_offenses:
        details = []
        if all_missing:
            details.append(f"{len(all_missing)} missing claims")
        if repeat_offenses:
            details.append(f"{len(repeat_offenses)} repeat offenses")
        print(f"VERDICT: FAIL ({', '.join(details)})")
        sys.exit(1)
    elif all_unverified:
        print(f"VERDICT: WARN ({len(all_unverified)} unverified claims — manual review needed)")
        sys.exit(0)
    else:
        print("VERDICT: PASS")
        sys.exit(0)


if __name__ == '__main__':
    main()
