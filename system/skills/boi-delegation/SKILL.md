---
name: boi-delegation
description: >
  Delegate tasks to BOI v2. Covers TOML spec authoring, dispatch, context
  wiring, and monitoring. Use when delegating any non-trivial task — code
  changes, research, generation, or multi-step work.
tags: boi, delegation, dispatch, specs
trigger: >
  User asks to delegate work, dispatch a task, or you need to send work to BOI.
version: 2
---

> **Updated 2026-07-22 for BOI v3.5.0 (per-phase runtime config) + v3.4.0 (lifecycle hardening).**
> v1's SKILL.md is preserved in git history if you need to see what changed.

# BOI Delegation

## BOI Is the Default — Always

BOI is the default for ALL non-trivial work. Not just code — research, analysis,
creative synthesis, brainstorming, everything.

BOI v2 provides what inline work cannot:
- **Plan + critique phases** — every spec passes through `plan` and a cross-model
  `critique_plan` before any code runs.
- **Deterministic verify gates** — `workspace_verify_in` / `workspace_verify_out`
  bracket every task; verifications run in a clean subshell.
- **DAG-aware scheduling** — `blocked_by` is honored, dependent tasks fan out in
  parallel as predecessors pass.
- **Worktree-per-task isolation** — each task gets its own git worktree; no
  cross-task contamination.
- **Typed reasons everywhere** — every state transition carries a typed reason
  (cancel, fail, block), enforced at the recovery commands.

**The only exception:** subagents (Agent tool) for quick bounded questions where
you need the answer *right now* in this conversation and the full answer fits in
context. Examples: "summarize this file", "what's the status of X", "parse this
output."

If in doubt, BOI. The plan/critique stage and verify gates justify the async cost.

## Pre-Second-Edit Gate (MANDATORY)

Before making your second file edit, STOP. Count how many files you plan to
touch total. If 3 or more → abandon inline editing immediately, write a TOML
spec, dispatch. No exceptions, no "just one more edit."

## Brainstorm → Plan → Dispatch (MANDATORY)

1. **Brainstorm** — List 2-3 approaches. Pick the best. Write it down.
2. **Plan** — Write the spec TOML: `[contract]` block + `[[tasks]]` array.
3. **Dispatch** — `~/.boi/bin/boi dispatch <spec.toml>`

Problem → spec (skipping brainstorm) is not acceptable. The brainstorm step is
not optional overhead — it's a required gate.

**No concept-first flow.** If you are about to type a mockup, prototype, design
concept, or feature outline to present for approval → STOP. Write it into the
spec `[contract].scope` field or a `[[decision]]` block instead. Concepts go
into the spec, not into chat responses.

## Spec Strategy: Behavior Statements over Step Lists

v2 specs describe **what behavior the task must implement**, not how to implement
it. Each `[[tasks]]` entry has one `behavior` field — a single statement of the
outcome. The worker decomposes the steps in the `plan` phase.

- **Behavior, not steps** — `behavior = "Add token-bucket rate limiting to /api/*"`
  beats a 12-bullet implementation checklist.
- **Verifications are the contract** — every task MUST declare at least one
  `verifications` entry (intent or command). That's what gets graded.
- **Spec-level guardrails** — use `[contract].exclusions`, `[contract].must_emit`,
  and `[contract].verifications` to fence the work without micromanaging.
- **Security-critical work** — use `command =` verifications (deterministic,
  shell-evaluated). Reserve `intent =` (LLM-judged) for behavior questions a
  command can't answer.

## Architecture Overview

### Runtime System (Goose-as-adapter)

BOI v2 routes LLM phases (`plan`, `execute`, `review`) through Goose recipes.
Deterministic phases (`workspace_prepare`, `workspace_verify_in`, `validate`,
`commit`, `workspace_verify_out`, `merge_to_integration`, `teardown`) run as
native Rust fn-pointers via the `DETERMINISTIC_STEPS` table. See design doc
§1 "Provider model — Goose-pattern" and the goose-as-adapter amendment
(`~/github.com/mrap/boi/docs/design/2026-05-17-goose-as-runtime-adapter.md`).

### Binary + Data Layout

- Binary: `~/.boi/bin/boi` (the shim; resolves to the v2 binary post-cutover)
- Data dir: `~/.boi/v2/`
  - `boi.db` — SQLite intent + runtime store
  - `phases/<name>.toml` — phase declarations
  - `pipelines/<name>.toml` — phase composition (v1.0 ships `standard.toml` only)
  - `recipes/` — generated Goose recipes per phase run
  - `worktrees/` — one git worktree per task
  - `daemon.sock` — Unix-domain control socket
