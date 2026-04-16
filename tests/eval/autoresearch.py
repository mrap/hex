#!/usr/bin/env python3
"""hex autoresearch — automated CLAUDE.md optimization loop.

Karpathy's autoresearch pattern applied to prompt optimization:
  1. Run eval → measure baseline
  2. Analyze failures → identify which standing orders need work
  3. Use Claude to generate a mutation to CLAUDE.md
  4. Apply mutation, commit
  5. Re-run eval → measure improvement
  6. Keep if improved (no regressions), revert if not
  7. Repeat until convergence or budget exhausted

Usage:
    python3 autoresearch.py                      # Run loop (default 10 iterations, sonnet)
    python3 autoresearch.py --iterations 50      # More iterations
    python3 autoresearch.py --budget 10.0        # Cost cap in dollars
    python3 autoresearch.py --model haiku         # Cheaper model for mutations
    python3 autoresearch.py --dry-run             # Show what would happen, no Claude calls
    python3 autoresearch.py --focus events        # Only target hex-events routing failures
    python3 autoresearch.py --candidates 3        # Tournament: try 3 mutations, keep best
    python3 autoresearch.py --focus boi --candidates 3  # Focused tournament
"""

import argparse
import copy
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Optional

SCRIPT_DIR = Path(__file__).parent.resolve()
REPO_ROOT = SCRIPT_DIR.parent.parent
CLAUDE_MD = REPO_ROOT / "templates" / "CLAUDE.md"
EVAL_RUNNER = SCRIPT_DIR / "run_eval.py"
RESULTS_LOG = SCRIPT_DIR / "autoresearch-log.jsonl"

# Cost per eval run (11 cases × ~$0.045/case with Sonnet)
COST_PER_EVAL = 0.50
# Cost per mutation generation call
COST_PER_MUTATION = 0.05

# Focus categories: map --focus flag to specific eval case names
FOCUS_CATEGORIES = {
    "events": [
        "route_schedule_to_events", "route_monitoring_to_events",
        "route_reactive_to_events", "hex_events_routing",
    ],
    "boi": [
        "delegation", "route_research_to_boi", "route_build_to_boi",
    ],
    "core": [
        "persistence", "onboarding", "memory_search", "startup_loads_context",
    ],
}

MODEL_ALIASES = {
    "sonnet": "claude-sonnet-4-5",
    "haiku": "claude-haiku-4-5",
    "opus": "claude-opus-4-5",
}


@dataclass
class EvalResult:
    """Result of one eval run."""
    total_cases: int = 0
    passed: int = 0
    failed: int = 0
    errors: int = 0
    per_case: dict = field(default_factory=dict)  # case_name → {passed, checks, response_snippet}

    @property
    def pass_rate(self) -> float:
        return self.passed / self.total_cases if self.total_cases > 0 else 0.0

    @property
    def score_str(self) -> str:
        return f"{self.passed}/{self.total_cases}"


@dataclass
class Iteration:
    """Record of one autoresearch iteration."""
    number: int
    timestamp: str
    hypothesis: str
    mutation: str
    baseline_score: str
    new_score: str
    delta: int  # +/- number of cases
    decision: str  # "KEEP" or "REVERT"
    reason: str
    commit_hash: Optional[str] = None
    cost: float = 0.0


# ── Eval runner ───────────────────────────────────────────────────────────────

def run_eval(model: str = "claude-sonnet-4-5", timeout: int = 180) -> EvalResult:
    """Run the eval harness and parse results."""
    cmd = [
        sys.executable, str(EVAL_RUNNER),
        "--live", "--model", model, "--timeout", str(timeout), "--verbose"
    ]
    result = subprocess.run(
        cmd, capture_output=True, text=True, timeout=timeout * 15,  # total timeout
        cwd=str(REPO_ROOT),
    )
    output = result.stdout + result.stderr
    return parse_eval_output(output)


