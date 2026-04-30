---
name: boi-delegation
description: >
  Delegate tasks to BOI. Covers spec generation, dispatch, context
  wiring, and completion notification via hex-events. Use when delegating any
  non-trivial task — code changes, research, generation, or multi-step work.
tags: boi, delegation, dispatch, hex-events, specs
trigger: >
  User asks to delegate work, dispatch a task, or you need to send work to BOI.
version: 1
---

# BOI Delegation

## BOI Is the Default — Always

BOI is the default for ALL non-trivial work. Not just code — research, analysis,
creative synthesis, brainstorming, everything. Mike: "We should always delegate to BOI."

BOI provides what inline work cannot:
- **Critic** — quality gate that catches gaps, weak verification, incomplete work
- **Self-evolution** — workers discover and add tasks you couldn't foresee
- **Discover mode** — execute + add new tasks (recommended for complex work)
- **Generate mode** — full creative authority from a goal-only spec
- **Compound discovery** — each iteration builds on the last

**The only exception:** subagents (Agent tool) for quick bounded questions where you need
the answer *right now* in this conversation and the full answer fits in context.
Examples: "summarize this file", "what's the status of X", "parse this output."

If in doubt, BOI. The critic and self-evolution justify the async cost.

## Spec Strategy: Goals over Instructions

**Evidence:** arXiv 2603.28990 (25,000 tasks, 8 models, 256 agents) found that
**minimal scaffolding + autonomous role selection** outperforms rigid hierarchy by
14-44%. The "endogeneity paradox": neither max control nor max freedom wins.

**Application to BOI:**
- **Prefer Generate specs** (goal-based) over Execute specs (task-based) when using
  strong models (Sonnet, Opus). Let the agent decompose the work.
- **Use Execute specs** for weak models or when the exact implementation steps are
  known and critical (no room for interpretation).
- **Provide goals + constraints + success criteria**, not step-by-step instructions.
  Workers choose HOW to accomplish each task.
- **Exception:** Security-critical or data-destructive work should use explicit Execute
  specs regardless of model capability.

## Architecture Overview

### Runtime System

BOI uses Claude Code as its worker runtime (`lib/runtime.py`).

- **`claude`** (default) — `claude -p "$(cat prompt)" --model <model> --add-dir ${AGENT_DIR}`
  Workers get CLAUDE.md, skills, commands, and memory search via the hex workspace.

Config: `~/.boi/config.json` → `"runtime": {"default": "claude"}`

### Context Flow
```
config.json: "context_root": "${AGENT_DIR}"
    ↓
worker.py: load_context_root() → self.context_root
    ↓
runtime.py: --add-dir ${AGENT_DIR} passed to Claude CLI
    ↓
Worker gets CLAUDE.md, skills, commands, memory search
```

### Event Flow (BOI → hex-events)
```
BOI daemon (daemon.py)
    ↓ emit_hex_event()
~/.hex-events/hex_emit.py → events.db (SQLite)
    ↓ hex_eventd.py polls
Policies in ~/.hex-events/policies/ fire actions
```

### Events BOI Emits
- `boi.spec.dispatched` — on dispatch (from boi.sh and cli_ops.py)
- `boi.spec.completed` — on successful completion (from daemon.py)
- `boi.spec.failed` — on failure (from daemon.py)
- `boi.iteration.done` — after each iteration (from daemon.py)
- `boi.workspace.leak` — workspace guard violations (from workspace_guard.py)

### Existing Policies Reacting to BOI Events
- `boi-auto-commit` — commits + pushes on `boi.spec.completed`
- `boi-completion-gate` — verifies tests after commit
- `boi-landings-bridge` — updates landings file on dispatch/complete/fail
- `boi-next-action-assessment` — assesses next steps on completion
- `ops-failure-pattern` — detects recurring failure patterns

## Spec Format — YAML ONLY

**All specs must be YAML (`.yaml`).** Markdown specs (`.spec.md`) are rejected by `boi dispatch`.

