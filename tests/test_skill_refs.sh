#!/usr/bin/env bash
# Audits path references in all system/skills/*/SKILL.md files.
# Installs hex to a temp dir and checks that referenced paths exist post-install.
# Exits 0 if all required paths resolve. Exits 1 if any are missing.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SKILLS_DIR="$REPO_ROOT/system/skills"
INSTALL_DIR="/tmp/hex-skillref-test-$$"

cleanup() {
    rm -rf "$INSTALL_DIR"
}
trap cleanup EXIT

echo "Installing hex to $INSTALL_DIR for reference audit..."
if ! bash "$REPO_ROOT/install.sh" "$INSTALL_DIR" >/dev/null 2>&1; then
    echo "FAIL: install.sh failed — cannot audit refs"
    exit 1
fi
echo "Install complete. Auditing path references..."
echo ""

python3 - "$SKILLS_DIR" "$INSTALL_DIR" <<'PYEOF'
import sys
import os
import re

skills_dir = sys.argv[1]
install_root = sys.argv[2]

# Template/wildcard patterns to skip
SKIP_PATTERNS = re.compile(
    r'\*|'           # glob wildcards
    r'\{[^}]+\}|'   # {placeholder}
    r'YYYY|HHMMSS|' # date templates
    r'<[^>]+>|'     # <placeholder>
    r'NNN|'          # numeric placeholder
    r'WXX'           # week template
)

# Path prefixes that indicate install-relative paths
HEX_DIR_PREFIX = re.compile(r'^\$HEX_DIR/')

# Only check paths that look like they reference known install-relative directories
KNOWN_PREFIXES = (
    '.hex/',
    '.claude/',
    '.agents/',
    'me/',
    'evolution/',
    'projects/',
    'people/',
    'landings/',
    'raw/',
    'specs/',
    'todo.md',
    'CLAUDE.md',
    'AGENTS.md',
)

def normalize_path(path):
    """Strip $HEX_DIR/ prefix; return path relative to install root."""
    path = path.strip().strip('"\'')
    path = HEX_DIR_PREFIX.sub('', path)
    # Remove leading ./
    if path.startswith('./'):
        path = path[2:]
    return path

def should_skip(path):
    """True if path is a template, wildcard, or clearly not checkable."""
    if SKIP_PATTERNS.search(path):
        return True
    # Skip if no slash (single-word tokens aren't paths)
    if '/' not in path and path not in ('todo.md', 'CLAUDE.md', 'AGENTS.md'):
        return True
    return False

def extract_paths_from_skill(skill_md_path):
    """Extract candidate path references from a SKILL.md file."""
    with open(skill_md_path, 'r') as f:
        content = f.read()

    paths = []

    # 1. Inline backtick code: `some/path`
    inline_codes = re.findall(r'`([^`\n]+)`', content)
    for code in inline_codes:
        code = code.strip()
        # Strip shell command prefix (bash, python3, etc.)
        code = re.sub(r'^(bash|python3|sh)\s+', '', code)
        # Strip $HEX_DIR/ prefix
        code = HEX_DIR_PREFIX.sub('', code)
        # Strip arguments (space onwards)
        code = code.split()[0] if ' ' in code else code
        # Strip leading ./
        if code.startswith('./'):
            code = code[2:]
        if '/' in code or code in ('todo.md', 'CLAUDE.md', 'AGENTS.md'):
            paths.append(code)

    # 2. Code block commands: bash $HEX_DIR/... or python3 $HEX_DIR/...
    code_blocks = re.findall(r'```[^\n]*\n(.*?)```', content, re.DOTALL)
    for block in code_blocks:
        for line in block.splitlines():
            line = line.strip()
            # Skip comments
            if line.startswith('#'):
                continue
            # Find $HEX_DIR/ paths in code block lines
            matches = re.findall(r'\$HEX_DIR/(\S+)', line)
            for m in matches:
                m = m.rstrip('.,;:)\'"')
                paths.append(m)
            # Find .hex/ paths
            matches = re.findall(r'(?:^|\s)(\.hex/\S+)', line)
            for m in matches:
                m = m.rstrip('.,;:)\'"')
                paths.append(m)

    return paths

errors = 0
warnings = 0
checked = 0
missing_required = []
missing_optional = []

for skill_name in sorted(os.listdir(skills_dir)):
    skill_path = os.path.join(skills_dir, skill_name)
    if not os.path.isdir(skill_path):
        continue

    skill_md = os.path.join(skill_path, 'SKILL.md')
    if not os.path.exists(skill_md):
        continue

    checked += 1
    raw_paths = extract_paths_from_skill(skill_md)
    checked_for_skill = set()

    for raw_path in raw_paths:
        path = normalize_path(raw_path)

        if should_skip(path):
            continue

        # Only audit paths that start with known install-relative prefixes
        if not any(path.startswith(p) for p in KNOWN_PREFIXES):
            continue

        if path in checked_for_skill:
            continue
        checked_for_skill.add(path)

        full_path = os.path.join(install_root, path)
        # For wildcard-like endings, check the directory instead
        check_path = full_path
        if full_path.endswith('/'):
            check_path = full_path.rstrip('/')

        if os.path.exists(check_path):
            print(f"  OK    {skill_name}: {path}")
        else:
            # Script/python files are required; data files are optional
            ext = os.path.splitext(path)[1]
            if ext in ('.sh', '.py'):
                print(f"  ERROR {skill_name}: {path}  [NOT FOUND]")
                missing_required.append((skill_name, path))
                errors += 1
            else:
                print(f"  WARN  {skill_name}: {path}  [not found — may be runtime-created]")
                missing_optional.append((skill_name, path))
                warnings += 1

print()
print(f"Checked: {checked} SKILL.md files  |  Errors: {errors}  |  Warnings: {warnings}")

if missing_required:
    print()
    print("Missing required paths (scripts/binaries):")
    for skill, path in missing_required:
        print(f"  {skill}: {path}")

if missing_optional:
    print()
    print("Missing optional paths (runtime-created or optional):")
    for skill, path in missing_optional:
        print(f"  {skill}: {path}")

if errors > 0:
    sys.exit(1)
sys.exit(0)
PYEOF

exit_code=$?

if [ $exit_code -eq 0 ]; then
    echo "PASS: skill reference audit succeeded"
else
    echo "FAIL: skill reference audit found missing required paths"
fi

exit $exit_code