def parse_eval_output(output: str) -> EvalResult:
    """Parse eval runner output into structured result."""
    er = EvalResult()

    # Parse summary line: "Total    4/11 passed"
    total_match = re.search(r"Total\s+(\d+)/(\d+)\s+passed", output)
    if total_match:
        er.passed = int(total_match.group(1))
        er.total_cases = int(total_match.group(2))
        er.failed = er.total_cases - er.passed

    # Parse per-case results
    # Pattern: "case_name    PASS    1/1" or "case_name    FAIL    0/2"
    for match in re.finditer(r"^(\S+)\s+(PASS|FAIL)\s+(\S+)", output, re.MULTILINE):
        name, status, checks = match.groups()
        if name != "Case" and name != "Total" and name != "-" * 10:
            er.per_case[name] = {
                "passed": status == "PASS",
                "checks": checks,
            }

    # Count errors (timeout, etc.)
    er.errors = output.count("ERROR")

    return er


# ── Failure analysis ──────────────────────────────────────────────────────────

def analyze_failures(result: EvalResult, claude_md_text: str, focus_cases: Optional[list] = None) -> str:
    """Build a failure analysis prompt for Claude."""
    failing = []
    for name, data in result.per_case.items():
        if not data["passed"]:
            if focus_cases and name not in focus_cases:
                continue
            failing.append(f"- {name}: {data['checks']} checks passed")

    if not failing:
        return ""

    return f"""The hex eval harness tests whether the CLAUDE.md operating model makes the agent
behave correctly. Current score: {result.score_str}.

FAILING CASES:
{chr(10).join(failing)}

The main failure pattern: the agent defaults to Claude Code's built-in features
(CronCreate for scheduling, hooks for automation, inline coding for multi-step work)
instead of hex's systems (hex-events for ALL automation, BOI for ALL multi-step work).

CURRENT CLAUDE.md STANDING ORDERS (the section that controls agent behavior):
---
{extract_standing_orders(claude_md_text)}
---

Propose ONE specific, surgical edit to the standing orders that would fix the most
failing cases. The edit should:
1. Be explicit — use NEVER/ALWAYS, not "should" or "prefer"
2. Name the specific Claude Code feature to avoid (CronCreate, hooks, inline coding)
3. Name the hex system to use instead (hex-events, BOI)
4. Be concise — one rule, 2-3 sentences max

Output format (exactly):
RULE_NUMBER: <which SO number to edit, or "NEW" for a new rule>
ORIGINAL: <the original text of the rule, or "N/A" for new>
REPLACEMENT: <the new text>
REASONING: <one sentence why this fixes the failing cases>"""


def extract_standing_orders(text: str) -> str:
    """Extract standing orders section from CLAUDE.md."""
    in_section = False
    lines = []
    for line in text.split("\n"):
        if "## Standing Orders" in line:
            in_section = True
        elif in_section and line.startswith("## ") and "Standing" not in line:
            break
        if in_section:
            lines.append(line)
    return "\n".join(lines)


# ── Mutation generation ───────────────────────────────────────────────────────

def generate_mutation(failure_analysis: str, model: str = "claude-sonnet-4-5") -> dict:
    """Ask Claude to propose a CLAUDE.md mutation. Returns parsed mutation dict."""
    cmd = [
        "claude", "-p", failure_analysis,
        "--model", model,
    ]
    result = subprocess.run(
        cmd, capture_output=True, text=True, timeout=120,
        cwd=str(REPO_ROOT),
    )
    response = result.stdout.strip()
    return parse_mutation_response(response)


def parse_mutation_response(response: str) -> dict:
    """Parse Claude's mutation suggestion."""
    mutation = {
        "rule_number": "",
        "original": "",
        "replacement": "",
        "reasoning": "",
        "raw_response": response,
    }

    for line in response.split("\n"):
        line = line.strip()
        if line.startswith("RULE_NUMBER:"):
            mutation["rule_number"] = line.split(":", 1)[1].strip()
        elif line.startswith("ORIGINAL:"):
            mutation["original"] = line.split(":", 1)[1].strip()
        elif line.startswith("REPLACEMENT:"):
            mutation["replacement"] = line.split(":", 1)[1].strip()
        elif line.startswith("REASONING:"):
            mutation["reasoning"] = line.split(":", 1)[1].strip()

    return mutation


# ── Apply/revert mutations ────────────────────────────────────────────────────