### Standard Spec
```yaml
title: "Task Title"
mode: execute           # execute | generate | challenge | discover
initiative: init-xyz    # required — links to an initiative
context: |
  Why this work is needed. What the end state looks like.

outcomes:
  - description: "Feature X works end-to-end"
    verify: "curl -s http://localhost:8080/ | grep -q 'expected'"
  - description: "Tests pass"
    verify: "python3 -m pytest tests/ -q"

tasks:
  - id: t-1
    title: "First task"
    status: PENDING
    spec: |
      What to do. Be explicit about files, functions, patterns,
      and deliverables. Include exact commands when possible.
    verify: "command to prove the work was done"

  - id: t-2
    title: "Second task"
    status: PENDING
    depends: [t-1]
    spec: |
      What to do next. Dependencies ensure t-1 is DONE first.
    verify: "test -f expected_output.py"
```

### Key Fields
- `mode`: `execute` (do exactly this), `generate` (creative authority), `challenge` (question assumptions), `discover` (find hidden work)
- `initiative`: required — every spec must link to an initiative or use `emergency: true`
- `outcomes`: spec-level verification — checked after all tasks DONE, before COMPLETED
- `tasks[].depends`: intra-spec DAG — task waits until dependencies are DONE
- `tasks[].status`: `PENDING`, `DONE`, `SKIPPED`, `FAILED`

### Self-Evolution
During execution, workers may discover additional work. They add new PENDING tasks to the tasks array, maintaining sequential IDs. The spec grows organically as understanding deepens.

## Dispatch Steps

1. Write spec to a YAML file:
   ```bash
   cat > /tmp/my-spec.yaml << 'EOF'
   title: "Your goal here"
   mode: execute
   initiative: init-xyz
   outcomes:
     - description: "Expected result"
       verify: "test -f expected_output"
   tasks:
     - id: t-1
       title: "First task"
       status: PENDING
       spec: |
         What to do.
       verify: "echo done"
   EOF
   ```

2. Dispatch:
   ```bash
   bash ~/.boi/boi dispatch /tmp/my-spec.yaml [--priority N] [--project <name>]
   ```

3. Monitor:
   ```bash
   bash ~/.boi/boi status
   bash ~/.boi/boi log <queue-id>
   ```

### Dispatch Options
- `--priority N` — lower = higher priority (default: 100)
- `--max-iter N` — max iterations (default: 30)
- `--mode MODE` — execute | challenge | discover | generate
- `--project NAME` — associate with a BOI project
- `--after q-NNN` — block until dependency completes
- `--no-critic` — skip critic validation
- `--source NAME` — event source tag (default: "mike")
- `--dry-run` — validate without dispatching

## Key File Locations
- BOI CLI: `~/.boi/boi`
- Config: `~/.boi/config.json`
- Queue: `~/.boi/queue/`
- Workers: `~/.boi/worktrees/boi-worker-{1..5}`
- BOI source: `~/github.com/mrap/boi/`
- Context injector: `~/github.com/mrap/boi/lib/context_injector.py`
- Preflight context: `~/github.com/mrap/boi/lib/preflight_context.py`
- Spec validator: `~/github.com/mrap/boi/lib/spec_validator.py`
- hex-events emitter: `~/.hex-events/hex_emit.py`
- hex-events policies: `~/.hex-events/policies/`
- hex-events DB: `~/.hex-events/events.db`

## Phase Timeout System

The daemon applies per-phase timeouts from `~/.boi/phases/*.phase.toml`.
These override the daemon's `DEFAULT_WORKER_TIMEOUT` (1800s) and take
precedence unless the spec has an explicit `worker_timeout_seconds`.

**Timeout precedence (checked in order):**
1. Spec-level `worker_timeout_seconds` (from DB, set at dispatch via `--timeout`)
2. Phase config `[worker] timeout` (from `~/.boi/phases/<phase>.phase.toml`)
3. Daemon default (`DEFAULT_WORKER_TIMEOUT = 1800` in `daemon.py`)

**Current phase timeouts (as of 2026-04-03):**
| Phase | Timeout | File |
|-------|---------|------|
| execute | 1800s (30min) | `~/.boi/phases/execute.phase.toml` |
| decompose | 1800s (30min) | `~/.boi/phases/decompose.phase.toml` |
| critic | 300s (5min) | `~/.boi/phases/critic.phase.toml` |
| evaluate | 300s (5min) | `~/.boi/phases/evaluate.phase.toml` |
| review | 300s (5min) | `~/.boi/phases/review.phase.toml` |

**Hot-reload:** The daemon reloads phase configs each poll cycle (detects mtime changes).
But already-running workers keep their old timeout — changes only apply to NEW launches.

