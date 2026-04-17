#!/usr/bin/env python3
"""hex eval harness — verify the agent follows the operating model.

Usage:
    python3 run_eval.py --dry-run                    # Validate test cases, no Claude calls
    python3 run_eval.py --live                       # Run evals with Claude Code
    python3 run_eval.py --live --case onboarding     # Run specific case
    python3 run_eval.py --live --model sonnet        # Use specific model (default: sonnet)
    python3 run_eval.py --live --timeout 180         # Override per-case timeout (seconds)
"""

import argparse
import fnmatch
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

try:
    import yaml
except ImportError:
    print("ERROR: PyYAML not installed. Run: pip3 install pyyaml")
    sys.exit(1)

# ── Constants ──────────────────────────────────────────────────────────────────

SCRIPT_DIR = Path(__file__).parent.resolve()
REPO_ROOT = SCRIPT_DIR.parent.parent
CASES_DIR = SCRIPT_DIR / "cases"
INSTALL_SH = REPO_ROOT / "install.sh"

DEFAULT_MODEL = "claude-sonnet-4-5"
DEFAULT_TIMEOUT = 300  # seconds per claude call

# Model shorthand → full model ID
MODEL_ALIASES = {
    "sonnet": "claude-sonnet-4-5",
    "haiku": "claude-haiku-4-5",
    "opus": "claude-opus-4-5",
}


# ── Data structures ────────────────────────────────────────────────────────────

@dataclass
class CheckResult:
    name: str
    passed: bool
    evidence: str


@dataclass
class CaseResult:
    case_name: str
    prompt: str
    response_text: str
    checks: list[CheckResult] = field(default_factory=list)
    error: Optional[str] = None
    skipped: bool = False

    @property
    def passed(self) -> bool:
        if self.skipped:
            return True
        if self.error:
            return False
        return all(c.passed for c in self.checks)

    @property
    def check_summary(self) -> str:
        if self.skipped:
            return "SKIP"
        passed = sum(1 for c in self.checks if c.passed)
        return f"{passed}/{len(self.checks)}"


# ── Install helpers ────────────────────────────────────────────────────────────

def fresh_install(target_dir: Path) -> None:
    """Run install.sh into target_dir. Raises on failure."""
    result = subprocess.run(
        ["bash", str(INSTALL_SH), str(target_dir)],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"install.sh failed (exit {result.returncode}):\n"
            f"STDOUT: {result.stdout}\nSTDERR: {result.stderr}"
        )


def apply_seed_data(hex_dir: Path, seed_data: dict) -> None:
    """Write seed files into the hex install."""
    for rel_path, content in seed_data.items():
        target = hex_dir / rel_path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)


def setup_hex(setup_type: str, seed_data: dict, base_tmp: Path) -> Path:
    """
    Create an isolated hex install for a single test case.

    setup_type:
      - 'fresh_install' — vanilla install, no seed data
      - 'populated'     — install + apply seed_data on top
    """
    hex_dir = base_tmp / f"hex-{int(time.time() * 1000)}"
    fresh_install(hex_dir)

    if setup_type == "populated" and seed_data:
        apply_seed_data(hex_dir, seed_data)

    return hex_dir


# ── Check runners ──────────────────────────────────────────────────────────────

def run_response_checks(response_text: str, checks: list[dict]) -> list[CheckResult]:
    results = []
    for check in checks:
        name = check["name"]
        pattern = check.get("pattern", "")
        description = check.get("description", "")

        if not pattern:
            results.append(CheckResult(name=name, passed=True, evidence="(no pattern — skip)"))
            continue

        inverted = check.get("invert", False)
        match = re.search(pattern, response_text)

        if inverted:
            # Inverted check: PASS if pattern is NOT found
            if match:
                snippet = response_text[max(0, match.start() - 40): match.end() + 40].strip()
                snippet = snippet.replace("\n", " ")
                results.append(CheckResult(
                    name=name,
                    passed=False,
                    evidence=f"should NOT match but found '{match.group()}' → ...{snippet}...\ndescription: {description}",
                ))
            else:
                results.append(CheckResult(
                    name=name,
                    passed=True,
                    evidence=f"correctly absent: {pattern!r}",
                ))
        else:
            # Normal check: PASS if pattern IS found
            if match:
                snippet = response_text[max(0, match.start() - 40): match.end() + 40].strip()
                snippet = snippet.replace("\n", " ")
                results.append(CheckResult(
                    name=name,
                    passed=True,
                    evidence=f"matched '{match.group()}' → ...{snippet}...",
                ))
            else:
                results.append(CheckResult(
                    name=name,
                    passed=False,
                    evidence=f"pattern not found: {pattern!r}\ndescription: {description}",
                ))
    return results