- Source: `~/github.com/mrap/boi/` (read-only from hex; modify via PR)

### Process Model

`boi daemon` is the single long-running process: orchestrator + event bus +
control-socket listener. Write-side commands (`dispatch`, `cancel`, `unblock`,
`resolve-conflict`, `fail`) are control-socket clients — they fail loud if no
daemon is running. Read-only commands (`log`, `dashboard`, `spec show`,
`traces query`, `failures top`) read SQLite/DuckDB directly.

See `~/github.com/mrap/boi/src/cli/mod.rs:14-29` for the process-model docs.

### Event Flow (BOI observability)

BOI v2 emits OpenTelemetry traces (DuckDB-backed) and writes inbox messages on
spec/task transitions. Query them via `boi traces query "<sql>"` and
`boi failures top --last 7d`. The hex-events event bus has been removed; there
is no policy bridge. Completion detection is via `boi log <spec-id>` or
`boi dashboard`.

## Spec Format — TOML ONLY

**All specs must be TOML (`.toml`).** YAML / Markdown specs are rejected with
typed parse errors. The full grammar lives at
`~/github.com/mrap/boi/src/config/spec.rs` — that file is the canonical
schema reference.

### Required Fields (§3.1)

- `title` — string
- `[contract]` block with:
  - `scope` — string, the sprint-contract anchor
  - `workspace` XOR `workspace_rationale` — exactly one (`workspace = "/path"`
    OR `workspace_rationale = "reason"`)
  - `base_branch` — string, **workspace-conditional**: `"develop"` for GitFlow
    workspaces — repos with a committed `.boi-policy.toml` (`model = "gitflow"`)
    at the root, e.g. boi and hex-foundation; `"main"` for unmanaged workspaces
    (no marker). The workspace's `.boi-policy.toml` marker decides; the engine
    rejects dispatches targeting a protected branch (e.g. a GitFlow `main`)
- At least one `[[tasks]]` entry, each with:
  - `behavior` — string
  - `verifications = [ { intent = "..." } | { command = "..." } ]` — at least one

### Optional Fields

- Top-level: `pipeline = "standard"` (default), `delivery = "merge" | "pr" | "branch-only"` (default `merge`)
- `[contract].exclusions = ["..."]`
- `[contract].verifications = [ { ... } ]` — spec-level checks
- `[contract].must_emit = ["path/to/file"]`
- `[[tasks]].ref = "slug"` — required if other tasks depend on it
- `[[tasks]].blocked_by = ["other-ref"]` — DAG predecessors
- `[overrides.<phase>.runtime]` — per-phase provider/model/effort for THIS spec (v3.5.0, see below)
- `[[decision]]` blocks — authored decisions (§3.6)
- `[[skill]]` blocks — declare skills (e.g. TDD, verification-before-completion)

### REJECTED Fields (typed errors, not silent strips)

`src/config/spec.rs:100-129` rejects each of these with a specific error:

| Field | Error variant | Why removed |
|---|---|---|
| `mode` | `ModesRemoved` | v1.0 has one pipeline (`standard`); no execute/discover/generate split |
| `max_iterations` | `MaxIterationsHardcoded` | Iteration caps hard-coded; per-spec tuning deferred to v1.x |
| `clean_state` | `CleanStateStrict` | Strict clean-state invariants are enforced unconditionally |
| `initiative` | `InitiativeRemoved` | Cross-cutting tracking deferred to v1.x |

Any unrecognized field (typo, drifted-field) is rejected at parse time by
`deny_unknown_fields`.

### Per-Phase Runtime Overrides (v3.5.0)

Every worker phase's LLM settings are configurable at three layers, resolved
field-by-field with precedence **spec override > pipeline override > phase
TOML base** (`~/.boi/v2/phases/<phase>.toml`, base default today:
`claude-opus-4-8`):

```toml
# In a spec TOML — run red-test authoring on a cheaper model for this spec:
[overrides.write_red_tests.runtime]
model = "claude-sonnet-5"

[overrides.critique_plan.runtime]
provider = "openrouter"
model = "openai/gpt-5"
effort = "high"                 # OpenAI reasoning-family only — see rules
extra = { temperature = 0.2 }   # passed verbatim into goose settings
```

Rules that will bite you if ignored:
- **Naming a phase that doesn't exist in the pipeline is a loud parse error**
  at dispatch — check the phase table below for valid names.
- **`effort` is only expressible for OpenAI reasoning-family models** (goose
  wires it via a model-name suffix). The `claude-code` provider has NO effort
  knob in goose — BOI **rejects** `effort` on combos goose can't express
  rather than silently ignoring it. On Claude phases, the cost/quality lever
  is per-phase MODEL choice (opus/sonnet/haiku).
