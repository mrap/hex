#!/usr/bin/env python3
"""BOI spec validator. Checks specs against known failure patterns from learnings.md.

Usage: python3 validate-boi-spec.py <spec-path>

Exit codes:
  0 = all checks pass
  1 = violations found
  2 = usage error
"""

import re
import sys
from pathlib import Path


# --- Check definitions ---
# Each check: (id, description, learning_ref, detector_fn)
# detector_fn(lines) -> list of (line_num, line_text) violations

def check_grep_perl_regex(lines):
    """macOS BSD grep doesn't support -P (Perl regex). Use -E instead."""
    violations = []
    pattern = re.compile(r'\bgrep\b.*\s-[a-zA-Z]*P')
    for i, line in enumerate(lines, 1):
        # Skip lines that are documenting a fix for grep -P (past tense / narrative)
        lower = line.lower()
        if 'replaced' in lower or 'already has' in lower or 'fixed' in lower:
            continue
        if pattern.search(line):
            violations.append((i, line.rstrip()))
    return violations


def check_env_grep_leak(lines):
    """env | grep leaks credentials in persisted transcripts."""
    violations = []
    pattern = re.compile(r'\benv\s*\|\s*grep')
    for i, line in enumerate(lines, 1):
        if pattern.search(line):
            violations.append((i, line.rstrip()))
    return violations


def check_relative_output_paths(lines):
    """BOI workers need absolute paths for output files."""
    violations = []
    # Match lines that look like verify commands or output directives with relative paths
    # Look for: test -f <relative>, > <relative>, >> <relative>, tee <relative>
    # A relative path starts with a word char (not / or ~ or $)
    verify_pattern = re.compile(r'(?:test\s+-[fedsrwx]\s+|>\s*|>>\s*|tee\s+)([a-zA-Z][a-zA-Z0-9_./-]+\.(md|json|txt|csv|yaml|yml|log|py|sh))')
    # Only flag inside Verify blocks or shell command blocks
    in_verify = False
    in_code_block = False
    for i, line in enumerate(lines, 1):
        stripped = line.strip()
        if stripped.startswith('**Verify:**') or stripped.startswith('```bash') or stripped.startswith('```shell') or stripped.startswith('```sh'):
            in_verify = True
            in_code_block = stripped.startswith('```')
        if in_code_block and stripped == '```':
            in_code_block = False
            in_verify = False
        if in_verify:
            m = verify_pattern.search(line)
            if m:
                path = m.group(1)
                # Allow paths starting with $, ~, or /
                if not path.startswith(('/', '$', '~')):
                    violations.append((i, line.rstrip()))
    return violations


def check_deprecated_models(lines):
    """Deprecated model IDs that have reached end-of-life."""
    violations = []
    deprecated = [
        'claude-3-5-haiku-20241022',
        'claude-3-haiku-20240307',
        'claude-3-5-sonnet-20241022',
        'claude-3-5-sonnet-20240620',
    ]
    for i, line in enumerate(lines, 1):
        for model in deprecated:
            if model in line:
                # Skip lines that are just documenting the deprecation
                lower = line.lower()
                if 'deprecated' in lower or 'end-of-life' in lower or 'eol' in lower:
                    continue
                violations.append((i, line.rstrip()))
                break
    return violations


def check_ls_for_existence(lines):
    """Verify commands should use 'test -f' not 'ls' for file existence."""
    violations = []
    # Match ls used to check if a file exists in verify blocks
    pattern = re.compile(r'\bls\s+\S+\.(md|json|txt|csv|yaml|yml|log|py|sh)\b')
    in_verify = False
    in_code_block = False
    for i, line in enumerate(lines, 1):
        stripped = line.strip()
        if stripped.startswith('**Verify:**'):
            in_verify = True
        if stripped.startswith('```bash') or stripped.startswith('```shell') or stripped.startswith('```sh'):
            in_code_block = True
        if in_code_block and stripped == '```':
            in_code_block = False
        # Check in verify lines and code blocks
        if in_verify or in_code_block:
            if pattern.search(line):
                # Skip ls used for listing dirs or piped to wc (counting files)
                if '| wc' in line:
                    continue
                violations.append((i, line.rstrip()))
        # Reset verify flag at next heading or blank line after non-verify content
        if in_verify and not in_code_block and stripped == '':
            in_verify = False
    return violations


