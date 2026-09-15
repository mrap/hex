<!-- # sync-safe -->
# hex-foundation

A minimal, installable template for the hex system — a persistent AI workspace for Claude Code that accumulates context, learns your patterns, and improves itself over time.

**For:** engineers on Claude Code who are tired of their agent starting from zero every session.

---

## Quick start

```bash
git clone https://github.com/mrap/hex-foundation /tmp/hex-setup && bash /tmp/hex-setup/install.sh && cd ~/hex && claude
```

Your agent walks you through setup on first run. Three questions, then you're working.

### Prerequisites

- Python 3.9+
- git
- [Claude Code CLI](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code) (`claude`) — warning-only; install separately
- Rust / cargo ([rustup.rs](https://rustup.rs)) — required to build the `hex` binary from source; without it, install attempts a pre-built binary download

The installer sets up `~/.boi` (BOI worker fleet). BOI version pinned in [`VERSIONS`](./VERSIONS).

### Install options

```bash
bash install.sh              # installs to ~/hex
bash install.sh ~/my-hex     # custom location
```

To use a fork of BOI, set `HEX_BOI_REPO` before running install.

---

## What you get

After install, `~/hex/` contains:

```
~/hex/
├── CLAUDE.md         Operating model for Claude Code (system zone + your zone)
├── AGENTS.md         Operating model for other agents (Codex, Cursor, etc.)
├── todo.md           Your priorities and action items
├── me/               About you — me.md (stable), learnings.md (observed patterns)
├── projects/         Per-project context, decisions, meetings, drafts
├── people/           Profiles and relationship notes
├── evolution/        Self-improvement engine — observations, suggestions, changelog
├── landings/         Daily outcome targets
├── raw/              Transcripts, handoffs, unprocessed input
├── specs/            BOI spec drafts
├── .hex/             System files (scripts, skills, memory.db) — managed
└── .claude/commands/ Claude Code slash commands — managed
```

Companion systems installed alongside:

- **[`~/.boi`](https://github.com/mrap/boi)** — parallel Claude Code worker dispatch

### Auto-configured by install.sh

- **Claude Code hooks** — `install.sh` writes `PreToolUse`, `PostToolUse`, `Stop`, and `SessionStart` hooks into `.claude/settings.json` automatically. No manual hook setup required.
- **Shell completions** — `install.sh` (and `hex upgrade` on re-run) automatically install shell completions for your current shell (zsh, bash, or fish) after the binary is in place. Idempotent: re-running produces no diff. See [Shell completions](#shell-completions) for manual setup or bespoke paths.
- **C3 observability scripts** — baseline instrumentation scripts (`c3-mirror-sink`, `c3-audit-completeness`, `c3-ttd-tracker`, `c3-orphan-scan`, `c3-quiet-failure-snapshot`) that track wake completion ratios, time-to-detect for quiet failures, orphaned audit records, and drain `action_log` to a queryable JSONL mirror. SQL migrations at `system/telemetry/migrations/` define the C3 VIEWs.

---

## Core ideas

**Persistent memory.** Every observation, decision, and learning gets written to a file — not summarized into a chat bubble that disappears. A SQLite FTS5 index at `.hex/memory.db` makes all of it searchable. With `fastembed` + `sqlite-vec` installed, the indexer upgrades to hybrid semantic + keyword search automatically; FTS5-only is the default when those libraries aren't present. A distill pipeline (`hex memory distill`) extracts structured facts from session transcripts using a two-phase LLM process (extract → judge), and a nightly consolidate pass (`hex memory consolidate quick`) prunes contradictions, staleness, and orphaned references automatically (use `hex memory consolidate full` for the LLM-assisted operating-model audit).

**Operating model.** `CLAUDE.md` ships with 20 core standing orders, a learning engine that records observations to `me/learnings.md` with evidence and dates, and an improvement engine that detects friction, proposes fixes after 3+ occurrences, and tracks what ships.

**Two-zone CLAUDE.md.** The system zone is managed by upgrades; your zone is preserved byte-for-byte. Add your own rules without losing them on every update.

```markdown
<!-- hex:system-start — DO NOT EDIT BELOW THIS LINE -->
... managed by hex
<!-- hex:system-end -->

<!-- hex:user-start — YOUR CUSTOMIZATIONS GO HERE -->
- Always check Jira before starting feature work
- Prefer rebase over merge
<!-- hex:user-end -->
```

**Decision records.** Every decision gets logged to `me/decisions/` (or `projects/{project}/decisions/`) with date, context, reasoning, and impact. A template ships at `.hex/templates/decision-template.md`. The `/hex-decide` command walks through the full framework.

---

## Slash commands (inside a Claude Code session)

These are Claude Code slash commands, not shell CLIs. Use them inside a `claude` session running in your hex directory.

| Command | What it does |
|---------|--------------|
| `/hex-decide` | Structured decision framework — context, options, reasoning, impact. |
| `/hex-doctor` | Health check. 20-point validation across env, memory, structure, config, and companions. Use `--fix` to repair auto-fixable issues, `--json` for machine-readable output. For the unified consolidate pass (structural + memory DB + optional LLM operating-model audit), use `hex memory consolidate quick` or `hex memory consolidate full`. |
| `/hex-upgrade` | Pull latest system files from hex-foundation. Runs doctor after. |

The commands that ship live in `system/commands/` (currently `hex-decide`, `hex-doctor`, `hex-upgrade`). Session-lifecycle commands (`hex-debrief`, `hex-triage`, startup, checkpoint, shutdown, reflect, save) are not shipped — hex is sessionless and event-driven, and the pre-demolition agent-fleet skills no longer ship.

---

## Upgrading

Inside your hex instance directory:

```bash
hex upgrade         # in-place upgrade
hex doctor          # post-upgrade health check
```

Common `hex upgrade` flags:

- `--dry-run` — show what would change without writing
- `--local PATH` — use a local hex-foundation checkout instead of fetching

What it does:

1. Backs up `.hex/` to `.hex-upgrade-backup-YYYYMMDD/`
2. Detects source layout (`system/` + `templates/`) and maps paths accordingly
3. Replaces `.hex/` (preserving `memory.db`)
4. Deletion pass: removes files no longer present in foundation (backed up before deletion)
5. Rebuilds the `hex` binary if the Cargo.toml version changed; verifies the installed binary matches
6. Merges `CLAUDE.md`: system zone replaced, user zone preserved

Your data (`me/`, `projects/`, `people/`, `evolution/`, `landings/`, `raw/`, `todo.md`) is never touched. Run `hex doctor` afterward for the post-upgrade health check.

You can also run the upgrade from inside Claude Code via `/hex-upgrade`.

---

## Multi-agent support

`AGENTS.md` ships for Codex, Cursor, Gemini CLI, Aider, or any agent that reads a markdown operating-model file. Slash commands are Claude Code-specific.

---

## Supported Runtimes

| Runtime | Status | Notes |
|---------|--------|-------|
| Claude Code | Full support | Primary development runtime |
| Codex (OpenAI) | Partial | Core scripting and agent wakes are broken; see below |

### Codex Limitations

**Broken (will not work without code changes):**
- **Agent wakes / headless invocation**: the harness (`harness/src/claude.rs`) resolves the `claude` binary via `$CLAUDE_BIN` env var, then `$HOME/.local/bin/claude`, then PATH. Set `CLAUDE_BIN=/path/to/claude` to override (useful for headless/daemon contexts). Codex uses `codex exec --json`. No runtime abstraction for the `--output-format json` flag exists yet.
- **Hook installation**: `doctor.sh` only writes hooks to `.claude/settings.json`. Codex reads `~/.codex/config.toml` (TOML format). Hooks are silently uninstalled for Codex users.
- **Wake scripts**: launchd/cron entries call `claude` directly — they will fail on a system where only `codex` is installed.

**Partial (works differently):**
- **Hooks**: event names (`PreToolUse`, `PostToolUse`, `Stop`, `SessionStart`) match, but the config file is `~/.codex/config.toml` (TOML), not `.claude/settings.json` (JSON). Hook contents transfer; the installer doesn't write them for Codex.
- **Skills**: skill format (SKILL.md) is compatible. Discovery path differs: Codex looks in `.codex/skills/`, hex installs to `.hex/skills/`.
- **Slash commands**: Codex supports custom commands from `.claude/commands/*.md` but does not auto-invoke them — users must type `/commandname` manually. Claude Code auto-invocation does not apply.
- **Memory**: Codex has a memory feature (`memories = true` in `config.toml`) stored at `~/.codex/memory/` globally. (Claude Code's `~/.claude/projects/<dir>/memory` path is deprecated for hex — hex uses its own SQLite store at `.hex/memory.db`.)
- **MCP servers**: both runtimes support MCP. Config format differs: hex uses `.mcp.json` (JSON), Codex uses `[mcp_servers]` in `config.toml` (TOML).
- **Session resume**: supported on both; Codex uses `codex resume <SESSION_ID>` or `--last`.
- **`CLAUDE.md`**: Codex reads it as a fallback when no `AGENTS.md` is present. `AGENTS.md` (included in this repo) is the preferred file for Codex.

**Works without changes:**
- `CLAUDE.md` / `AGENTS.md` operating model
- `doctor.sh` LLM preference detection and `.codex/config.toml` creation
- Hook event names are identical across both runtimes
- All file-based memory and project context

---

## Architecture

`hex` is a unified Rust binary — **core infrastructure**, not optional. One binary handles everything: persistent memory, session lifecycle, system health, telemetry rotation, integration bundles, hooks, the harness loop, triggers, in-place upgrades, the worker runtime, and release tooling.

- **Memory** (`hex memory`) — search/index/recall/distill/consolidate over the SQLite FTS5 + vec0 store
- **Doctor** (`hex doctor`) — system health checks across env, memory, structure, config, companions
- **Telemetry** (`hex telemetry`) — telemetry file rotation and management
- **HITL** (`hex hitl`) — pending-human-action queue (file/list/close items, iMessage pings + daily digest)
- **Integration** (`hex integration`) — integration bundle lifecycle
- **Hook** (`hex hook`) — Claude Code hook runners (session-start, post-tool-use, backup-session)
- **Harness** (`hex harness`) — the wake/loop runtime
- **Triggers** (`hex triggers`) — trigger evaluation
- **Upgrade** (`hex upgrade`) — in-place upgrade pipeline
- **Worker** (`hex worker`) — worker process entrypoint
- **Env** (`hex env`) — environment setup utilities

13 subcommands:

```
hex memory       — search, index, recall, distill, consolidate
hex doctor       — system health checks (--fix, --json)
hex telemetry    — telemetry file rotation and management
hex hitl         — pending-human-action queue (add, list, show, done, skip, snooze, nudge, digest)
hex integration  — integration bundle lifecycle
hex completions  — shell completions (zsh, bash, fish)
hex hook         — Claude Code hook runners (session-start, post-tool-use, backup-session)
hex harness      — wake/loop runtime
hex triggers     — trigger evaluation
hex upgrade      — upgrade hex installation
hex worker       — worker process entrypoint
hex version      — print version
hex env          — environment setup utilities
```

Requirements: Rust toolchain (for building from source) or a supported platform for pre-built binaries (macOS arm64/x86_64, Linux x86_64).

The binary is built or downloaded automatically by `install.sh`. If it cannot be built or downloaded, install warns and continues — core scripting still works, but agent wakes, the server, and fleet management require the binary. Run `hex-doctor` to verify status after install.

### Version

`system/harness/Cargo.toml` is the single source of truth. `env!("CARGO_PKG_VERSION")` embeds the version at compile time. Git tags must match — enforced by `hex release cut`, the GitFlow release ceremony. See [docs/versioning.md](./docs/versioning.md).

### Module authoring

Harness behavior is built from **modules** — typed Rust workers auto-discovered by a `*.worker.rs` file convention. A module is one file exposing `pub fn worker() -> Worker`, built with the `hex::worker` API (`Worker::new(name).on_cron(...)` / `.on_event(...)` / `.on_state(...)` / `.on_queue(...)`). The build script recursively globs `*.worker.rs` from `system/harness/src/modules/` (core, shipped) and `$HEX_DIR/.hex/modules/` (personal overlay, under `--features personal`), derives a module ident from each filename, and generates the registry — no central file to edit.

Installing a module is a rebuild: drop the file in a root, rebuild the harness, then `hex module list` / `hex module status <name>` to confirm it registered. See [docs/module-authoring.md](./docs/module-authoring.md) for the full guide.

## Quality assurance — gaming detection

hex ships a Quality Antagonist: an adversarial checker that validates completed work is real, not gamed.

**What it detects:**
- Metric commands that are trivially rewritten (`echo 0` → `echo 1`) rather than measuring real behavior
- KRs marked "met" where the independent measurement disagrees with the claimed value
- Math errors (`lower_is_better` KRs where `current > target` yet `status = met`)
- Specs that complete suspiciously fast relative to their described scope
- File-existence proxies (script exists ≠ script runs)

**How it works:**

```
boi.spec.completed → quality-spec-audit policy → hex doctor quality-check --spec <id>
initiative.kr.met  → quality-kr-check policy   → hex doctor quality-check --kr <init>/<kr>
timer.tick.6h      → quality-sweep policy       → hex doctor quality-check --sweep
                                                     ↓
                                          hex.quality.gaming.detected
                                          hex.quality.kr.reverted
                                          hex.quality.suspect
```

The antagonist runs independently — it does not trust the metric command the worker used. It re-runs the metric, checks the math, and reverts KR status if fraud is confirmed. The charter lives at `system/reference/core-agents/quality-antagonist.yaml`.

**CLI:**

```bash
hex doctor quality-check --spec Sxxxxxxxx  # audit one spec (Crockford base32)
hex doctor quality-check --kr init-foo/kr-1 # reality-check a KR
hex doctor quality-check --sweep           # scan last 24h
```

---

## Project layout (this repo)

```
hex-foundation/
├── install.sh           Single install entrypoint
├── VERSIONS             Pinned BOI release
├── system/              → becomes ~/hex/.hex/ on install
│   ├── harness/         ← hex binary Rust source (14 subcommands)
│   │   ├── src/main.rs     unified CLI
│   │   ├── src/hook.rs     Claude Code hook runners (session-start, post-tool-use, backup-session)
│   │   ├── src/doctor.rs   DoctorCheck trait + checks
│   │   ├── src/upgrade.rs  upgrade pipeline
│   │   └── build.rs        injects git SHA; Cargo.toml is version source
│   ├── scripts/         env.sh, hex-integration, hex-integration-check.sh
│   ├── commands/        → copied to ~/hex/.claude/commands/ (Claude Code) and ~/hex/.hex/commands/
│   ├── skills/          memory/, landings, hex-decide, hex-doctor, hex-upgrade,
│   │                    boi-delegation, conjecture-criticism, repo-audit,
│   │                    repo-docs, vibe-to-prod
│   └── reference/       core-agents/ — agent charters
├── templates/           Seeds for CLAUDE.md, AGENTS.md, me.md, todo.md, decision-template.md
├── docs/architecture.md System overview
└── tests/               E2E, layout, and memory tests
```

---

## Testing

The test suite verifies installation, migration, skill discovery, and Codex parity. See [`docs/testing.md`](./docs/testing.md) for the full matrix and how to run locally. The rules for writing and reviewing tests are in [`docs/testing-standard.md`](./docs/testing-standard.md).

Key test files:

| Test | What it verifies |
|------|-----------------|
| `tests/agent-harness/Dockerfile` | Agent harness E2E — charter discovery, wake, fleet, core drift, messages (43 tests) |
| `tests/agent-harness/Dockerfile.initiative` | Initiative E2E — auto-seeding, watchdog, scheduled promotion (10 tests) |
| `tests/agent-harness/Dockerfile.migration` | v0.8.0 migration — binary rename, symlink, backward compat, version (17 tests) |
| `tests/contract-verification/Dockerfile` | Schema contracts across hex components (22 tests) |
| `tests/feedback-loops/Dockerfile` | All 4 feedback loops — pivots, escalation, redesign (30 tests) |
| `tests/codex-parity/Dockerfile` | Codex runtime parity — hooks, skills, memory, agent wake |
| `tests/test_skill_frontmatter.sh` | Every SKILL.md has valid YAML frontmatter |
| `tests/test_skill_refs.sh` | All paths referenced inside SKILL.md resolve |
| `tests/test_e2e.sh` | Full install + doctor + upgrade lifecycle |
| `tests/core-e2e/run-all.sh` | Hex primitives + BOI integration (containerized; CI-gated) |

To run the full suite locally:

```bash
# Static tests (no API key needed)
bash tests/test_skill_frontmatter.sh
bash tests/test_skill_refs.sh

# Core E2E (requires Docker; ANTHROPIC_API_KEY for BOI suites)
bash tests/core-e2e/run-all.sh                    # all suites
bash tests/core-e2e/run-all.sh --exclude boi       # skip BOI (no Docker-in-Docker)
bash tests/core-e2e/run-all.sh --include boi       # BOI suites only (host runner)

# Live eval tests (requires ~/.hex-test.env with ANTHROPIC_API_KEY)
bash tests/eval/run_eval_docker.sh --live    # Linux Docker
bash tests/eval/run_eval_macos.sh            # macOS Tart
```

---

## Roadmap

v0.19.1: **V2 Memory Plan 2 — facts layer, distill pipeline, nightly consolidate.**
- **Facts layer**: six new tables (`facts`, `fact_history`, `sessions`, `topics`, `transcript_files`, `vec0`, FTS5). Structured facts with full history, typed by 24-predicate vocabulary.
- **Distill pipeline** (`hex memory distill`): two-phase LLM process — Phase 1 extracts candidate facts from transcripts; Phase 2 judges each as ADD/UPDATE/NOOP/FLAG with embedding dedup and per-file watermarking.
- **Nightly consolidate** (`hex memory consolidate`): 6-op consolidation pass — contradiction detection, staleness pruning, orphan cleanup, dedup, topic reorg, summary refresh. Runs nightly.
- **Recall extended**: `hex memory recall` now searches the facts table alongside indexed memory entries.

v0.18.0: **Capability system — agent-registered functions with security guard.**
- **`capability_add` / `capability_call` trail entry types**: agents can register executable functions and invoke them from trail entries. Gate schema enforces required fields.
- **Security guard**: static body-scan hard-denies network egress (`curl`, `wget`, `nc`), secrets access, `rm -rf`, and pipe-to-shell before any script is persisted. Capabilities are write-once after registration.
- **Sandboxed execution**: per-wake call caps, wall-clock timeouts, and output byte limits enforced by `capability_exec.rs`.
- **Registry**: `FunctionCapability` and `TriggerCapability` structs persisted to `.hex/registry/`. Pilot-agent allowlist controls who can register.
- **1466 lines of new tests**: `capability_exec_test.rs`, `capability_guard_test.rs`, `capability_lifecycle_test.rs`, `gate_test.rs`, `registry_test.rs`.

v0.17.3: **Act evidence verification.**
- **Mechanical act evidence gate**: harness now verifies `detail.evidence` on every `act` trail entry that claims a mechanical operation (git push, BOI dispatch, file write, etc.). Unverifiable claims are recorded as `UNVERIFIED` — they do not count as done.
- **Agent prompt hardened**: agents are now explicitly instructed that mechanical acts without verifiable evidence are not accepted. Prevents claim-without-action loops.

v0.17.2: **Doctor ports + release.rs native module.**
- **4 doctor commands native**: `hex doctor introspect`, `goal-alignment`, `cleanup-projects`, `tech-scout` ported to Rust. 4 `.legacy.sh` stubs removed.
- **`release.rs` native module**: deterministic LLM-free release command replaces the manual legacy script steps. The tool that ships hex is now Rust.
- **Quality policies migrated**: `system/policies/` → `system/events/policies/`. Commands repointed to `hex doctor quality-check`.

v0.17.1: **Doctor consolidate + startup cleanup.**
- **`hex doctor consolidate` native**: consolidation subcommand ported to Rust. `/hex-consolidate` command deleted.
- **Startup shell-out removal**: startup sequence no longer shells out to Python scripts. Regression test added.

v0.17.0: **TriggerSpec unification + truncation recovery.**
- **Bare-string trigger syntax**: charter YAML now accepts `"timer.tick.6h"` directly — no struct wrapper required. Both forms valid. Same for `BlockedItem`. Every existing policy continues to work.
- **Truncated response recovery**: harness salvages complete leading elements from truncated JSON responses. Every truncation emits a loud audit entry (S6 compliance — no more silent failures).

v0.16.0: **Full rustification + S1 skills sync.**
- **Zero Python required**: `hex hook`, `hex doctor`, `hex upgrade`, and more ported from Python/shell. 130 Python files reduced to ~22.
- **S1 skills sync**: 9 new skills + `bet-status` command promoted from personal instance to foundation.
- See [CHANGELOG.md](./CHANGELOG.md) for full details.

v0.15.0: **Harness messaging + binary resolution fix.**
- **Agent messaging deadlock fixed**: `cli_send()` now writes to per-agent JSONL inbox (`.hex/messages/{agent}.jsonl`). Prior behavior wrote only to `messages.json`, which the harness wake cycle doesn't read — messages were silently dropped.
- **Daemon binary resolution**: `claude::invoke()` resolves the binary via `$CLAUDE_BIN` env var, then `$HOME/.local/bin/claude`, then PATH. Fixes "No such file or directory" on LaunchAgent/daemon wakes where `~/.local/bin` is not in PATH.

v0.14.0: **Slack/cc-connect deprecation + upgrade pipeline cleanup.**
- **Slack/cc-connect fully removed**: hex no longer requires or references Slack. Removed `--slack` flag from `hex-vitals.py`, Slack posting code (~155 LOC), dead `check-slack-alert-roundtrip` module from doctor, `/api/messages` Slack-fetch endpoint from pulse dashboard, and `slack` surface from `telemetry-ratio.py`. `slack-bot.sh` and `secrets-pipeline.sh` archived.
- **Upgrade pipeline simplified**: the legacy standalone shell upgrader was retired in favor of the Rust `hex upgrade` subcommand; legacy standalone install paths dropped.

v0.13.3 fixes: **Health scripts + messaging.receive + wake crash-recovery.**
- **New health scripts**: `check-vector-search.sh` verifies sqlite-vec is loadable and memory.db has vectors.
- **wake.rs crash recovery**: `messaging.receive()` transitions inbox messages to `in_progress` atomically — messages survive `claude::invoke` errors and are re-delivered on the next wake. Legacy JSONL inbox drained in parallel.

v0.13.2 fixes: **Session-start checkpoint resume, integration-check emit fix, and memory leak fix.**
- **Channel checkpoint resume**: `session-start.sh` now surfaces `projects/<topic>/checkpoint.md` for `hex-<topic>` channels. Sessions pick up where they left off automatically. Generalized blocker-flag scan (`.hex/state/blockers/*.flag`) and topic-regex sanitization included.
- **Integration-check emit fix**: `export _error_raw` bug — 11,948+ events/day were emitted with `error: null` because the env var wasn't propagating into the command-substitution subshell. Fixed. Emit-throttle added for persistent fail streaks (heartbeat every 60 checks; no more event spam for a single dead probe).
- **Memory vector leak fixed**: `memory_index.py` now cascade-deletes `vec_chunks` orphans on re-index. 82,377 orphan rows (58% of the vec table) had accumulated because FTS5 chunk deletion didn't cascade to the `vec0` virtual table. Fixed.
- **Honest hybrid search**: `memory_search.py` documents `_rrf_merge` as FTS-only (KNOWN GAP) — `--hybrid` was paying embedding+vec-query cost without fusing vec results into the score.
- **Doctor: new health module**: Memory Vector Search (surfaces sqlite-vec drift).

v0.13.1 fixes: **Doctor reliability and skip_llm WakeConfig.**
- **Doctor streaming**: Doctor command streams output live via `Stdio::inherit` (was buffered — appeared to hang on slow modules). `hex-doctor` bash script also switched to `tee | tail -n +5` streaming with full PIPESTATUS capture.
- **Path bug fix**: 3 orchestration scripts had stale `CLAUDE_DIR=$HEX_DIR/.claude` (should be `.hex`). Caused 5 spurious doctor ERRORs on standard installs. Fixed.
- **BOI daemon detection**: LaunchAgent-aware detection replaces `pgrep`-based check.
- **`skip_llm` WakeConfig**: Agents that exercise wake plumbing without needing LLM reasoning (e.g. health probes) can set `wake.skip_llm: true`. Harness bypasses shift loop and self-assessment; inbox still loads and `mark_delivered` fires. `state.json` inbox drain prevents unbounded growth on high-frequency skip_llm wakes.

v0.13.0: _Fleet self-driving mechanisms (fleet-pulse, stalled-initiative-monitor, mike-pending-escalator, fleet-scorecard, agent-performance-review) shipped here, then removed — hex is sessionless/event-driven and the agent fleet was demolished. Entries dropped._

v0.12.0 adds: **Upgrade reliability, shell completions, failure-revive protocol, and doctor improvements.**
- **Shell completions**: `hex completions bash|zsh|fish` generates completion scripts for all subcommands. Install snippets in README.
- **Upgrade reliability**: 5-bug patch to `/hex-upgrade` — stale-symlink cleanup, RC file detection, hex-binary version sync via Cargo.toml. Docker E2E 101/101.
- **Doctor enhanced**: hex-binary version-sync check added (catches binary/Cargo.toml divergence).
- **Failure-revive protocol**: three-strike detection + spec-owner-resolver + build-failure-brief for automated BOI failure analysis. _(Removed — it routed failures to the now-demolished agent fleet; rebuild planned.)_
- **Policy validator**: `dagu` added to VALID_ACTION_TYPES, fixing false-positive validation errors.
- **AGENTS.md**: Quick Start, Gotchas, and How to Modify sections; Layer 2 Mechanisms condensed to compact table. Cross-repo navigation links added.
- **Cleanup**: cc-connect/slack-bot scripts removed (7 files). Attack surface reduced.

v0.10.0 adds: **BOI v1.1.0 integration + containerized BOI E2E.**
- **BOI v1.1.0**: pipeline-v2 phases (clean spec-pre / task / spec-post separation), interactive `boi dashboard` TUI, spec-critique↔spec-improve quality loop, deterministic phases (commit/merge/cleanup) that skip Claude. Upgrade: run `install.sh` again.
- **Containerized BOI E2E**: `tests/core-e2e/` suites cover fresh install, upgrade (catches stale-symlink bugs), and doctor runtime checks. CI-gated via GitHub Actions core-e2e workflow.
- **Doctor expanded**: `check_17` now runs `boi --help`, `boi --version`, and `boi dashboard` instead of file-existence checks. Each failure includes a repair hint.

v0.11.0 adds: **Full hex sync sweep — 93 atomic units.**
- **New subsystems**: spec-tool (spec browsing + critic-loop UI), vibe-to-prod skill, conjecture-criticism skill, pulse dashboard with E2E test harness. _(comments-service, sse-bus, hex-fleet, boi-pm, hex-overseer shipped here, since removed.)_
- **Improvements**: shared `hex_utils.py` library; 7 metrics scripts (continuity, done-claim, frustration, loop-waste, etc.); 6 doctor-checks; 16 health-checks (agent memory, BOI dispatch, MCP servers, etc.); skills: memory, hex-event, hex-switch, x-twitter, hex-ideate, hex-triage, hex-upgrade, hex-sync-base, secret-intake, boi-delegation; 30+ MCP integration health-check wrappers.

v0.10.1: _Releaser auto-unblock regression — releaser agent has since been removed with the agent fleet. Entry dropped._

v0.9.0 adds: **BOI v1.0.0 Rust binary + doctor runtime checks.**
- **BOI rewrite**: BOI is now a compiled Rust binary at `~/.boi/bin/boi`. Install clones and builds from source; `VERSIONS` pins `BOI_VERSION`.
- **Doctor runtime checks**: `check_17` now validates `boi --help`, `boi --version` (against `VERSIONS`), `boi dashboard` (DB queryable), dangling-symlink detection, and the full wrapper chain (`~/.boi/boi --help`). Each failure includes a repair hint.
- **Doctor unit tests**: `tests/test_doctor.bats` covers all new BOI checks (missing binary, dangling symlink, broken wrapper, version mismatch, status failure).

v0.8.0 adds: **Unified `hex` binary + 3 new primitives.**
- **Unified binary**: single Rust binary with subcommands for HTTP/SSE server, asset registry, and comment system.
- **Asset registry**: unified `{type}:{id}` namespace for all hex artifacts (posts, proposals, specs, decisions, projects). Auto-discovery, periodic re-scan, CLI + HTTP API.
- **Unified comments**: single comment store, embeddable widget, LLM-classified routing, action log with related assets.
- **Telemetry**: append-only JSONL for all server requests and events. _(SSE bus shipped here, since removed.)_
- **Version system**: `Cargo.toml` is single source of truth, `env!("CARGO_PKG_VERSION")` embeds version at compile time. (v0.11.3 removed the `version.txt` sidecar.)

v0.3.0 adds: **Modular integration bundles + `hex integration` CLI.** Every external surface (API, MCP, system service, refresh flow) lives in one directory under `integrations/<name>/` — manifest, probe, runbook, secrets schema, maintenance scripts, tests. `hex integration install/uninstall/update/list/validate/status/probe/rotate` manages the lifecycle. See `docs/integrations.md` and `templates/integrations/_template/`.

v0.2.4 adds: Containerized skill discovery tests — static frontmatter validation, internal reference audit, Claude Code skill discovery (all 11 skills), Codex parity test. Both Docker and macOS Tart eval harnesses wired up.

v0.2.3 adds: Codex bake in Docker image + Codex onboarding eval case.

v0.2.2 adds: `bootstrap-migrate.sh` one-liner for v1 → v2 layout migration, generic migrator with rollback + idempotency, synthetic v1 fixtures + test suite.

v0.2.1 fixed: Hindsight removal, install.sh doctor-clean on fresh install, hidden sync-safe markers.

v0.2.0 shipped: hybrid memory search, 20-check doctor, layout-aware upgrade, decision template, 11 skills.

Next up:

- Hooks pack: transcript backup, reflection dispatch
- Session lifecycle automation (warming → hot → checkpoint transitions)

Open an issue or PR — the system is meant to evolve.

---

## Shell completions

`hex` can generate shell completions for bash, zsh, fish, elvish, and PowerShell.

```bash
# bash — add to ~/.bashrc
eval "$(hex completions bash)"

# zsh — add to ~/.zshrc
eval "$(hex completions zsh)"

# fish — add to ~/.config/fish/completions/hex.fish
hex completions fish > ~/.config/fish/completions/hex.fish
```

Or source on-the-fly:

```bash
source <(hex completions bash)   # bash
source <(hex completions zsh)    # zsh
```

---

## Pre-commit hooks

Run once: `git config core.hooksPath .githooks`

---

## License

MIT. See [LICENSE](./LICENSE).