- `extra` accepts scalar keys goose recipes understand today (`temperature`,
  `max_turns`) and passes them through verbatim — future goose settings need
  no BOI change.
- **Model-tier guidance (Standing Order 3b applied to specs):** phase defaults
  run opus-tier; when a spec's work is mechanical (docs batches, config
  sweeps, well-specified small fixes), downshift `write_red_tests` /
  `critique_plan` / `review` to `claude-sonnet-5` via overrides and say so in
  the scope. Leave `execute` on the default unless the whole spec is trivial.
- Survey of what goose recipes actually accept:
  `~/github.com/mrap/boi/docs/research/2026-07-21-goose-recipe-settings.md`.

### Worked Example (the §13 typo-fix fixture)

```toml
# adapted from ~/github.com/mrap/boi/tests/fixtures/specs/01-typo-fix.toml
title = "Fix a typo in the README"

[contract]
scope = "Correct the spelling of 'recieve' to 'receive' in README.md"
# Workspace-conditional: example-repo commits .boi-policy.toml (model = "gitflow"),
# so work lands on develop. Unmanaged workspaces (no marker) use "main".
base_branch = "develop"
workspace = "~/github.com/mrap/example-repo"

[[tasks]]
ref = "fix-typo"
behavior = "Replace the misspelled word 'recieve' with 'receive' in README.md"
verifications = [
  { name = "typo-fixed", command = "grep -q receive README.md && ! grep -q recieve README.md" },
]
```

### DAG Example

```toml
title = "Add token-bucket rate limiting to the API"

[contract]
scope = "Add token-bucket rate limiting middleware to all /api routes"
# Workspace-conditional: example-api is unmanaged (no .boi-policy.toml), so "main"
# is correct here. GitFlow workspaces (e.g. boi, hex-foundation) use "develop".
base_branch = "main"
workspace = "~/github.com/mrap/example-api"
exclusions = ["frontend changes", "auth changes"]
verifications = [
  { intent = "Requests over the limit get a 429 with a Retry-After header" },
]
must_emit = ["src/middleware/token_bucket.rs"]

[[tasks]]
ref = "setup-middleware"
behavior = "Create the token_bucket middleware module"
verifications = [
  { intent = "Unit tests pass for the new middleware::token_bucket module" },
]

[[tasks]]
ref = "apply-middleware"
behavior = "Apply the token_bucket middleware to every /api/* handler"
blocked_by = ["setup-middleware"]
verifications = [
  { intent = "An integration test verifies /api routes return 429 after the limit is hit" },
]
```

## Dispatch + Lifecycle

```bash
~/.boi/bin/boi dispatch /tmp/my-spec.toml
~/.boi/bin/boi log <S-spec-id>
~/.boi/bin/boi dashboard <S-spec-id>     # TUI
~/.boi/bin/boi cancel <id> --reason "..." # --reason is MANDATORY
~/.boi/bin/boi fail <S-spec-id> --reason "..." # operator-marked failure
~/.boi/bin/boi unblock <T-task-id> [--reset-counter]
~/.boi/bin/boi resolve-conflict <T-task-id>    # interactive shell
~/.boi/bin/boi spec show <S-spec-id>
~/.boi/bin/boi clean <S-spec-id> [--force]
~/.boi/bin/boi traces query "<sql>"            # OTel/DuckDB
~/.boi/bin/boi failures top --last 7d
```

Since v3.4.0 (2026-07-20 hardening): `boi cancel` SIGKILLs the worker's whole
process group (no more orphaned provider processes burning spend); `boi
dispatch` verifies the spec row is durable from a fresh connection before
reporting success (a durability failure is loud, not a silent loss); boot
recovery kills crashed specs' surviving worker trees (pid-reuse guarded).

Spec IDs are `S`-prefixed (e.g. `S0a3f1c2b`); task IDs are `T`-prefixed.
`dispatch` requires `boi daemon` to be running — it exits non-zero with
`DispatchError::NoDaemon` otherwise (`src/cli/dispatch.rs:103,167`).

### Dropped v1 Commands

Removed intentionally per the v1.0 parity audit. Do not invoke:
`boi workers`, `boi config`, `boi telemetry` (→ `boi traces` / `boi failures`),
`boi stop`, `boi outputs`, `boi phases`, `boi providers`, `boi bench`,
`boi research`, `boi prune-orphans` (→ `boi clean`).

## v2 Phase Set (standard pipeline)

Per design doc §6 / `pipelines/standard.toml`:

| Phase | Level | Kind | Notes |
|---|---|---|---|
| `workspace_prepare` | spec | deterministic | creates integration worktree |
| `plan` | spec | LLM | produces task breakdown |
| `critique_plan` | spec | LLM | cross-model critique |
| `workspace_verify_in` | task | deterministic | precondition gate |
| `write_red_tests` | task | LLM | TDD red-test authoring |
| `execute` | task | LLM | the work |
| `validate` | task | deterministic | runs `verifications` |
| `review` | task | LLM | code-review pass |
| `commit` | task | deterministic | atomic commit to task branch |
| `workspace_verify_out` | task | deterministic | postcondition gate |
| `merge_to_integration` | spec | deterministic | fast-forward to integration |
| `teardown` | spec | deterministic | tear down worktrees |

If `validate` fails, the side-chain `propose_adjustment` → `review_adjustment`
(both LLM) auto-inserts before re-running `execute`.

## Verify Gate Rules (footguns — carried verbatim from v1)

Verify-gate phases (the deterministic `validate` and `workspace_verify_*`
phases) run `verifications.command` in a **minimal subshell with stripped
PATH**. Workers do real work fine; verify gates fail silently because the
gate's environment isn't what you'd assume. Specific footguns:

| Mistake | Fix |
|---|---|
| `cargo build && ...` in verify | Prepend `export PATH="/opt/homebrew/bin:$PATH" &&` before any non-coreutils binary (cargo, node, etc.). Exit 127 = command not found. |
| `cmd 2>&1 \| tail -5 \| grep "..."` | Never pipe through `tail`/`head` before checking exit status — the tail's exit code wins. Use `cmd > /tmp/log && grep "..." /tmp/log` instead. |
| Checking `.hex/bin/hex` after a build | That's the *deployed* binary (yesterday's). The engine injects a shared `CARGO_TARGET_DIR` (`~/.boi/v2/cargo-target`) into every verification command, so cargo artifacts do NOT land in the worktree's `target/`. Use `${CARGO_TARGET_DIR:-target}/release/hex` — the freshly built one, wherever cargo actually put it. Never symlink the worktree `target/` to the shared dir — it holds other specs' binaries, so `test -x` would false-pass. |
| Querying main-workspace state (e.g. `$HEX_DIR`-rooted commands) from worker | Worker is in a task worktree; commands that read `$HEX_DIR` (main workspace) ignore worktree edits. Check **artifacts the worker created**, not derived state. |
| `grep -q -v "ERROR"` | Inverted flag combo. Use `! grep -q "ERROR"` instead. |
| `... \| wc -l \| grep -q "^14$"` | macOS `wc -l` pads with whitespace (`      14`). Use `count=$(... \| wc -l \| tr -d ' '); test "$count" = "14"` instead. |
| Multi-line `python3 -c "..."` with indented lines | `IndentationError: unexpected indent`. Python `-c` doesn't strip indentation. Use `python3 - <<'PYEOF' ... PYEOF` heredoc instead; or single-line semicolon-separated. |
| Verify checks a path that historical refs may keep | Verify the deliverable EXISTS (test -f), not that grep returns 0 against the whole workspace. |

**Mandatory pre-dispatch step:** Before dispatching any TOML spec, run every
`verifications.command` in a real subshell against either the current workspace
or a representative state — with `export CARGO_TARGET_DIR="$HOME/.boi/v2/cargo-target"`
set first if the gate touches cargo, so the local run matches the engine's
injected environment (a bare local subshell passes `target/...` checks that the
engine then fails). If your verify doesn't pass against known-good state, the
worker will fail forever. Cost of running verify locally first: ~30s. Cost of a
verify cycle through BOI: 5–15min plus restart overhead.

**Verify-gate doctrine:** Test artifacts the worker produces (files, build
outputs in the task worktree). Don't test system state the worker can't see
(deployed binaries, fleet count from main `$HEX_DIR`). When invoking any
binary, prepend the PATH for that binary. When chaining, use `&&` — never lose
exit codes through pipes.

## Task Sizing Rule

One task = one deliverable completable in <15 min of agent time. Workers get
killed at iteration boundaries and ALL in-progress work is lost. Break large
work into many small tasks. A spec with 15-20 focused tasks beats one with 6
monoliths. Iteration caps are hard-coded in v1.0; per-spec tuning is deferred.

Signs a task is too large:
- More than 3 distinct deliverables in one `behavior` string
- Requires reading/processing more than ~50 files
- `behavior` line longer than ~200 chars
- Combines design + implementation + benchmarking

## Debugging BOI Failures

When a spec fails, diagnose systematically:

1. **TUI:** `~/.boi/bin/boi dashboard <spec-id>` — live phase status, the
   verdict for each phase run, and the failure typed-reason.
2. **History:** `~/.boi/bin/boi log <spec-id>` — phase-run history in order.
3. **Spec snapshot:** `~/.boi/bin/boi spec show <spec-id>` — the immutable
   `spec_versions` row that was dispatched.
4. **OTel traces:** `~/.boi/bin/boi traces query "<sql>"` — full structured
   trace store (DuckDB).
5. **Recurring failures:** `~/.boi/bin/boi failures top --last 7d` — fingerprint
   aggregation across specs.

### Common Failure Patterns

- **`DispatchError::NoDaemon`:** `boi daemon` not running. Start it first.
- **`ConfigError::ModesRemoved` / `InitiativeRemoved`:** spec carries a v1 field.
  Remove `mode = ...` / `initiative = ...` from the TOML.
- **`ConfigError::WorkspaceXor`:** `[contract]` set both `workspace` and
  `workspace_rationale`, or neither. Pick exactly one.
- **`ConfigError::DependencyCycle` / `DanglingDep` / `DuplicateRef`:** the
  `blocked_by` graph is malformed. Validate refs against the task list.
- **`ConfigError::VerificationMutex`:** a verification set both `intent` and
  `command`. Pick one.
- **Validate phase fails repeatedly:** the task's `verifications.command`
  doesn't pass even in known-good state. Run it locally first (see verify-gate
  doctrine above).

## Recovery

Every recovery command requires a typed reason. There is no "quiet cancel."

```bash
~/.boi/bin/boi cancel <spec-or-task-id> --reason "<typed reason>"
~/.boi/bin/boi fail <spec-id> --reason "<typed reason>"
~/.boi/bin/boi unblock <task-id>                  # forces blocked → active
~/.boi/bin/boi unblock <task-id> --reset-counter  # also zeros iteration count
~/.boi/bin/boi resolve-conflict <task-id>         # interactive shell
```

There is intentionally no `--ai` flag on `resolve-conflict` — LLM-driven
conflict resolution is deferred to v1.x
(`src/cli/mod.rs:165-172`).

## Pitfalls

1. **BOI source repo is read-only from hex.** Modifications go through normal
   PR review against `~/github.com/mrap/boi/`.
2. **`boi daemon` MUST be running** before any write-side command. Read-only
   commands (`log`, `dashboard`, `spec show`, `traces`, `failures`) work
   regardless — they hit SQLite/DuckDB directly.
3. **TOML only.** Any `.yaml` / `.spec.md` spec is rejected. The grammar lives
   at `~/github.com/mrap/boi/src/config/spec.rs`; defer to it when in doubt.
4. **`deny_unknown_fields` is strict.** Typos in field names fail at parse
   time with line/column. Don't paste v1 specs and hope they work.
5. **VERBAL MONITORING = BUG.** When you leave and a spec is running, NEVER say
   "I'll keep an eye on it." Use `boi dashboard` or `boi log <spec-id>` to
   check status; wire a hex harness worker (`.on_cron(...)`) or a Claude Code
   hook to notify on completion or failure — never a new launchd job
   (foundation Automation policy: sanctioned launchd surface in
   `docs/hex-ops.md`). Mechanical action, not verbal (SO #10).
6. **Daemon deploys/restarts must carry the phase budget env.** `boi daemon
   start` bakes `BOI_PHASE_WALL_CLOCK_BUDGET_SECS` into the launchd plist FROM
   THE INVOKING SHELL (default 1200s). A redeploy from a clean shell silently
   reverts a longer budget — this wedged two specs for 4 days (2026-07-16/20).
   Until the value moves to a config file: always
   `BOI_PHASE_WALL_CLOCK_BUDGET_SECS=3600 boi daemon start`.
7. **One pipeline at v1.0.** `pipeline` accepts only the literal string
   `"standard"`; any other value — including a path like `./my.toml` — is
   rejected at parse time with `unknown pipeline` (`src/config/spec.rs`). Omit
   the field or set it to `"standard"`.

## Deeper Reference

- Canonical design: `~/github.com/mrap/boi/docs/design/2026-05-16-design.md`
- Goose-as-adapter amendment: `~/github.com/mrap/boi/docs/design/2026-05-17-goose-as-runtime-adapter.md`
- Spec grammar: `~/github.com/mrap/boi/src/config/spec.rs`
- CLI surface: `~/github.com/mrap/boi/src/cli/mod.rs`
- §13 fixtures: `~/github.com/mrap/boi/tests/fixtures/specs/0[1-5]-*.toml`