def apply_mutation(claude_md_path: Path, mutation: dict) -> bool:
    """Apply a mutation to CLAUDE.md. Returns True if applied."""
    text = claude_md_path.read_text()

    if mutation["rule_number"] == "NEW":
        # Add new rule at the end of the Core Rules table
        # Find the last row of the core rules table
        pattern = r"(\| 20 \|.*?\|)"
        match = re.search(pattern, text, re.DOTALL)
        if match:
            new_text = text[:match.end()] + f"\n| 21 | {mutation['replacement']} |" + text[match.end():]
            claude_md_path.write_text(new_text)
            return True
    elif mutation["original"] and mutation["original"] != "N/A":
        # Replace existing text
        if mutation["original"] in text:
            new_text = text.replace(mutation["original"], mutation["replacement"], 1)
            claude_md_path.write_text(new_text)
            return True
        # Try fuzzy match — find the line containing the rule number
        rule_num = mutation["rule_number"]
        lines = text.split("\n")
        for i, line in enumerate(lines):
            if f"| {rule_num} |" in line or f"|{rule_num}|" in line:
                # Replace this line's content
                lines[i] = f"| {rule_num} | {mutation['replacement']} |"
                claude_md_path.write_text("\n".join(lines))
                return True

    return False


def git_commit(message: str) -> Optional[str]:
    """Commit current changes. Returns commit hash or None."""
    subprocess.run(["git", "add", str(CLAUDE_MD)], cwd=str(REPO_ROOT), capture_output=True)
    result = subprocess.run(
        ["git", "commit", "-m", message],
        cwd=str(REPO_ROOT), capture_output=True, text=True,
    )
    if result.returncode != 0:
        return None
    # Extract commit hash
    hash_match = re.search(r"\[[\w/-]+ ([a-f0-9]+)\]", result.stdout)
    return hash_match.group(1) if hash_match else "unknown"


def git_revert():
    """Revert last commit."""
    subprocess.run(
        ["git", "reset", "--hard", "HEAD~1"],
        cwd=str(REPO_ROOT), capture_output=True,
    )


# ── Logging ───────────────────────────────────────────────────────────────────

def log_iteration(iteration: Iteration):
    """Append iteration record to JSONL log."""
    record = {
        "number": iteration.number,
        "timestamp": iteration.timestamp,
        "hypothesis": iteration.hypothesis,
        "mutation": iteration.mutation,
        "baseline": iteration.baseline_score,
        "result": iteration.new_score,
        "delta": iteration.delta,
        "decision": iteration.decision,
        "reason": iteration.reason,
        "commit": iteration.commit_hash,
        "cost": iteration.cost,
    }
    with open(RESULTS_LOG, "a") as f:
        f.write(json.dumps(record) + "\n")


# ── Keep/Revert decision ─────────────────────────────────────────────────────

def should_keep(baseline: EvalResult, current: EvalResult) -> tuple[bool, str]:
    """Decide whether to keep or revert a mutation."""
    delta = current.passed - baseline.passed

    # Must improve
    if delta <= 0:
        return False, f"No improvement ({baseline.score_str} → {current.score_str})"

    # Check for regressions: cases that WERE passing but NOW fail
    regressions = []
    for name, data in baseline.per_case.items():
        if data["passed"] and name in current.per_case and not current.per_case[name]["passed"]:
            regressions.append(name)

    if regressions:
        return False, f"Regressions in: {', '.join(regressions)}"

    return True, f"Improved {baseline.score_str} → {current.score_str} (+{delta}), no regressions"


# ── Main loop ─────────────────────────────────────────────────────────────────