def run_file_checks(hex_dir: Path, checks: list[dict]) -> list[CheckResult]:
    results = []
    for check in checks:
        name = check["name"]
        path_pattern = check.get("path_pattern", "")
        check_type = check.get("check", "exists")
        description = check.get("description", "")

        if not path_pattern:
            results.append(CheckResult(name=name, passed=True, evidence="(no path_pattern — skip)"))
            continue

        # Glob match under hex_dir
        matched = list(hex_dir.glob(path_pattern))

        if check_type == "exists":
            if matched:
                results.append(CheckResult(
                    name=name,
                    passed=True,
                    evidence=f"found: {[str(p.relative_to(hex_dir)) for p in matched]}",
                ))
            else:
                results.append(CheckResult(
                    name=name,
                    passed=False,
                    evidence=f"no files matched pattern '{path_pattern}' in {hex_dir}\ndescription: {description}",
                ))

        elif check_type == "contains":
            expected = check.get("content", "")
            found_match = False
            evidence = ""
            for p in matched:
                text = p.read_text()
                if re.search(expected, text):
                    found_match = True
                    evidence = f"found in {p.relative_to(hex_dir)}"
                    break
            if not found_match:
                evidence = (
                    f"pattern {expected!r} not found in any file matching '{path_pattern}'"
                    f"\ndescription: {description}"
                )
            results.append(CheckResult(name=name, passed=found_match, evidence=evidence))

        else:
            results.append(CheckResult(
                name=name,
                passed=False,
                evidence=f"unknown check type: {check_type!r}",
            ))

    return results


# ── Claude runner ──────────────────────────────────────────────────────────────

def run_claude(prompt: str, hex_dir: Path, model: str, timeout: int) -> tuple[str, str]:
    """
    Run `claude -p <prompt> --cwd <hex_dir>` and return (stdout, stderr).
    Raises subprocess.TimeoutExpired or RuntimeError on failure.
    """
    cmd = [
        "claude",
        "-p", prompt,
        "--model", model,
        "--dangerously-skip-permissions",
    ]
    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=timeout,
        cwd=str(hex_dir),
    )
    return result.stdout, result.stderr


# ── Codex runner ───────────────────────────────────────────────────────────────

def run_codex(prompt: str, hex_dir: Path, timeout: int) -> tuple[str, str]:
    """
    Run `codex exec --model codex-mini-latest <prompt>` in hex_dir.
    Returns (stdout, stderr). Returns ("", warning) if OPENAI_API_KEY is absent.
    """
    openai_key = os.environ.get("OPENAI_API_KEY", "")
    if not openai_key:
        return "", "OPENAI_API_KEY not set — Codex live session skipped"

    cmd = ["codex", "exec", "--model", "codex-mini-latest", prompt]
    env = {**os.environ, "OPENAI_API_KEY": openai_key}
    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=timeout,
        cwd=str(hex_dir),
        env=env,
    )
    return result.stdout, result.stderr


def run_shell_script(script_path: Path, timeout: int) -> tuple[str, str]:
    """Run a shell script from REPO_ROOT. Returns (stdout, stderr)."""
    result = subprocess.run(
        ["bash", str(script_path)],
        capture_output=True,
        text=True,
        timeout=timeout,
        cwd=str(REPO_ROOT),
    )
    return result.stdout, result.stderr


# ── YAML loader ────────────────────────────────────────────────────────────────

def load_case(path: Path) -> dict:
    with open(path) as f:
        data = yaml.safe_load(f)

    # Validate required fields (prompt is optional for shell agent cases)
    required = ["name", "description"]
    if data.get("agent", "claude") != "shell":
        required.append("prompt")
    for field_name in required:
        if field_name not in data:
            raise ValueError(f"Case {path.name} missing required field: {field_name!r}")

    # Set defaults
    data.setdefault("agent", "claude")
    data.setdefault("setup", "fresh_install")
    data.setdefault("seed_data", {})
    data.setdefault("prompt", "")
    data.setdefault("response_checks", [])
    data.setdefault("file_checks", [])

    return data