def check_parallel_same_file_warning(lines):
    """Warn if spec mentions modifying files that other specs might touch."""
    warnings = []
    # Look for explicit mentions of files being modified
    # This is a heuristic: can't fully check without queue access
    modify_pattern = re.compile(r'(?:modify|edit|update|write to|overwrite)\s+[`"]?([/~$]\S+\.\w+)')
    files_mentioned = []
    for i, line in enumerate(lines, 1):
        m = modify_pattern.search(line.lower())
        if m:
            files_mentioned.append((i, m.group(1), line.rstrip()))
    if len(files_mentioned) > 0:
        # Can't check queue without access, just flag for awareness
        for i, fpath, text in files_mentioned:
            warnings.append((i, f"[file: {fpath}] {text}"))
    return warnings


CHECKS = [
    (
        'GREP_PERL',
        'No grep -P or grep -oP (macOS BSD grep lacks Perl regex)',
        'learnings.md: macOS BSD grep does not support -P. Use grep -oE instead. (2026-03-24)',
        check_grep_perl_regex,
        'error',
    ),
    (
        'ENV_GREP',
        'No env | grep (leaks credentials in logs)',
        'learnings.md: BOI workers must never run env | grep. Use ${VAR:+set} checks. (2026-03-24)',
        check_env_grep_leak,
        'error',
    ),
    (
        'RELATIVE_PATH',
        'No relative paths for output files in verify commands',
        'learnings.md: BOI workers need absolute paths for output files.',
        check_relative_output_paths,
        'error',
    ),
    (
        'DEPRECATED_MODEL',
        'No deprecated model IDs',
        'learnings.md: claude-3-5-haiku-20241022 EOL 2026-02-19. Use claude-haiku-4-5-20251001. (2026-03-24)',
        check_deprecated_models,
        'error',
    ),
    (
        'LS_EXISTENCE',
        'Verify commands should use test -f, not ls, for file existence',
        'Best practice: test -f is the correct idiom for file existence checks.',
        check_ls_for_existence,
        'error',
    ),
    (
        'PARALLEL_FILE',
        'Check for files this spec modifies (may conflict with parallel specs)',
        'learnings.md: Parallel BOI specs on the same file create code fragmentation. (2026-03-24)',
        check_parallel_same_file_warning,
        'warn',
    ),
]


def validate(spec_path):
    path = Path(spec_path)
    if not path.exists():
        print(f"ERROR: File not found: {spec_path}", file=sys.stderr)
        return 2
    if not path.is_file():
        print(f"ERROR: Not a file: {spec_path}", file=sys.stderr)
        return 2

    lines = path.read_text().splitlines()
    errors = []
    warnings = []

    for check_id, description, learning_ref, detector, severity in CHECKS:
        violations = detector(lines)
        if violations:
            entry = {
                'id': check_id,
                'description': description,
                'ref': learning_ref,
                'violations': violations,
                'severity': severity,
            }
            if severity == 'error':
                errors.append(entry)
            else:
                warnings.append(entry)

    # Output
    if not errors and not warnings:
        print(f"PASS  {path.name} -- all checks passed")
        return 0

    if errors:
        print(f"FAIL  {path.name} -- {len(errors)} violation(s) found\n")
        for entry in errors:
            print(f"  [{entry['id']}] {entry['description']}")
            print(f"    Ref: {entry['ref']}")
            for line_num, text in entry['violations']:
                print(f"    L{line_num}: {text}")
            print()

    if warnings:
        label = "WARN" if not errors else "    "
        if not errors:
            print(f"WARN  {path.name} -- {len(warnings)} warning(s)\n")
        else:
            print(f"  Warnings:\n")
        for entry in warnings:
            print(f"  [{entry['id']}] {entry['description']}")
            print(f"    Ref: {entry['ref']}")
            for line_num, text in entry['violations']:
                print(f"    L{line_num}: {text}")
            print()

    return 1 if errors else 0


def main():
    if len(sys.argv) < 2:
        print("Usage: python3 validate-boi-spec.py <spec-path>", file=sys.stderr)
        print("       python3 validate-boi-spec.py <spec-path> [<spec-path> ...]", file=sys.stderr)
        sys.exit(2)

    exit_code = 0
    for spec_path in sys.argv[1:]:
        result = validate(spec_path)
        if result > exit_code:
            exit_code = result

    sys.exit(exit_code)


if __name__ == '__main__':
    main()