**For specs needing more than 30min per task,** override at dispatch time:
```bash
bash ~/.boi/boi dispatch /tmp/spec.md --timeout 3600  # 1 hour
```

## Task Sizing Rule

**Each task must be completable in under 15 minutes of agent work.** This leaves
2x headroom before the 30-minute execute timeout.

If a task involves multiple substantial steps (design + implement + benchmark),
split it into separate tasks. BOI tracks completion at the task level — if a worker
times out mid-task, ALL progress is lost.

Signs a task is too large:
- More than 3 distinct deliverables
- Requires reading/processing more than ~50 files
- Spec section is longer than 20 lines
- Combines design + implementation + benchmarking

**Anti-pattern:** q-401 had 6 monolithic tasks (each 30-60 min). Workers burned all
5 iterations doing reconnaissance, completed 0 tasks. Should have been 15-20 small tasks.

## Research Tool Defaults

When writing BOI specs that involve web research (surveys, competitive analysis, framework comparison, article ingestion):

**Default: Exa Highlights** — use `exa-highlights.py` instead of `web_fetch_exa` for multi-page research. 96% fewer tokens, same retrieval quality. This prevents workers from hitting context ceiling mid-survey.

Include this in research spec task blocks:
```
For web research, use the token-efficient highlights wrapper:
  python3 $AGENT_DIR/.hex/scripts/exa-highlights.py --search "query" --query "focus topic" --num N --compact
For known URLs:
  python3 $AGENT_DIR/.hex/scripts/exa-highlights.py <url> --query "what to extract" --compact
Only use web_fetch_exa (full page) when highlights are insufficient for deep single-page analysis.
```

**When to use full fetch (`web_fetch_exa`):** Single-page deep analysis where you need the complete document (reading a full spec, extracting all code examples, parsing an entire API reference).

**When to use highlights (`exa-highlights.py`):** Surveying 3+ sources, extracting key facts, comparing systems, ingesting articles for signal. This is the 90% case for research specs.

## Debugging BOI Failures

When a spec fails, diagnose systematically:

1. **Check status:** `bash ~/.boi/boi status`
2. **Read the log:** `bash ~/.boi/boi log <queue-id> --full`
3. **Read iteration metadata:** `cat ~/.boi/queue/<queue-id>.iteration-*.json`
   - `exit_code: 124` = timeout (worker killed by `SIGTERM`)
   - `tasks_completed: 0` across all iterations = worker never finished a single task
4. **Check the DB for spec config:** 
   ```python
   python3 -c "
   import sqlite3
   db = sqlite3.connect('$HOME/.boi/boi.db')
   db.row_factory = sqlite3.Row
   row = db.execute('SELECT id, worker_timeout_seconds, max_iterations, status, iteration, consecutive_failures, failure_reason FROM specs WHERE id = ?', ('<queue-id>',)).fetchone()
   for key in row.keys(): print(f'{key}: {row[key]}')
   "
   ```
5. **Check what the running daemon actually passes:**
   ```bash
   ps aux | grep worker.py  # look for --timeout flag value
   ```
6. **Check phase config:** `cat ~/.boi/phases/execute.phase.toml`
7. **Verify daemon source matches expectations:**
   The running daemon is at `~/github.com/mrap/boi/daemon.py` (the repo),
   NOT the worktree. Worktree code may differ from what's actually running.

### Common Failure Patterns

- **Exit code 124, 0 tasks completed across all iterations:** Timeout too short.
  The agent spends its entire budget on reconnaissance and never reaches productive work.
  Fix: increase timeout via `--timeout` at dispatch or bump the phase config.
- **Agent re-reads irrelevant files every iteration:** Each iteration starts fresh with
  no memory of prior runs. The spec prompt should be directive about what context to
  read (and what to skip). Overly broad specs cause agents to explore the entire workspace.
- **5 consecutive failures → auto-failed:** The daemon marks specs as failed after
  `consecutive_failures` reaches the threshold. The spec must be re-dispatched after fixing
  the root cause.

### Tracing Timeout Issues End-to-End

When a spec dies to timeout, trace the ACTUAL value through the full chain:

1. **Check what the running worker got:**
   ```bash
   ps aux | grep "worker.py q-NNN" | grep -v grep
   # Look for --timeout flag and its value
   ```