def run_tournament(
    candidates_count: int,
    best_result: EvalResult,
    claude_md_path: Path,
    model: str,
    eval_timeout: int,
    focus_cases: Optional[list] = None,
    dry_run: bool = False,
) -> tuple[Optional[dict], Optional[EvalResult], float]:
    """Generate N mutation candidates, eval each, return the best.

    Returns (best_mutation, best_eval_result, cost) or (None, None, cost) if none improved.
    """
    claude_md_text = claude_md_path.read_text()
    original_text = claude_md_text  # Save for restoration

    analysis = analyze_failures(best_result, claude_md_text, focus_cases)
    if not analysis:
        return None, None, 0.0

    cost = 0.0
    candidates = []

    # Generate N different mutations
    for c in range(candidates_count):
        if dry_run:
            mutation = {
                "rule_number": f"S{c+1}",
                "original": "example",
                "replacement": f"candidate {c+1} replacement",
                "reasoning": f"dry-run candidate {c+1}",
                "raw_response": "",
            }
        else:
            # Vary the analysis prompt slightly for diversity
            varied_analysis = analysis
            if c > 0:
                varied_analysis += f"\n\nIMPORTANT: Propose a DIFFERENT mutation than: {candidates[-1]['reasoning'] if candidates else 'N/A'}. Try a different rule or a different approach."
            mutation = generate_mutation(varied_analysis, model=model)
            cost += COST_PER_MUTATION

        candidates.append(mutation)
        print(f"    Candidate {c+1}: Rule {mutation.get('rule_number', '?')} — {mutation.get('reasoning', '')[:80]}")

    # Eval each candidate
    best_mutation = None
    best_eval = None
    best_delta = 0

    for c, mutation in enumerate(candidates):
        print(f"    Evaluating candidate {c+1}/{len(candidates)}...")

        if dry_run:
            # Simulate improvement for first candidate only
            sim_passed = best_result.passed + (1 if c == 0 else 0)
            eval_result = EvalResult(total_cases=best_result.total_cases, passed=sim_passed, failed=best_result.total_cases - sim_passed, per_case=best_result.per_case)
        else:
            # Apply mutation temporarily
            applied = apply_mutation(claude_md_path, mutation)
            if not applied:
                print(f"    Candidate {c+1}: could not apply, skipping")
                continue

            # Run eval
            eval_result = run_eval(model=model, timeout=eval_timeout)
            cost += COST_PER_EVAL

            # Restore original CLAUDE.md for next candidate
            claude_md_path.write_text(original_text)

        delta = eval_result.passed - best_result.passed

        # Check for regressions
        regressions = []
        for name, data in best_result.per_case.items():
            if data["passed"] and name in eval_result.per_case and not eval_result.per_case[name]["passed"]:
                regressions.append(name)

        if delta > best_delta and not regressions:
            best_delta = delta
            best_mutation = mutation
            best_eval = eval_result

        status = f"+{delta}" if delta > 0 else str(delta)
        regr = f" (regressions: {', '.join(regressions)})" if regressions else ""
        print(f"    Candidate {c+1}: {eval_result.score_str} ({status}){regr}")

    return best_mutation, best_eval, cost