def load_all_cases(case_filter: Optional[str] = None) -> list[dict]:
    cases = []
    for yaml_file in sorted(CASES_DIR.glob("*.yaml")):
        case = load_case(yaml_file)
        if case_filter and case["name"] != case_filter:
            continue
        cases.append(case)
    return cases


# ── Dry-run mode ───────────────────────────────────────────────────────────────

def dry_run(cases: list[dict]) -> int:
    """
    Validate test cases and verify that install.sh works.
    No Claude calls. Returns exit code.
    """
    print("=== hex eval — dry run ===")
    print(f"  Repo root : {REPO_ROOT}")
    print(f"  Cases dir : {CASES_DIR}")
    print(f"  install.sh: {INSTALL_SH}")
    print()

    # Check install.sh exists
    if not INSTALL_SH.exists():
        print("FAIL: install.sh not found")
        return 1
    print(f"  install.sh found  ✓")

    # Check claude CLI
    claude_available = shutil.which("claude") is not None
    if claude_available:
        print("  claude CLI found  ✓")
    else:
        print("  claude CLI not found  (live Claude cases will fail)")

    # Check codex CLI
    codex_available = shutil.which("codex") is not None
    if codex_available:
        print("  codex CLI found  ✓")
    else:
        print("  codex CLI not found  (live Codex cases will fail)")
    print()

    # Validate each case
    print(f"Validating {len(cases)} case(s)...")
    errors = []
    for case in cases:
        name = case["name"]
        try:
            # Validate shell agent cases
            if case.get("agent") == "shell":
                if "script" not in case:
                    raise ValueError("shell agent case missing 'script' field")
                script_path = REPO_ROOT / case["script"]
                if not script_path.exists():
                    raise ValueError(f"shell script not found: {script_path}")

            # Validate response_checks
            for rc in case["response_checks"]:
                if "name" not in rc:
                    raise ValueError("response_check missing 'name'")
                if "pattern" in rc:
                    re.compile(rc["pattern"])  # verify regex compiles

            # Validate file_checks
            for fc in case["file_checks"]:
                if "name" not in fc:
                    raise ValueError("file_check missing 'name'")
                valid_checks = {"exists", "contains"}
                check_type = fc.get("check", "exists")
                if check_type not in valid_checks:
                    raise ValueError(f"invalid check type: {check_type!r}")

            print(f"  {name} PASS (validated)")
        except Exception as e:
            print(f"  {name}: INVALID — {e}")
            errors.append(name)

    print()

    # Test a fresh install
    print("Testing fresh install...")
    with tempfile.TemporaryDirectory(prefix="hex-eval-dryrun-") as tmp:
        tmp_path = Path(tmp)
        try:
            hex_dir = tmp_path / "hex-test"
            fresh_install(hex_dir)
            # Verify key files
            assert (hex_dir / "CLAUDE.md").exists(), "CLAUDE.md missing"
            assert (hex_dir / "me" / "me.md").exists(), "me/me.md missing"
            assert (hex_dir / "todo.md").exists(), "todo.md missing"
            print("  Fresh install OK  ✓")
        except Exception as e:
            print(f"  Fresh install FAILED: {e}")
            errors.append("install")

    print()
    if errors:
        print(f"FAIL: {len(errors)} error(s): {errors}")
        return 1

    print(f"{len(cases)} case(s) validated, ready for live run.")
    print()
    print("To run live:")
    print(f"  python3 {__file__} --live")
    return 0


# ── Live mode ──────────────────────────────────────────────────────────────────

