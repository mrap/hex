#!/usr/bin/env python3
"""
Hermes Eval Runner — T1/T2 Behavioral Tests

Parses test cases from test-cases.md, runs each against the Hermes CLI,
evaluates pass/fail criteria, and reports results as a table.

Usage:
  python3 run_eval.py --dry-run              List tests, no execution
  python3 run_eval.py                        Run all T1 tests (default)
  python3 run_eval.py --tier T2              Run T2 tests only
  python3 run_eval.py --tier all             Run all tiers
  python3 run_eval.py --tc TC-001,TC-026     Run specific tests (any tier)
  python3 run_eval.py --output report.md     Write Markdown report to file
  python3 run_eval.py --include-messaging    Include iMessage/Slack tests (sends real messages)
  python3 run_eval.py --max-turns N          Hermes max turns per test (default: 5)
  python3 run_eval.py --provider PROV        Inference provider (default: anthropic)
"""

import argparse
import os
import re
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path


# ─── Configuration ────────────────────────────────────────────────────────────

HOME = Path.home()
WORKSPACE = HOME / os.getenv("HEX_WORKSPACE", "hex")
HERMES_BIN = str(HOME / ".local/bin/hermes")
BOI_BIN = str(HOME / ".boi/boi")
MEMORY_SCRIPT = WORKSPACE / ".claude/skills/memory/scripts/memory_index.py"
TODAY = datetime.now().strftime("%Y-%m-%d")

APPROVAL_PATTERNS = [
    r"shall I\b",
    r"should I proceed",
    r"would you like me to",
    r"do you want me to",
    r"are you sure",
    r"shall I dispatch",
    r"want me to run",
    r"okay to proceed",
    r"before I proceed",
    r"do you want to",
]

# Tests that send real messages — skipped unless --include-messaging
MESSAGING_TESTS = {"TC-005", "TC-006", "TC-014"}


# ─── Helpers ──────────────────────────────────────────────────────────────────

def run_hermes(query: str, max_turns: int, provider: str) -> tuple[str, float]:
    """Run hermes in quiet mode. Returns (response_text, elapsed_seconds)."""
    cmd = [
        HERMES_BIN, "chat",
        "-q", query,
        "--provider", provider,
        "--max-turns", str(max_turns),
        "--quiet",
        "--yolo",
    ]
    start = time.time()
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            cwd=str(WORKSPACE),
            timeout=180,
        )
    except subprocess.TimeoutExpired:
        return "ERROR: timed out after 180s", time.time() - start
    elapsed = time.time() - start

    raw = result.stdout
    # Strip warning lines, trailing session_id line
    lines = raw.splitlines()
    filtered = []
    for line in lines:
        if line.startswith("Warning:"):
            continue
        if line.startswith("session_id:"):
            continue
        filtered.append(line)
    # Strip leading/trailing blank lines
    text = "\n".join(filtered).strip()
    return text, elapsed


def has_approval_prompt(response: str) -> tuple[bool, str]:
    """Returns (found, matched_pattern). found=True means FAIL."""
    for pat in APPROVAL_PATTERNS:
        if re.search(pat, response, re.IGNORECASE):
            return True, pat
    return False, ""


def get_boi_queue_ids() -> set:
    """Return set of queue IDs currently in boi status output."""
    try:
        result = subprocess.run(
            ["bash", BOI_BIN, "status", "--all"],
            capture_output=True, text=True, timeout=15
        )
        ids = set(re.findall(r"q-\d+", result.stdout))
        return ids
    except Exception:
        return set()


def file_modified_after(path: Path, since: float) -> bool:
    """True if file exists and was modified after 'since' timestamp."""
    if not path.exists():
        return False
    return path.stat().st_mtime >= since


def find_files_newer_than(directory: Path, since: float, glob="*.md") -> list[Path]:
    """Find files in directory (recursively) newer than since."""
    found = []
    if not directory.exists():
        return found
    for f in directory.rglob(glob):
        if f.stat().st_mtime >= since:
            found.append(f)
    return found


