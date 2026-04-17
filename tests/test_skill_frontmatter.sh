#!/usr/bin/env bash
# Validates YAML frontmatter in all system/skills/*/SKILL.md files.
# Exits 0 if all required fields pass. Exits 1 if any errors found.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SKILLS_DIR="$REPO_ROOT/system/skills"

errors=0
warnings=0
checked=0

python3 - "$SKILLS_DIR" <<'PYEOF'
import sys
import os
import re

skills_dir = sys.argv[1]
errors = 0
warnings = 0
checked = 0

for skill_name in sorted(os.listdir(skills_dir)):
    skill_path = os.path.join(skills_dir, skill_name)
    if not os.path.isdir(skill_path):
        continue

    skill_md = os.path.join(skill_path, "SKILL.md")
    if not os.path.exists(skill_md):
        # No SKILL.md — warn but don't error (some skill dirs are scripts-only)
        print(f"  WARN  {skill_name}/SKILL.md: file not present (skipping frontmatter check)")
        warnings += 1
        continue

    checked += 1
    with open(skill_md, "r") as f:
        content = f.read()

    # Extract frontmatter block (must be at top, between --- markers)
    fm_match = re.match(r'^---\s*\n(.*?)\n---', content, re.DOTALL)
    if not fm_match:
        print(f"  ERROR {skill_name}/SKILL.md: no valid YAML frontmatter at top of file")
        errors += 1
        continue

    fm_text = fm_match.group(1)

    # Parse with yaml
    try:
        import yaml
        fm = yaml.safe_load(fm_text)
    except Exception as e:
        print(f"  ERROR {skill_name}/SKILL.md: YAML parse error: {e}")
        errors += 1
        continue

    if not isinstance(fm, dict):
        print(f"  ERROR {skill_name}/SKILL.md: frontmatter is not a YAML mapping")
        errors += 1
        continue

    file_ok = True

    # Required: name (non-empty string)
    if "name" not in fm:
        print(f"  ERROR {skill_name}/SKILL.md: missing required field 'name'")
        errors += 1
        file_ok = False
    elif not isinstance(fm["name"], str) or not fm["name"].strip():
        print(f"  ERROR {skill_name}/SKILL.md: 'name' must be a non-empty string (got {fm['name']!r})")
        errors += 1
        file_ok = False
    else:
        # Warn if name doesn't match directory (some skills intentionally differ)
        if fm["name"] != skill_name:
            print(f"  WARN  {skill_name}/SKILL.md: 'name' ({fm['name']!r}) does not match directory name ({skill_name!r})")
            warnings += 1

    # Required: description (non-empty string)
    if "description" not in fm:
        print(f"  ERROR {skill_name}/SKILL.md: missing required field 'description'")
        errors += 1
        file_ok = False
    elif not isinstance(fm["description"], str) or not fm["description"].strip():
        print(f"  ERROR {skill_name}/SKILL.md: 'description' must be a non-empty string")
        errors += 1
        file_ok = False

    # Optional: allowed-tools (array of strings)
    if "allowed-tools" in fm:
        tools = fm["allowed-tools"]
        if not isinstance(tools, list):
            print(f"  ERROR {skill_name}/SKILL.md: 'allowed-tools' must be an array")
            errors += 1
            file_ok = False
        else:
            for t in tools:
                if not isinstance(t, str):
                    print(f"  ERROR {skill_name}/SKILL.md: 'allowed-tools' entries must be strings (got {t!r})")
                    errors += 1
                    file_ok = False
                    break

    # Optional: version (string)
    if "version" in fm:
        if not isinstance(fm["version"], str) or not fm["version"].strip():
            print(f"  ERROR {skill_name}/SKILL.md: 'version' must be a non-empty string if present")
            errors += 1
            file_ok = False

    if file_ok:
        print(f"  OK    {skill_name}/SKILL.md")

print()
print(f"Checked: {checked} SKILL.md files  |  Errors: {errors}  |  Warnings: {warnings}")

if errors > 0:
    sys.exit(1)
sys.exit(0)
PYEOF

exit_code=$?

if [ $exit_code -eq 0 ]; then
    echo "PASS: frontmatter validation succeeded"
else
    echo "FAIL: frontmatter validation found errors"
fi

exit $exit_code