def run_loop(
    max_iterations: int = 10,
    budget: float = 50.0,
    model: str = "claude-sonnet-4-5",
    eval_timeout: int = 180,
    dry_run: bool = False,
    focus: Optional[str] = None,
    candidates: int = 1,
):
    focus_cases = FOCUS_CATEGORIES.get(focus) if focus else None

    print("=" * 60)
    print(" hex autoresearch — CLAUDE.md optimization loop")
    print("=" * 60)
    print(f"  Model          : {model}")
    print(f"  Max iterations : {max_iterations}")
    print(f"  Budget         : ${budget:.2f}")
    print(f"  Eval timeout   : {eval_timeout}s per case")
    print(f"  Focus          : {focus or 'all'}{f' ({len(focus_cases)} cases)' if focus_cases else ''}")
    print(f"  Candidates     : {candidates} per iteration {'(tournament)' if candidates > 1 else '(single)'}")
    print(f"  CLAUDE.md      : {CLAUDE_MD}")
    print(f"  Log            : {RESULTS_LOG}")
    print()

    if not CLAUDE_MD.exists():
        print("ERROR: CLAUDE.md not found")
        return 1

    if not shutil.which("claude"):
        print("ERROR: claude CLI not found")
        return 1

    # Create branch for autoresearch
    branch = f"autoresearch/{datetime.now().strftime('%Y%m%d-%H%M%S')}"
    subprocess.run(
        ["git", "checkout", "-b", branch],
        cwd=str(REPO_ROOT), capture_output=True,
    )
    print(f"  Branch         : {branch}")
    print()

    cumulative_cost = 0.0
    consecutive_reverts = 0
    best_result = None

    # Step 0: Baseline
    print("[0] Running baseline eval...")
    if dry_run:
        baseline = EvalResult(
            total_cases=11, passed=4, failed=7,
            per_case={
                "onboarding": {"passed": True, "checks": "1/1"},
                "memory_search": {"passed": True, "checks": "1/1"},
                "startup_loads_context": {"passed": True, "checks": "1/1"},
                "route_build_to_boi": {"passed": True, "checks": "2/2"},
                "delegation": {"passed": False, "checks": "0/1"},
                "hex_events_routing": {"passed": False, "checks": "0/1"},
                "persistence": {"passed": False, "checks": "0/2"},
                "route_monitoring_to_events": {"passed": False, "checks": "1/2"},
                "route_reactive_to_events": {"passed": False, "checks": "1/2"},
                "route_research_to_boi": {"passed": False, "checks": "1/2"},
                "route_schedule_to_events": {"passed": False, "checks": "1/2"},
            },
        )
        print(f"  [dry-run] Simulated baseline: {baseline.score_str}")
    else:
        baseline = run_eval(model=model, timeout=eval_timeout)
        cumulative_cost += COST_PER_EVAL
        print(f"  Baseline: {baseline.score_str} (${cumulative_cost:.2f} spent)")

    best_result = copy.deepcopy(baseline)
    print()

    for i in range(1, max_iterations + 1):
        print(f"{'=' * 60}")
        print(f"  ITERATION {i}/{max_iterations}")
        print(f"  Best so far: {best_result.score_str} | Budget: ${budget - cumulative_cost:.2f} remaining")
        print(f"{'=' * 60}")

        # Budget check
        iteration_cost = COST_PER_EVAL + COST_PER_MUTATION
        if cumulative_cost + iteration_cost > budget:
            print(f"  STOP: Budget would be exceeded (${cumulative_cost:.2f} + ${iteration_cost:.2f} > ${budget:.2f})")
            break

        # Consecutive revert check
        if consecutive_reverts >= 3:
            print("  STOP: 3 consecutive reverts — mutations aren't helping. Need a different strategy.")
            break

        # Convergence check
        if best_result.passed == best_result.total_cases:
            print(f"  STOP: Perfect score ({best_result.score_str}). Nothing left to improve.")
            break

        # Identify failures
        failing_cases = [n for n, d in best_result.per_case.items() if not d["passed"]]
        if focus_cases:
            failing_cases = [n for n in failing_cases if n in focus_cases]

        if not failing_cases:
            print(f"\n  No {'focused ' if focus else ''}failures to fix. Done!")
            break

        print(f"\n  Targeting: {', '.join(failing_cases)}")

        if candidates > 1:
            # Tournament mode: generate N candidates, eval each, keep best
            print(f"\n  [Tournament] Generating {candidates} candidates...")
            winner_mutation, winner_result, tournament_cost = run_tournament(
                candidates_count=candidates,
                best_result=best_result,
                claude_md_path=CLAUDE_MD,
                model=model,
                eval_timeout=eval_timeout,
                focus_cases=focus_cases,
                dry_run=dry_run,
            )
            cumulative_cost += tournament_cost

            if winner_mutation and winner_result:
                # Apply winning mutation and commit
                apply_mutation(CLAUDE_MD, winner_mutation)
                commit_hash = git_commit(
                    f"autoresearch: {winner_mutation.get('reasoning', 'mutation')[:72]}"
                ) if not dry_run else "dry-run"

                delta = winner_result.passed - best_result.passed
                print(f"\n  Tournament winner: {winner_result.score_str} (+{delta})")
                print(f"  ✓ KEEP — {winner_mutation.get('reasoning', '')[:100]}")
                best_result = copy.deepcopy(winner_result)
                consecutive_reverts = 0

                log_iteration(Iteration(
                    number=i, timestamp=datetime.now().isoformat(),
                    hypothesis=winner_mutation.get("reasoning", ""),
                    mutation=winner_mutation.get("replacement", "")[:200],
                    baseline_score=f"{best_result.passed - delta}/{best_result.total_cases}",
                    new_score=winner_result.score_str,
                    delta=delta, decision="KEEP", reason=f"Tournament winner (+{delta})",
                    commit_hash=commit_hash, cost=cumulative_cost,
                ))
            else:
                print(f"\n  ✗ No candidate improved. Reverting all.")
                consecutive_reverts += 1
                log_iteration(Iteration(
                    number=i, timestamp=datetime.now().isoformat(),
                    hypothesis="tournament", mutation="none improved",
                    baseline_score=best_result.score_str, new_score=best_result.score_str,
                    delta=0, decision="REVERT", reason="No tournament candidate improved",
                    cost=cumulative_cost,
                ))
        else:
            # Single mutation mode (original behavior)
            print(f"\n  [1/4] Analyzing failures...")
            claude_md_text = CLAUDE_MD.read_text()
            analysis = analyze_failures(best_result, claude_md_text, focus_cases)
            if not analysis:
                print("  No failures to analyze. Done!")
                break

            print(f"  [2/4] Generating mutation...")
            if dry_run:
                mutation = {
                    "rule_number": "S4", "original": "example",
                    "replacement": "example replacement", "reasoning": "dry-run",
                    "raw_response": "",
                }
            else:
                mutation = generate_mutation(analysis, model=model)
                cumulative_cost += COST_PER_MUTATION

            print(f"  Target: Rule {mutation.get('rule_number', '?')}")
            print(f"  [3/4] Applying mutation...")
            if dry_run:
                commit_hash = "dry-run"
            else:
                applied = apply_mutation(CLAUDE_MD, mutation)
                if not applied:
                    print(f"  SKIP: Could not apply")
                    consecutive_reverts += 1
                    continue
                commit_hash = git_commit(f"autoresearch: {mutation.get('reasoning', '')[:72]}")

            print(f"  [4/4] Running eval...")
            if dry_run:
                new_result = EvalResult(total_cases=11, passed=best_result.passed + 1, failed=best_result.failed - 1, per_case=best_result.per_case)
            else:
                new_result = run_eval(model=model, timeout=eval_timeout)
                cumulative_cost += COST_PER_EVAL

            keep, reason = should_keep(best_result, new_result)
            delta = new_result.passed - best_result.passed

            if keep:
                print(f"  ✓ KEEP — {reason}")
                best_result = copy.deepcopy(new_result)
                consecutive_reverts = 0
            else:
                print(f"  ✗ REVERT — {reason}")
                if not dry_run:
                    git_revert()
                consecutive_reverts += 1

            log_iteration(Iteration(
                number=i, timestamp=datetime.now().isoformat(),
                hypothesis=mutation.get("reasoning", ""),
                mutation=mutation.get("replacement", "")[:200],
                baseline_score=best_result.score_str if not keep else f"{best_result.passed - delta}/{best_result.total_cases}",
                new_score=new_result.score_str, delta=delta,
                decision="KEEP" if keep else "REVERT", reason=reason,
                commit_hash=commit_hash, cost=cumulative_cost,
            ))

        print(f"  Cost so far: ${cumulative_cost:.2f}")
        print()

    # Summary
    print()
    print("=" * 60)
    print(" AUTORESEARCH COMPLETE")
    print("=" * 60)
    print(f"  Baseline      : {baseline.score_str}")
    print(f"  Final best    : {best_result.score_str}")
    print(f"  Improvement   : +{best_result.passed - baseline.passed} cases")
    print(f"  Total cost    : ${cumulative_cost:.2f}")
    print(f"  Branch        : {branch}")
    print(f"  Log           : {RESULTS_LOG}")
    print()

    if best_result.passed > baseline.passed:
        print(f"  To merge improvements: git checkout main && git merge {branch}")
    else:
        print(f"  No improvements found. Delete branch: git branch -D {branch}")

    return 0 if best_result.passed > baseline.passed else 1