def live_run(cases: list[dict], model: str, timeout: int, verbose: bool = False) -> int:
    """Run cases against real Claude. Returns exit code."""
    # Sandbox guard: --live uses `--dangerously-skip-permissions` and writes to the
    # invoking user's filesystem. Agents can and do write outside the temp hex_dir
    # (e.g. policies to ~/.hex-events/). The shell runners (run_eval_docker.sh,
    # run_eval_macos.sh) set HEX_EVAL_SANDBOXED=1 to opt in. Refuse otherwise.
    if os.environ.get("HEX_EVAL_SANDBOXED") != "1":
        print("ERROR: --live eval must run inside a sandbox (Docker or Tart VM).")
        print("  Agents write outside the temp dir (e.g. policies to ~/.hex-events/)")
        print("  which would contaminate the host environment.")
        print()
        print("Use one of:")
        print("  bash tests/eval/run_eval_docker.sh --live")
        print("  bash tests/eval/run_eval_macos.sh --live")
        print()
        print("To override (dangerous, writes to your real home dir):")
        print("  HEX_EVAL_SANDBOXED=1 python3 tests/eval/run_eval.py --live")
        return 2

    print("=== hex eval — live run ===")
    print(f"  Model   : {model}")
    print(f"  Cases   : {len(cases)}")
    print(f"  Timeout : {timeout}s per case")
    print()

    # Check required CLIs (warn, don't abort — Codex cases may be skipped gracefully)
    if not shutil.which("claude"):
        claude_cases = [c for c in cases if c.get("agent", "claude") == "claude"]
        if claude_cases:
            print("ERROR: claude CLI not found. Install: npm install -g @anthropic-ai/claude-code")
            return 1
    if not shutil.which("codex"):
        codex_cases = [c for c in cases if c.get("agent", "claude") == "codex"]
        if codex_cases:
            print("WARNING: codex CLI not found. Codex cases will produce empty responses.")

    results: list[CaseResult] = []

    with tempfile.TemporaryDirectory(prefix="hex-eval-live-") as tmp:
        tmp_path = Path(tmp)

        for i, case in enumerate(cases, 1):
            name = case["name"]
            agent = case.get("agent", "claude")
            print(f"[{i}/{len(cases)}] {name} [{agent}] — {case['description']}")

            # Shell agent cases run scripts directly — no hex install needed
            if agent == "shell":
                script_rel = case.get("script", "")
                script_path = REPO_ROOT / script_rel
                try:
                    stdout, stderr = run_shell_script(script_path, timeout)
                    response_text = (stdout + stderr).strip()
                except subprocess.TimeoutExpired:
                    result = CaseResult(
                        case_name=name,
                        prompt="",
                        response_text="",
                        error=f"Timed out after {timeout}s",
                    )
                    results.append(result)
                    print(f"  ERROR (timeout): exceeded {timeout}s")
                    continue
                except Exception as e:
                    result = CaseResult(
                        case_name=name,
                        prompt="",
                        response_text="",
                        error=str(e),
                    )
                    results.append(result)
                    print(f"  ERROR (shell): {e}")
                    continue

                response_results = run_response_checks(response_text, case["response_checks"])
                result = CaseResult(
                    case_name=name,
                    prompt="",
                    response_text=response_text,
                    checks=response_results,
                )
                results.append(result)
                for cr in response_results:
                    status = "PASS" if cr.passed else "FAIL"
                    print(f"  [{status}] {cr.name}")
                    if not cr.passed:
                        for line in cr.evidence.splitlines():
                            print(f"         {line}")
                overall = "PASS" if result.passed else "FAIL"
                print(f"  => {overall} ({result.check_summary} checks passed)")
                print()
                continue

            # Setup hex install for Claude/Codex cases
            try:
                hex_dir = setup_hex(case["setup"], case.get("seed_data", {}), tmp_path)
            except Exception as e:
                result = CaseResult(
                    case_name=name,
                    prompt=case["prompt"],
                    response_text="",
                    error=f"Setup failed: {e}",
                )
                results.append(result)
                print(f"  ERROR (setup): {e}")
                continue

            # Dispatch to the appropriate agent runner
            prompt = case["prompt"]

            # Skip codex cases gracefully when OPENAI_API_KEY is absent
            if agent == "codex" and not os.environ.get("OPENAI_API_KEY"):
                result = CaseResult(
                    case_name=name,
                    prompt=prompt,
                    response_text="",
                    skipped=True,
                )
                results.append(result)
                print("  SKIP: OPENAI_API_KEY not set — codex live session skipped")
                print()
                continue

            try:
                if agent == "codex":
                    stdout, stderr = run_codex(prompt, hex_dir, timeout)
                else:
                    stdout, stderr = run_claude(prompt, hex_dir, model, timeout)
                response_text = stdout.strip()
            except subprocess.TimeoutExpired:
                result = CaseResult(
                    case_name=name,
                    prompt=prompt,
                    response_text="",
                    error=f"Timed out after {timeout}s",
                )
                results.append(result)
                print(f"  ERROR (timeout): exceeded {timeout}s")
                continue
            except Exception as e:
                result = CaseResult(
                    case_name=name,
                    prompt=prompt,
                    response_text="",
                    error=str(e),
                )
                results.append(result)
                print(f"  ERROR (claude): {e}")
                continue

            if not response_text:
                response_text = stderr.strip()

            if verbose:
                print(f"  --- Response ({len(response_text)} chars) ---")
                for line in response_text.split("\n")[:15]:
                    print(f"  | {line}")
                if response_text.count("\n") > 15:
                    print(f"  | ... ({response_text.count(chr(10)) - 15} more lines)")
                print(f"  ---")

            # Run checks
            response_results = run_response_checks(response_text, case["response_checks"])
            file_results = run_file_checks(hex_dir, case["file_checks"])
            all_checks = response_results + file_results

            result = CaseResult(
                case_name=name,
                prompt=prompt,
                response_text=response_text,
                checks=all_checks,
            )
            results.append(result)

            # Print check results inline
            for cr in all_checks:
                status = "PASS" if cr.passed else "FAIL"
                print(f"  [{status}] {cr.name}")
                if not cr.passed:
                    for line in cr.evidence.splitlines():
                        print(f"         {line}")

            overall = "PASS" if result.passed else "FAIL"
            checks_summary = result.check_summary
            print(f"  => {overall} ({checks_summary} checks passed)")
            print()

    # Summary table
    print_summary(results)

    failed = sum(1 for r in results if not r.passed)
    return 0 if failed == 0 else 1