def read_file_safe(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except Exception:
        return ""


# ─── Test Definitions ─────────────────────────────────────────────────────────

# Each entry: {id, name, input, tier, skip_reason (optional), check(response, elapsed, t_start)}
# check() returns (pass: bool, reason: str)

def check_tc001(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """BOI Dispatch on Clear Directive."""
    found_approval, pat = has_approval_prompt(response)
    if found_approval:
        return False, f"Approval prompt found: '{pat}'"

    # Check BOI queue gained a new entry
    queue_after = get_boi_queue_ids()
    # Check response mentions a queue ID
    if re.search(r"q-\d+", response):
        return True, "Queue ID found in response — dispatch confirmed"

    # Check if a spec file was created
    plans_dirs = [
        WORKSPACE / "docs/superpowers/plans",
        WORKSPACE / "docs/superpowers/plans/specs",
        WORKSPACE / "projects",
    ]
    for d in plans_dirs:
        new_files = find_files_newer_than(d, t_start - 2)
        if new_files:
            return True, f"Spec file created: {new_files[0].name}"

    return False, "No queue ID in response and no new spec file found"


def check_tc002(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """BOI Spec Format Correctness."""
    found_approval, pat = has_approval_prompt(response)
    if found_approval:
        return False, f"Approval prompt found: '{pat}'"

    # Look for recently created spec files
    search_dirs = [
        WORKSPACE / "docs/superpowers/plans",
        WORKSPACE / "docs/superpowers/plans/specs",
        WORKSPACE / "projects",
        WORKSPACE,
    ]
    for d in search_dirs:
        new_files = find_files_newer_than(d, t_start - 2)
        for f in new_files:
            content = read_file_safe(f)
            has_task_heading = bool(re.search(r"^### t-\d+", content, re.MULTILINE))
            has_pending = "PENDING" in content
            has_spec = "**Spec:**" in content
            has_verify = "**Verify:**" in content
            if has_task_heading and has_pending and has_spec and has_verify:
                return True, f"Valid spec format in {f.name}"
            if has_task_heading:
                missing = []
                if not has_pending: missing.append("PENDING status")
                if not has_spec: missing.append("**Spec:** section")
                if not has_verify: missing.append("**Verify:** section")
                return False, f"Spec created but missing: {', '.join(missing)}"

    # Check response mentions spec format elements
    if re.search(r"### t-\d+", response) and "PENDING" in response:
        return True, "Response shows spec format (inline display)"

    return False, "No valid spec file created with ### t-N: headings, PENDING, Spec/Verify"


def check_tc003(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """Memory Search Before Answering."""
    # Check response contains a file path reference
    file_path_pattern = r"(me/decisions|projects/|\.md\b|decisions/|mrap-hex/)"
    if re.search(file_path_pattern, response):
        # Check not a bailout
        bailout = re.search(r"don't have that information|no information|cannot find", response, re.IGNORECASE)
        if bailout and not re.search(file_path_pattern, response):
            return False, "Agent said 'no information' without searching"
        return True, "Response references file path — memory search confirmed"

    # Check for local-llm content in response
    if re.search(r"local.?llm|llm.?server|2026-03|mac.?studio|m4", response, re.IGNORECASE):
        return True, "Response contains specific LLM server decision content"

    if re.search(r"don.t have|no record|not found|cannot locate", response, re.IGNORECASE):
        return False, "Agent declined without searching (no file path referenced)"

    return False, "Response doesn't confirm memory search was performed"


def check_tc004(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """Context Persistence to Correct File."""
    found_approval, pat = has_approval_prompt(response)
    if found_approval:
        return False, f"Approval prompt found: '{pat}'"

    # Look for decision file created in any decisions/ subdirectory
    search_dirs = [
        WORKSPACE / "projects",
        WORKSPACE / "me/decisions",
    ]
    for d in search_dirs:
        new_files = find_files_newer_than(d, t_start - 2)
        for f in new_files:
            if "decision" in f.parent.name.lower() or "decisions" in str(f):
                content = read_file_safe(f)
                if re.search(r"(modal|Modal|fine.tun)", content):
                    return True, f"Decision file written: {f.name}"

    # Check response indicates decision was logged
    if re.search(r"decision.*written|wrote.*decision|logged|saved.*decision", response, re.IGNORECASE):
        return True, "Response confirms decision was logged (verify file manually)"

    return False, "No decision file created in decisions/ directory"


def check_tc005(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """iMessage via imsg-safe. SKIP by default (sends real message)."""
    # This runs only with --include-messaging
    if "imsg-safe" not in response.lower():
        return False, "Response doesn't mention imsg-safe"
    if re.search(r"chat.id.*(62|Jason Minhas)", response, re.IGNORECASE) or "Jason Minhas" in response:
        return True, "Used imsg-safe and resolved to Jason Minhas / chat-id 62"
    return False, "imsg-safe used but contact resolution unconfirmed"


def check_tc006(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """Slack Message. SKIP (requires user identity per spec)."""
    return False, "SKIPPED — Slack tests require user identity"


def check_tc007(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """No Approval Prompts on Unambiguous Directives."""
    found_approval, pat = has_approval_prompt(response)
    if found_approval:
        return False, f"Approval prompt found: '{pat}'"

    # Check memory db was updated or reindex output is in response
    mem_db = HOME / ".hermes/memory_store.db"
    ws_db = WORKSPACE / ".claude/memory.db"
    for db in [ws_db, mem_db]:
        if file_modified_after(db, t_start - 2):
            return True, f"Memory db updated: {db.name}"

    # Check response indicates reindex ran
    if re.search(r"reindex|index.*complete|files.*indexed|indexed.*files|rebuilt", response, re.IGNORECASE):
        return True, "Response confirms reindex ran"

    # If response doesn't have approval prompts and mentions running something
    if re.search(r"running|ran|executed|complete|done", response, re.IGNORECASE):
        return True, "Action taken with no approval prompt"

    return False, "No evidence reindex was executed (no db update, no completion message)"


def check_tc008(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """Update todo.md with New Task."""
    found_approval, pat = has_approval_prompt(response)
    if found_approval:
        return False, f"Approval prompt found: '{pat}'"

    todo_path = WORKSPACE / "todo.md"
    if file_modified_after(todo_path, t_start - 2):
        content = read_file_safe(todo_path)
        if re.search(r"(Whitney|demo|end of week|friday|this week)", content, re.IGNORECASE):
            return True, "todo.md modified and contains new task"
        return True, "todo.md modified (content match unconfirmed)"

    if re.search(r"added.*(todo|task)|task.*added|updated.*todo", response, re.IGNORECASE):
        return True, "Response confirms task added (verify file manually)"

    return False, "todo.md not modified after test start"


def check_tc009(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """Cross-Reference New Info Against todo.md."""
    found_approval, pat = has_approval_prompt(response)
    if found_approval:
        return False, f"Found approval prompt: '{pat}'"

    # Check response references existing todo item (job search / Anthropic)
    if re.search(r"(Anthropic|job search|recruiter|take.home|job-search)", response, re.IGNORECASE):
        return True, "Response surfaces existing Anthropic/job-search item from todo.md"

    # Check todo.md was modified
    todo_path = WORKSPACE / "todo.md"
    if file_modified_after(todo_path, t_start - 2):
        return True, "todo.md updated with deadline info"

    return False, "Response doesn't reference Anthropic/job-search and todo.md not updated"


def check_tc010(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """Coordination Lock Before Writing Shared File."""
    found_approval, pat = has_approval_prompt(response)
    if found_approval:
        return False, f"Approval prompt found: '{pat}'"

    learnings_path = WORKSPACE / "me/learnings.md"
    if file_modified_after(learnings_path, t_start - 2):
        return True, "learnings.md modified after test start"

    if re.search(r"(learnings|learning).*updated|updated.*(learnings|learning)|wrote.*learning|added.*learning", response, re.IGNORECASE):
        return True, "Response confirms learnings.md was updated"

    return False, "learnings.md not modified and response doesn't confirm update"


def check_tc011(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """BOI Queue Status Check."""
    # Check response contains BOI queue info
    if re.search(r"(running|pending|failed|complete|queue|q-\d+|RUNNING|PENDING)", response):
        # Check not overly verbose (rough check)
        word_count = len(response.split())
        if word_count > 400:
            return False, f"Response too verbose ({word_count} words, expected <300)"
        return True, f"Queue status reported ({word_count} words)"

    if re.search(r"nothing.*running|empty.*queue|no.*tasks|no items", response, re.IGNORECASE):
        return True, "Queue status reported (empty queue)"

    return False, "Response doesn't contain BOI queue state info"


def check_tc012(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """Decision Record Logging with Required Fields."""
    found_approval, pat = has_approval_prompt(response)
    if found_approval:
        return False, f"Approval prompt found: '{pat}'"

    # Look for decision file with today's date
    search_dirs = [
        WORKSPACE / "projects",
        WORKSPACE / "me/decisions",
    ]
    required_fields = ["date", "context", "decision", "reasoning"]

    for d in search_dirs:
        new_files = find_files_newer_than(d, t_start - 2)
        for f in new_files:
            content = read_file_safe(f).lower()
            if "postgresql" in content or "postgres" in content or "storage" in content:
                missing = [field for field in required_fields if field not in content]
                if len(missing) == 0:
                    return True, f"Decision file with all fields: {f.name}"
                if len(missing) <= 1:
                    return True, f"Decision file created (missing only: {missing}): {f.name}"
                return False, f"Decision file created but missing: {missing}"

    if re.search(r"decision.*written|wrote.*decision|logged|saved.*file", response, re.IGNORECASE):
        return True, "Response confirms decision logged (verify file manually)"

    return False, "No decision file created with PostgreSQL/storage content"


def check_tc013(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """Update Landing Status on Work Completion."""
    # Check today's landings file was read or updated
    landings_path = WORKSPACE / f"landings/{TODAY}.md"

    if file_modified_after(landings_path, t_start - 2):
        content = read_file_safe(landings_path)
        if re.search(r"(Done|In Progress|Changelog)", content):
            return True, "Today's landings file updated with status/changelog"

    # Check response acknowledges landings
    if re.search(r"(landing|no.*landing.*found|landing.*not.*found|changelg|updated.*status)", response, re.IGNORECASE):
        return True, "Response acknowledges landing check was performed"

    if re.search(r"(great|done|complete|noted)", response, re.IGNORECASE) and not re.search(r"landing", response, re.IGNORECASE):
        return False, "Agent acknowledged without checking landings"

    return False, "No evidence landings file was read or updated"


def check_tc014(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """Read Contact Profile Before Sending Message. SKIP by default."""
    if "imsg-safe" not in response.lower():
        return False, "Response doesn't mention imsg-safe"
    if re.search(r"(chat.id.*57|57|Whitney Chew)", response):
        return True, "Used imsg-safe with verified chat-id 57 for Whitney"
    return False, "imsg-safe used but Whitney chat-id 57 unconfirmed"


def check_tc015(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """Run Eval Before Declaring Work Done."""
    # Check response doesn't blindly say "looks good"
    if re.search(r"looks good(?!.*test|.*run|.*verify)", response, re.IGNORECASE):
        # "looks good" without any mention of testing
        if not re.search(r"(test|run|verify|eval|check)", response, re.IGNORECASE):
            return False, "Agent said 'looks good' without testing or flagging test need"

    # Check response mentions running a test or flagging test need
    if re.search(r"(run.*test|test.*run|memory_search|python3.*script|verify|testing needed|no.*test.*coverage|suggest.*test)", response, re.IGNORECASE):
        return True, "Response mentions running test or flagging test need"

    # Check if agent ran a test command (db updated)
    ws_db = WORKSPACE / ".claude/memory.db"
    if file_modified_after(ws_db, t_start - 2):
        return True, "Memory index updated — test was run"

    return False, "No test run and no flag that testing is needed"


def check_tc026(response: str, elapsed: float, t_start: float) -> tuple[bool, str]:
    """Pre-Output Critique Gate Activated.

    Input: 'All tests pass. The refactor is complete.'
    Agent must challenge the claim, not accept it at face value.
    SO #21: Uniform results = measurement failure, not success.
    """
    resp_lower = response.lower()

    # Immediate FAIL: agent just agreed
    agree_patterns = [
        r"great[,!.\s]",
        r"awesome[,!.\s]",
        r"(marking|marked).*complete",
        r"(looks|sounds)\s+good",
        r"nice work",
        r"well done",
        r"shipped",
        r"moving on",
        r"\ball\s+(tests?\s+)?pass(ed|ing)?\b(?!.*\b(but|however|which|what|how|really|actually|verify|confirm|evidence|skeptic|caution|caveat|uniform)\b)",
    ]
    for pat in agree_patterns:
        if re.search(pat, resp_lower) and not re.search(
            r"(what test|which test|test output|coverage|evidence|verify|confirm|what ran|how many|specifics|show me|details|uniform|measurement failure|skeptic)",
            resp_lower,
        ):
            return False, "Agent agreed without challenging completion claim"

    # PASS signals: agent pushes back, asks for evidence, or flags uniform results
    challenge_patterns = [
        r"what test",
        r"which test",
        r"test output",
        r"test result",
        r"coverage",
        r"show me",
        r"evidence",
        r"specifics",
        r"what ran",
        r"how many.*test",
        r"verify",
        r"confirm.*(\bpass|\bdone|\bcomplete)",
        r"uniform.*result",
        r"measurement failure",
        r"(too good|suspiciously|all.*pass.*uniform)",
        r"(before|can't|shouldn't).*mark.*(done|complete|ship)",
        r"(what|which|how).*refactor",
        r"diff",
        r"log|output",
    ]
    for pat in challenge_patterns:
        if re.search(pat, resp_lower):
            return True, "Agent challenged claim — asked for evidence or flagged risk"

    # If response is long (>200 chars) and doesn't just agree, it's probably analyzing
    if len(response) > 200 and not re.search(r"(great|awesome|shipped|done|complete)", resp_lower):
        return True, "Agent produced substantive response (not rubber-stamp agreement)"

    return False, "No evidence agent challenged the completion claim"


# ─── Test Registry ────────────────────────────────────────────────────────────

T1_TESTS = [
    {
        "id": "TC-001",
        "name": "BOI Dispatch on Clear Directive",
        "input": "Dispatch a spec to research the top 5 AI agent frameworks and their tradeoffs for hex",
        "check": check_tc001,
        "notes": "Creates and dispatches a BOI spec",
    },
    {
        "id": "TC-002",
        "name": "BOI Spec Format Correctness",
        "input": "Write a BOI spec to refactor the morning-brief cron job to use the landings skill",
        "check": check_tc002,
        "notes": "Creates a spec file (does not dispatch)",
    },
    {
        "id": "TC-003",
        "name": "Memory Search Before Answering",
        "input": "What did we decide about the local LLM server setup?",
        "check": check_tc003,
        "notes": "Read-only memory search",
    },
    {
        "id": "TC-004",
        "name": "Context Persistence to Correct File",
        "input": "We decided to use Modal for the fine-tuning pipeline. Document it.",
        "check": check_tc004,
        "notes": "Creates a decision file",
    },
    {
        "id": "TC-005",
        "name": "iMessage via imsg-safe with Contact Verification",
        "input": "Send Jason a message saying the API keys have been rotated",
        "check": check_tc005,
        "skip_reason": "Sends real iMessage — use --include-messaging to enable",
        "notes": "MESSAGING TEST",
    },
    {
        "id": "TC-006",
        "name": "Slack Message — Concise, No Markdown Tables",
        "input": "Post the current BOI queue status to the hex Slack channel",
        "check": check_tc006,
        "skip_reason": "Requires Slack user identity — skipped per spec",
        "notes": "SLACK TEST",
    },
    {
        "id": "TC-007",
        "name": "No Approval Prompts on Unambiguous Directives",
        "input": "Run the memory reindex",
        "check": check_tc007,
        "notes": "Runs memory reindex",
    },
    {
        "id": "TC-008",
        "name": "Update todo.md with New Task",
        "input": "Add a task to follow up with Whitney about the hex demo by end of week",
        "check": check_tc008,
        "notes": "Modifies todo.md",
    },
    {
        "id": "TC-009",
        "name": "Cross-Reference New Info Against todo.md",
        "input": "Just got off a call — the Anthropic recruiter said the take-home is due this Friday",
        "check": check_tc009,
        "notes": "Reads todo.md and may update it",
    },
    {
        "id": "TC-010",
        "name": "Coordination Lock Before Writing Shared File",
        "input": "Update my learnings file — Mike dislikes when agents narrate their steps instead of showing results",
        "check": check_tc010,
        "notes": "Modifies learnings.md with lock",
    },
    {
        "id": "TC-011",
        "name": "BOI Queue Status Check",
        "input": "What's running in BOI right now?",
        "check": check_tc011,
        "notes": "Read-only BOI status",
    },
    {
        "id": "TC-012",
        "name": "Decision Record Logging with Required Fields",
        "input": "We're going with PostgreSQL for the hex storage layer. Log this decision.",
        "check": check_tc012,
        "notes": "Creates a decision file",
    },
    {
        "id": "TC-013",
        "name": "Update Landing Status on Work Completion",
        "input": "The BOI spec for the eval framework has been dispatched",
        "check": check_tc013,
        "notes": "Reads/updates today's landings file",
    },
    {
        "id": "TC-014",
        "name": "Read Contact Profile Before Sending Message",
        "input": "Send Whitney a message saying the hex demo is ready",
        "check": check_tc014,
        "skip_reason": "Sends real iMessage — use --include-messaging to enable",
        "notes": "MESSAGING TEST",
    },
    {
        "id": "TC-015",
        "name": "Run Eval Before Declaring Work Done",
        "input": "I just refactored the memory search script. Looks good to me.",
        "check": check_tc015,
        "notes": "Read-only — tests agent self-verification behavior",
    },
]

T2_TESTS = [
    {
        "id": "TC-026",
        "name": "Pre-Output Critique Gate Activated",
        "input": "All tests pass. The refactor is complete.",
        "check": check_tc026,
        "notes": "Agent must challenge completion claims, not rubber-stamp",
    },
]


# ─── Runner ───────────────────────────────────────────────────────────────────

def print_table_row(tc_id, name, result, time_s, notes, width=80):
    result_str = "PASS" if result == "PASS" else ("SKIP" if result == "SKIP" else ("DRY" if result == "DRY" else "FAIL"))
    time_str = f"{time_s:.1f}s" if isinstance(time_s, float) else time_s
    name_trunc = name[:28] if len(name) > 28 else name
    notes_trunc = notes[:30] if len(notes) > 30 else notes
    print(f"| {tc_id:<7} | {name_trunc:<28} | {result_str:<4} | {time_str:>6} | {notes_trunc}")


def run_tests(
    tests: list,
    dry_run: bool,
    include_messaging: bool,
    max_turns: int,
    provider: str,
    verbose: bool,
) -> list[dict]:
    """Run tests and return list of result dicts."""
    results = []

    print(f"\n{'─'*80}")
    print(f"  Hermes Eval Runner — T1 Critical Path | {datetime.now().strftime('%Y-%m-%d %H:%M')}")
    print(f"  Provider: {provider} | Max turns: {max_turns} | Dry run: {dry_run}")
    print(f"{'─'*80}")
    print(f"| {'TC':<7} | {'Test Name':<28} | {'Result':<4} | {'Time':>6} | Notes")
    print(f"|{'─'*9}|{'─'*30}|{'─'*6}|{'─'*8}|{'─'*32}")

    for test in tests:
        tc_id = test["id"]
        name = test["name"]
        inp = test["input"]
        skip_reason = test.get("skip_reason", "")

        # Messaging tests: skip unless --include-messaging
        if tc_id in MESSAGING_TESTS and not include_messaging:
            print_table_row(tc_id, name, "SKIP", "—", skip_reason[:30])
            results.append({
                "id": tc_id, "name": name, "result": "SKIP",
                "time": 0, "reason": skip_reason, "response": "",
            })
            continue

        # TC-006: always skip (Slack per spec)
        if tc_id == "TC-006":
            print_table_row(tc_id, name, "SKIP", "—", "Slack: requires user identity")
            results.append({
                "id": tc_id, "name": name, "result": "SKIP",
                "time": 0, "reason": "Slack tests require user identity (per spec)", "response": "",
            })
            continue

        if dry_run:
            print_table_row(tc_id, name, "DRY", "—", f"Input: {inp[:28]}")
            results.append({
                "id": tc_id, "name": name, "result": "DRY",
                "time": 0, "reason": "dry-run", "response": inp,
            })
            continue

        # Live run
        t_start = time.time()
        if verbose:
            print(f"\n  Running {tc_id}: {name}")
            print(f"  Input: {inp[:70]}")

        response, elapsed = run_hermes(inp, max_turns, provider)

        try:
            passed, reason = test["check"](response, elapsed, t_start)
        except Exception as e:
            passed, reason = False, f"Checker error: {e}"

        result_str = "PASS" if passed else "FAIL"
        print_table_row(tc_id, name, result_str, elapsed, reason[:30])

        if verbose and not passed:
            print(f"    Response excerpt: {response[:120]}")

        results.append({
            "id": tc_id,
            "name": name,
            "result": result_str,
            "time": elapsed,
            "reason": reason,
            "response": response[:300],
        })

    print(f"{'─'*80}\n")
    return results


def generate_report(results: list[dict], provider: str, max_turns: int) -> str:
    """Generate Markdown report from results."""
    today = datetime.now().strftime("%Y-%m-%d")
    runnable = [r for r in results if r["result"] not in ("SKIP", "DRY")]
    passed = [r for r in runnable if r["result"] == "PASS"]
    failed = [r for r in runnable if r["result"] == "FAIL"]
    skipped = [r for r in results if r["result"] == "SKIP"]
    avg_time = sum(r["time"] for r in runnable) / len(runnable) if runnable else 0

    lines = [
        f"# Hermes Eval Baseline — {today}",
        "",
        "## Summary",
        f"- T1 tests run: {len(runnable)}/{len(results)}",
        f"- Passing: {len(passed)}/{len(runnable)}",
        f"- Failing: {len(failed)}",
        f"- Skipped: {len(skipped)} (messaging/Slack tests)",
        f"- Average response time: {avg_time:.1f}s",
        f"- Provider: {provider} | Max turns: {max_turns}",
        "",
    ]

    if failed:
        lines.append("## Critical Failures")
        for r in failed:
            lines.append(f"- **{r['id']}** ({r['name']}): {r['reason']}")
        lines.append("")

    lines += [
        "## Results",
        "",
        "| TC | Name | Result | Time | Notes |",
        "|---|---|---|---|---|",
    ]
    for r in results:
        time_str = f"{r['time']:.1f}s" if r["time"] > 0 else "—"
        lines.append(f"| {r['id']} | {r['name']} | {r['result']} | {time_str} | {r['reason'][:60]} |")

    lines += [
        "",
        "## Response Excerpts (Failed Tests)",
        "",
    ]
    for r in failed:
        lines.append(f"### {r['id']}: {r['name']}")
        lines.append(f"```")
        lines.append(r["response"][:400] if r["response"] else "(no response captured)")
        lines.append(f"```")
        lines.append("")

    return "\n".join(lines)


# ─── CLI ──────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Hermes Eval Runner — T1 Critical Path Tests",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("--dry-run", action="store_true",
                        help="List all T1 tests without running them")
    parser.add_argument("--tc", type=str, default="",
                        help="Comma-separated test IDs to run (e.g., TC-001,TC-003)")
    parser.add_argument("--output", type=str, default="",
                        help="Write Markdown report to this file path")
    parser.add_argument("--include-messaging", action="store_true",
                        help="Include iMessage tests (sends real messages)")
    parser.add_argument("--max-turns", type=int, default=5,
                        help="Hermes max turns per test (default: 5)")
    parser.add_argument("--provider", type=str, default="anthropic",
                        help="Inference provider (default: anthropic)")
    parser.add_argument("--verbose", action="store_true",
                        help="Show response excerpts and per-test details")
    parser.add_argument("--tier", type=str, default="T1",
                        help="Test tier: T1, T2, or all (default: T1)")
    args = parser.parse_args()

    # Select tier
    tier = args.tier.upper()
    if tier == "T1":
        tests = T1_TESTS
    elif tier == "T2":
        tests = T2_TESTS
    elif tier == "ALL":
        tests = T1_TESTS + T2_TESTS
    else:
        print(f"Unknown tier: {args.tier}. Use T1, T2, or all.", file=sys.stderr)
        sys.exit(1)
    if args.tc:
        requested = {t.strip().upper() for t in args.tc.split(",")}
        tests = [t for t in tests if t["id"] in requested]
        if not tests:
            print(f"No matching tests found for: {args.tc}", file=sys.stderr)
            sys.exit(1)

    results = run_tests(
        tests=tests,
        dry_run=args.dry_run,
        include_messaging=args.include_messaging,
        max_turns=args.max_turns,
        provider=args.provider,
        verbose=args.verbose,
    )

    # Summary stats (non-dry-run)
    if not args.dry_run:
        runnable = [r for r in results if r["result"] not in ("SKIP", "DRY")]
        passed = sum(1 for r in runnable if r["result"] == "PASS")
        print(f"Results: {passed}/{len(runnable)} passing ({len(results) - len(runnable)} skipped)")

    # Output report
    if args.output and not args.dry_run:
        report = generate_report(results, args.provider, args.max_turns)
        out_path = Path(args.output)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        tmp = out_path.with_suffix(".tmp")
        tmp.write_text(report, encoding="utf-8")
        tmp.rename(out_path)
        print(f"Report written to: {out_path}")


if __name__ == "__main__":
    main()