def main():
    parser = argparse.ArgumentParser(description="hex autoresearch — CLAUDE.md optimization loop")
    parser.add_argument("--iterations", type=int, default=10, help="Max iterations (default: 10)")
    parser.add_argument("--budget", type=float, default=50.0, help="Cost budget in dollars (default: 50)")
    parser.add_argument("--model", default="sonnet", help="Model for eval + mutations (default: sonnet)")
    parser.add_argument("--timeout", type=int, default=180, help="Eval timeout per case in seconds")
    parser.add_argument("--dry-run", action="store_true", help="Simulate without Claude calls")
    parser.add_argument("--focus", default=None, help="Focus category: events, boi, core (or comma-separated case names)")
    parser.add_argument("--candidates", type=int, default=1, help="Tournament size per iteration (default: 1)")
    args = parser.parse_args()

    model = MODEL_ALIASES.get(args.model, args.model)

    # Parse focus: could be a category name or comma-separated case names
    focus = args.focus
    if focus and "," in focus:
        # Raw case names passed (from parallel launcher)
        FOCUS_CATEGORIES[focus] = focus.split(",")

    sys.exit(run_loop(
        max_iterations=args.iterations,
        budget=args.budget,
        model=model,
        eval_timeout=args.timeout,
        dry_run=args.dry_run,
        focus=focus,
        candidates=args.candidates,
    ))


if __name__ == "__main__":
    main()