def print_summary(results: list[CaseResult]) -> None:
    total = len(results)
    passed = sum(1 for r in results if r.passed)
    failed = total - passed

    col_name = max(len(r.case_name) for r in results) if results else 10
    col_name = max(col_name, 4)

    print("=" * 60)
    print("SUMMARY")
    print("=" * 60)
    header = f"{'Case':<{col_name}}  {'Result':<6}  Checks"
    print(header)
    print("-" * 60)
    for r in results:
        status = "SKIP" if r.skipped else ("PASS" if r.passed else "FAIL")
        checks = r.check_summary if (r.checks or r.skipped) else ("ERROR" if r.error else "—")
        print(f"{r.case_name:<{col_name}}  {status:<6}  {checks}")
        if r.error:
            print(f"  {'':>{col_name}}  {r.error}")
    print("-" * 60)
    print(f"{'Total':<{col_name}}  {passed}/{total} passed")
    print()

    if failed == 0:
        print("All cases passed.")
    else:
        print(f"{failed} case(s) failed.")


# ── Entry point ────────────────────────────────────────────────────────────────

def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="hex eval harness — verify the agent follows the operating model.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--dry-run", action="store_true", help="Validate cases without calling Claude")
    mode.add_argument("--live", action="store_true", help="Run evals with Claude Code")

    parser.add_argument(
        "--case",
        metavar="NAME",
        help="Run only the named case (e.g. --case onboarding)",
    )
    parser.add_argument(
        "--model",
        default=DEFAULT_MODEL,
        metavar="MODEL",
        help=f"Model to use (default: {DEFAULT_MODEL}). Shorthands: sonnet, haiku, opus",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=DEFAULT_TIMEOUT,
        metavar="SECONDS",
        help=f"Per-case Claude timeout in seconds (default: {DEFAULT_TIMEOUT})",
    )
    parser.add_argument(
        "--verbose", "-v",
        action="store_true",
        help="Show full Claude response text for each case",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    # Resolve model alias
    model = MODEL_ALIASES.get(args.model, args.model)

    # Load cases
    cases = load_all_cases(case_filter=args.case)
    if not cases:
        if args.case:
            print(f"ERROR: No case named {args.case!r} found in {CASES_DIR}")
        else:
            print(f"ERROR: No cases found in {CASES_DIR}")
        return 1

    if args.dry_run:
        return dry_run(cases)
    else:
        return live_run(cases, model=model, timeout=args.timeout, verbose=args.verbose)


if __name__ == "__main__":
    sys.exit(main())