2. **Check spec-level override in DB:**
   ```python
   python3 -c "
   import sqlite3
   db = sqlite3.connect('$HOME/.boi/boi.db')
   db.row_factory = sqlite3.Row
   row = db.execute('SELECT worker_timeout_seconds FROM specs WHERE id = ?', ('q-NNN',)).fetchone()
   print(row['worker_timeout_seconds'])  # None = no override
   "
   ```
3. **Check phase config:** `grep timeout ~/.boi/phases/execute.phase.toml`
4. **CRITICAL: The running daemon is from the REPO, not worktrees:**
   ```bash
   ps aux | grep daemon.py  # shows ~/github.com/mrap/boi/daemon.py
   ```
   Worktree code may differ from what's actually running. Always check the repo version
   when debugging daemon behavior.

## Task Sizing Rule
One task = one deliverable completable in <15 min of agent time. Workers get killed at timeout boundaries and ALL in-progress work is lost. Break large work into many small tasks. A spec with 15-20 focused tasks beats one with 6 monoliths. Example anti-pattern: "Design eval framework" (too big) → should be "Create 30 benchmark queries → save JSON" + "Write eval harness that loads JSON → save script" (right-sized).

## Timeout Architecture
- Worker-side: `--timeout` flag from phase config (execute.phase.toml = 1800s as of 2026-04-07)
- Daemon-side: `spec.worker_timeout_seconds` or `DEFAULT_WORKER_TIMEOUT` (1800s)
- Both must align. Phase configs live in `~/.boi/phases/`. Daemon hot-reloads on change.
- If worker burns entire budget on recon without starting work, the spec needs to be more directive.

## Pitfalls

### VERBAL MONITORING = BUG (TC-040)
When user leaves and BOI specs are running, NEVER say "I'll keep an eye on it." You cease to exist when the session ends. Set up MECHANICAL monitoring:
1. Use hex-events to poll `boi status` on a schedule
2. Notify via Slack on failure or completion
3. Silent while running — no noise overnight
This is SO #37 (mechanical action, not verbal) applied to BOI monitoring.
1. **BOI repo is read-only from hex.** Never write to `~/github.com/mrap/boi/` directly.
   Changes go through worktrees.
2. **context_root must exist on disk.** If `${AGENT_DIR}` doesn't exist,
   `load_context_root()` silently returns None and the worker gets no hex context.
3. **BOI's internal events (write_event) go to ~/.boi/events/ as JSON files.** These are
   separate from hex-events (SQLite). The bridge is `emit_hex_event()` in daemon.py which
   calls `hex_emit.py`.
4. **`boi do` translates NL to BOI management commands** (status, cancel, etc.), NOT to
   spec generation. Don't confuse it with spec authoring.
5. **Generate specs reject `### t-N:` headings.** Tasks are created during decompose phase.
9. **Standard spec format requires BARE status on its own line.** The task heading and
   status must be separate lines, and the status must be just the keyword — no markdown
   formatting, no prefix:
   ```
   ### t-1: Task name
   PENDING
   ```
   These all FAIL validation:
   - `### t-1: Task name — PENDING` (inline on heading)
   - `**Status:** PENDING` (markdown bold prefix)
   - `Status: PENDING` (any prefix)
   
   Just write the bare keyword: `PENDING`, `DONE`, `SKIPPED`, `FAILED`, etc.
10. **Python 3.10+ required for BOI.** `boi doctor` checks this. System python3 on macOS
    is 3.9.6 which fails on `dict[str, list[str]] | None` syntax in `spec_parser.py`.
    Fix applied 2026-04-03: patched `~/.boi/boi` with a function override at the top:
    ```bash
    if command -v /opt/homebrew/bin/python3.12 &>/dev/null; then
        python3() { /opt/homebrew/bin/python3.12 "$@"; }
    fi
    ```
    This overrides `python3` within the script only. Alternative: set up PATH globally.
11. **Don't dispatch from queue dir.** If the spec file is already in `~/.boi/queue/`,
    dispatch fails with `SameFileError` (shutil.copy2 source=dest). Fix: copy to /tmp
    first, delete from queue, then dispatch from /tmp:
    ```bash
    cp ~/.boi/queue/q-NNN.yaml /tmp/ && rm ~/.boi/queue/q-NNN.yaml
    bash ~/.boi/boi dispatch /tmp/q-NNN.yaml
    ```
