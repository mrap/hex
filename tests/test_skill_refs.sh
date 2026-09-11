#!/usr/bin/env bash
# Audits install-relative SKILL.md references against their source-layout owner.
# This checker deliberately does not execute install.sh. The installer can build
# companions and modify the operator environment, which is outside a static
# reference audit's contract.
set -euo pipefail

SCRIPT_PATH="${BASH_SOURCE[0]}"
case "$SCRIPT_PATH" in
    /*) ;;
    *) SCRIPT_PATH="$PWD/$SCRIPT_PATH" ;;
esac
SCRIPT_DIR="${SCRIPT_PATH%/*}"
REPO_ROOT="${SCRIPT_DIR%/tests}"

# Use the system interpreter directly so a hostile PATH cannot turn this
# source-only audit into an invocation of a repository or operator command.
exec /usr/bin/python3 -I -B - "$REPO_ROOT" <<'PYEOF'
from __future__ import annotations

import re
import sys
from pathlib import Path, PurePosixPath

repo_root = Path(sys.argv[1]).resolve()
skills_dir = repo_root / "system" / "skills"

SKIP_PATTERNS = re.compile(
    r"\*|"             # glob wildcards
    r"\{[^}]+\}|"      # placeholders
    r"YYYY|HHMMSS|NNN|WXX|"  # date and numeric templates
    r"<[^>]+>"          # placeholders
)
HEX_DIR_PREFIX = re.compile(r"^\$\{?HEX_DIR\}?/")
KNOWN_PREFIXES = (
    ".hex/", ".agents/", ".claude/", "me/", "evolution/", "projects/", "people/",
    "landings/", "raw/", "specs/",
)
ROOT_FILES = {"todo.md", "CLAUDE.md", "AGENTS.md"}

# These paths are created by the installer or during normal operation. They are
# intentionally reported as optional, never treated as source files.
OPTIONAL_PREFIXES = (
    ".hex/.upgrade-cache/", ".hex/memory/",
    "projects/", "people/", "landings/", "raw/", "specs/",
)
OPTIONAL_PATHS = {
    ".hex/", ".hex/llm-preference", ".hex/memory.db", ".hex/migrate-from",
    ".hex/settings.local.json", ".hex/upgrade.json", "evolution/", "me/",
}

# install.sh builds this binary from the harness crate and copies it into the
# installed layout. A generated binary is valid only when both declarations
# remain present in source; it is not a blanket exception for missing files.
GENERATED = {
    ".hex/bin/hex": (
        "system/harness/Cargo.toml",
        "install.sh",
    ),
}


def normalize_path(path: str) -> str:
    path = path.strip().strip("\"'")
    path = HEX_DIR_PREFIX.sub("", path)
    return path[2:] if path.startswith("./") else path


def should_skip(path: str) -> bool:
    return bool(SKIP_PATTERNS.search(path)) or (
        "/" not in path and path not in ROOT_FILES
    )


def extract_paths(skill_md: Path) -> list[str]:
    content = skill_md.read_text(encoding="utf-8")
    paths: list[str] = []
    for code in re.findall(r"`([^`\n]+)`", content):
        code = re.sub(r"^(?:bash|python3|sh)\s+", "", code.strip())
        code = code.split()[0] if " " in code else code
        if "/" in code or code in ROOT_FILES:
            paths.append(code)
    for block in re.findall(r"```[^\n]*\n(.*?)```", content, re.DOTALL):
        for line in block.splitlines():
            line = line.strip()
            if line.startswith("#"):
                continue
            paths.extend(re.findall(r"\$\{?HEX_DIR\}?/(\S+)", line))
            paths.extend(re.findall(r"(?:^|\s)((?:\.hex|\.agents|\.claude)/\S+)", line))
    return [path.rstrip(".,;:)\'\"") for path in paths]


def safe_relative(path: str) -> PurePosixPath | None:
    pure = PurePosixPath(path.rstrip("/"))
    if not path or pure.is_absolute() or ".." in pure.parts:
        return None
    return pure


def is_bounded(root: Path, candidate: Path) -> bool:
    """Allow only files whose resolved path stays under the declared source root."""
    try:
        candidate.resolve(strict=False).relative_to(root.resolve())
    except ValueError:
        return False
    return True


def source_path(path: str) -> tuple[Path, str] | None:
    """Map an installed reference to its authoritative source owner."""
    pure = safe_relative(path)
    if pure is None:
        return None
    parts = pure.parts
    if path.startswith(".hex/"):
        return repo_root / "system" / Path(*parts[1:]), "system"
    if path.startswith(".agents/skills/"):
        return repo_root / "system" / "skills" / Path(*parts[2:]), "system/skills"
    if path.startswith(".claude/commands/"):
        return repo_root / "system" / "commands" / Path(*parts[2:]), "system/commands"
    if path == "AGENTS.md" or path == "CLAUDE.md":
        return repo_root / "templates" / "AGENTS.md", "templates/AGENTS.md"
    if path == "todo.md":
        return repo_root / "templates" / "todo.md", "templates/todo.md"
    if path == "me/me.md":
        return repo_root / "templates" / "me.md", "templates/me.md"
    if path == "evolution/observations.md":
        return repo_root / "templates" / "observations.md", "templates/observations.md"
    return None


def uncommented(line: str) -> str:
    """Return a TOML or shell line with an unquoted comment removed."""
    quote = ""
    escaped = False
    for index, char in enumerate(line):
        if escaped:
            escaped = False
        elif char == "\\" and quote == '"':
            escaped = True
        elif char in ("'", '"'):
            if not quote:
                quote = char
            elif quote == char:
                quote = ""
        elif char == "#" and not quote:
            return line[:index]
    return line


def cargo_declares_hex(manifest: Path) -> bool:
    in_binary = False
    for raw_line in manifest.read_text(encoding="utf-8").splitlines():
        line = uncommented(raw_line).strip()
        if line == "[[bin]]":
            in_binary = True
            continue
        if line.startswith("["):
            in_binary = False
            continue
        if in_binary and re.fullmatch(r'name\s*=\s*"hex"', line):
            return True
    return False


def installer_publishes_hex(installer: Path) -> bool:
    statement = re.compile(r'cp\s+"\$built"\s+"\$TARGET_DIR/\.hex/bin/hex"\s*')
    return any(
        statement.fullmatch(uncommented(raw_line).strip())
        for raw_line in installer.read_text(encoding="utf-8").splitlines()
    )


def generated_is_declared(path: str) -> bool:
    declaration = GENERATED.get(path)
    if declaration is None:
        return False
    source_file, installer_file = declaration
    source = repo_root / source_file
    installer = repo_root / installer_file
    return (
        is_bounded(repo_root, source)
        and is_bounded(repo_root, installer)
        and source.is_file()
        and installer.is_file()
        and cargo_declares_hex(source)
        and installer_publishes_hex(installer)
    )


errors = 0
warnings = 0
checked = 0
if not is_bounded(repo_root, skills_dir):
    print("  ERROR skills: system/skills  [skill root escapes repository]")
    errors += 1
elif not skills_dir.is_dir():
    print("  ERROR skills: system/skills  [missing skill root]")
    errors += 1
else:
    for skill_path in sorted(skills_dir.iterdir()):
        if not is_bounded(repo_root, skill_path):
            print(f"  ERROR {skill_path.name}: skill directory escapes repository")
            errors += 1
            continue
        if not skill_path.is_dir():
            continue
        skill_md = skill_path / "SKILL.md"
        if not is_bounded(repo_root, skill_md):
            print(f"  ERROR {skill_path.name}: SKILL.md escapes repository")
            errors += 1
            continue
        if not skill_md.is_file():
            continue
        checked += 1
        seen: set[str] = set()
        for raw_path in extract_paths(skill_md):
            path = normalize_path(raw_path)
            if should_skip(path) or (not path.startswith(KNOWN_PREFIXES) and path not in ROOT_FILES):
                continue
            if path in seen:
                continue
            seen.add(path)
            if safe_relative(path) is None:
                print(f"  ERROR {skill_path.name}: {path}  [unsafe install-relative path]")
                errors += 1
                continue
            if path in GENERATED:
                if generated_is_declared(path):
                    print(f"  OK    {skill_path.name}: {path}  [generated from declared source]")
                else:
                    print(f"  ERROR {skill_path.name}: {path}  [generated artifact declaration missing]")
                    errors += 1
                continue
            if path in OPTIONAL_PATHS or path.startswith(OPTIONAL_PREFIXES):
                if path.endswith((".py", ".sh")):
                    print(f"  ERROR {skill_path.name}: {path}  [missing required script]")
                    errors += 1
                    continue
                print(f"  WARN  {skill_path.name}: {path}  [runtime-created or optional]")
                warnings += 1
                continue
            mapped = source_path(path)
            if mapped is not None:
                candidate, owner = mapped
                required_file = (
                    bool(PurePosixPath(path.rstrip("/")).suffix)
                    or path.startswith(".hex/bin/")
                )
                if not is_bounded(repo_root, candidate):
                    print(f"  ERROR {skill_path.name}: {path}  [source escapes repository]")
                    errors += 1
                elif not candidate.exists():
                    print(f"  ERROR {skill_path.name}: {path}  [missing source: {candidate.relative_to(repo_root)}]")
                    errors += 1
                elif required_file and not candidate.is_file():
                    print(f"  ERROR {skill_path.name}: {path}  [required source is not a file]")
                    errors += 1
                else:
                    print(f"  OK    {skill_path.name}: {path}  [{owner}]")
                continue
            # A supported install-relative path must have a source mapping. This
            # keeps a missing script or binary from being hidden as an optional path.
            print(f"  ERROR {skill_path.name}: {path}  [no source-layout mapping]")
            errors += 1

if checked == 0:
    print("  ERROR skills: no readable SKILL.md files found")
    errors += 1

print()
print(f"Checked: {checked} SKILL.md files  |  Errors: {errors}  |  Warnings: {warnings}")
if errors:
    print("FAIL: skill reference audit found invalid source references")
    raise SystemExit(1)
print("PASS: skill reference audit succeeded")
PYEOF
