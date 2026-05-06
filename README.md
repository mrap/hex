<!-- # sync-safe -->
# hex-foundation

A minimal, installable template for the hex agent system — a persistent AI workspace for Claude Code that accumulates context, learns your patterns, and improves itself over time.

**For:** engineers on Claude Code who are tired of their agent starting from zero every session.

---

## Quick start

```bash
git clone https://github.com/mrap/hex-foundation /tmp/hex-setup && bash /tmp/hex-setup/install.sh && cd ~/hex && claude
```

Your agent walks you through setup on first run. Three questions, then you're working.

### Already running hex v1? Migrate to v2

If your existing hex instance has `.claude/scripts/`, `.claude/skills/`, etc., you're on the v1 layout. `/hex-upgrade` won't cleanly pull foundation v0.2.1+ content until you migrate. One line from your hex dir:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/mrap/hex-foundation/v0.2.2/bootstrap-migrate.sh)
```

Full guide: [docs/migrate-v1-to-v2.md](./docs/migrate-v1-to-v2.md).

### Prerequisites

- Python 3.9+
- git
- [Claude Code CLI](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code) (`claude`) — warning-only; install separately
- Rust / cargo ([rustup.rs](https://rustup.rs)) — required to build the `hex` binary from source; without it, install attempts a pre-built binary download

The installer sets up `~/.boi` (BOI worker fleet) and deploys hex-events from `system/events/` to `~/.hex-events`. BOI version pinned in [`VERSIONS`](./VERSIONS).

### Install options

```bash
bash install.sh              # installs to ~/hex
bash install.sh ~/my-hex     # custom location
```

To use a fork of BOI, set `HEX_BOI_REPO` before running install. hex-events ships inline at `system/events/` — no external repo needed.

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
- **`~/.hex-events`** — reactive event policies (deployed from `system/events/` — no external repo)

### Auto-configured by install.sh

- **Claude Code hooks** — `install.sh` writes `PreToolUse`, `PostToolUse`, `Stop`, and `SessionStart` hooks into `.claude/settings.json` automatically. No manual hook setup required.
- **Shell completions** — `install.sh` and `upgrade.sh` automatically install shell completions for your current shell (zsh, bash, or fish) after the binary is in place. Idempotent: re-running produces no diff. See [Shell completions](#shell-completions) for manual setup or bespoke paths.
- **Default event policies** — a starter set of hex-events policies is deployed to `~/.hex-events/policies/` during install, enabling out-of-the-box reactivity (agent lifecycle, scheduler, BOI completion events).

---

## Core ideas

**Persistent memory.** Every observation, decision, and learning gets written to a file — not summarized into a chat bubble that disappears. A SQLite FTS5 index at `.hex/memory.db` makes all of it searchable. With `fastembed` + `sqlite-vec` installed, the indexer upgrades to hybrid semantic + keyword search automatically; FTS5-only is the default when those libraries aren't present.

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
| `/hex-startup` | Session init. Loads priorities, today's landings, pending reflection fixes. Triggers onboarding on first run. |
| `/hex-checkpoint` | Mid-session save. Distill pass, handoff file, landings update. |
| `/hex-shutdown` | Session close. Quick distill, deregister session. |
| `/hex-reflect` | Session reflection. Extract learnings, identify failures, propose standing order candidates. |
| `/hex-consolidate` | System hygiene. Audit operating model for contradictions, staleness, orphaned refs. |
| `/hex-debrief` | Weekly walk-through of projects, org signals, relationships, career. |
| `/hex-decide` | Structured decision framework — context, options, reasoning, impact. |
| `/hex-triage` | Route untriaged content from `raw/` to the right files. |
| `/hex-doctor` | Health check. 20-point validation across env, memory, structure, config, and companions. Use `--fix` to repair auto-fixable issues, `--json` for machine-readable output. |
| `/hex-upgrade` | Pull latest system files from hex-foundation. Handles v1→v2 layout migration. Runs doctor after. |

---

## Upgrading

Inside your hex instance directory:

```bash
bash .hex/scripts/upgrade.sh
```

Options:

- `--dry-run` — show what would change
- `--local PATH` — use a local hex-foundation checkout instead of fetching
- `--skip-boi` / `--skip-events` — skip a companion

What it does:

1. Backs up `.hex/` to `.hex-upgrade-backup-YYYYMMDD/`
2. Detects source layout (v1 `dot-claude/` or v2 `system/`) and maps paths accordingly
3. Replaces `.hex/` (preserving `memory.db`)
4. Deletion pass: removes files no longer present in foundation (backed up before deletion)
5. Rebuilds the `hex` binary if the Cargo.toml version changed; verifies the installed binary matches
6. Merges `CLAUDE.md`: system zone replaced, user zone preserved
7. Runs `doctor.sh`

Your data (`me/`, `projects/`, `people/`, `evolution/`, `landings/`, `raw/`, `todo.md`) is never touched.

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
- **Agent wakes / headless invocation**: the harness (`harness/src/claude.rs`) and `env.sh` hardcode the `claude` binary and `--output-format json` flag. Codex uses `codex exec --json`. No runtime abstraction exists yet.
- **Hook installation**: `doctor.sh` only writes hooks to `.claude/settings.json`. Codex reads `~/.codex/config.toml` (TOML format). Hooks are silently uninstalled for Codex users.
- **Wake scripts**: generated by `hex-agent-spawn.sh`, call `claude` directly — they will fail on a system where only `codex` is installed.

**Partial (works differently):**
- **Hooks**: event names (`PreToolUse`, `PostToolUse`, `Stop`, `SessionStart`) match, but the config file is `~/.codex/config.toml` (TOML), not `.claude/settings.json` (JSON). Hook contents transfer; the installer doesn't write them for Codex.
- **Skills**: skill format (SKILL.md) is compatible. Discovery path differs: Codex looks in `.codex/skills/`, hex installs to `.hex/skills/`.
- **Slash commands**: Codex supports custom commands from `.claude/commands/*.md` but does not auto-invoke them — users must type `/commandname` manually. Claude Code auto-invocation does not apply.
- **Memory**: Codex has a memory feature (`memories = true` in `config.toml`) stored at `~/.codex/memory/` globally. Claude Code auto-populates `~/.claude/projects/<dir>/` per project. Different opt-in model and path.
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

`hex` is a unified Rust binary — **core infrastructure**, not optional. One binary handles:

- **Agent harness** (`hex agent`) — fleet management, wakes, cost tracking, gate validation, initiative execution
- **HTTP/SSE server** (`hex server`) — serves all web surfaces, real-time event bus with namespaced topics
- **Asset registry** (`hex asset`) — unified `{type}:{id}` namespace for all hex artifacts
- **Comment system** (`hex comment`) — feedback on any asset, routed to agents via LLM classification

Subcommands:

```
hex agent wake/fleet/status/message/list    — agent fleet management
hex server start                            — HTTP server + SSE bus (port 8880)
hex asset resolve/list/search/types         — asset registry
hex comment respond/list                    — comment management
hex sse publish/topics                      — SSE bus operations
hex version                                 — print version
```

Backward compatibility: `hex-agent` is a symlink to `hex`. Existing scripts and policies that call `hex-agent` continue to work unchanged.

Requirements: Rust toolchain (for building from source) or a supported platform for pre-built binaries (macOS arm64/x86_64, Linux x86_64).

The binary is built or downloaded automatically by `install.sh`. If it cannot be built or downloaded, install warns and continues — core scripting still works, but agent wakes, the server, and fleet management require the binary. Run `hex-doctor` to verify status after install.

### Version

`system/harness/Cargo.toml` is the single source of truth. `env!("CARGO_PKG_VERSION")` embeds the version at compile time. Git tags must match — enforced by `release.sh`. See [docs/versioning.md](./docs/versioning.md).

### SSE bus

The server includes a real-time event bus with namespaced topics (`content.comments`, `system.agents`, `system.boi`, `content.assets`). Clients subscribe via `GET /events/stream?topics=content.*`. Topic manifests at `.hex/sse/topics/*.yaml` define the contract — adding a topic means adding a YAML file, not editing bus code. hex-events are bridged to SSE via a policy, so any backend event becomes observable in real time.

---

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
boi.spec.completed → quality-spec-audit policy → quality-check.py --spec <id>
initiative.kr.met  → quality-kr-check policy   → quality-check.py --kr <init>/<kr>
timer.tick.6h      → quality-sweep policy       → quality-check.py --sweep
                                                     ↓
                                          hex.quality.gaming.detected
                                          hex.quality.kr.reverted
                                          hex.quality.suspect
```

The antagonist runs independently — it does not trust the metric command the worker used. It re-runs the metric, checks the math, and reverts KR status if fraud is confirmed. The charter lives at `system/reference/core-agents/quality-antagonist.yaml`.

**CLI:**

```bash
python3 .hex/scripts/quality-check.py --spec q-123      # audit one spec
python3 .hex/scripts/quality-check.py --kr init-foo/kr-1 # reality-check a KR
python3 .hex/scripts/quality-check.py --sweep           # scan last 24h
```

---

## Project layout (this repo)

```
hex-foundation/
├── install.sh           Single install entrypoint
├── VERSIONS             Pinned boi version (hex-events is now inline)
├── system/              → becomes ~/hex/.hex/ on install
│   ├── harness/         ← hex binary Rust source (agent + server + SSE + assets + comments)
│   │   ├── src/main.rs     unified CLI (hex agent/server/asset/comment/sse)
│   │   ├── src/server.rs   HTTP server + reverse proxy
│   │   ├── src/sse.rs      SSE bus (topics, subscriptions, publish)
│   │   ├── src/comments.rs comment API handler
│   │   ├── src/assets.rs   asset registry handler
│   │   ├── src/telemetry.rs request + event telemetry (append-only JSONL)
│   │   ├── src/wake.rs     agent wake cycle
│   │   └── build.rs        injects git SHA; Cargo.toml is version source
│   ├── events/          ← hex-events daemon, emitter, CLI (merged from hex-events repo)
│   │   ├── hex_eventd.py   main daemon
│   │   ├── hex_emit.py     event emitter
│   │   ├── hex_events_cli.py CLI
│   │   ├── actions/        action handlers (shell, emit, notify, render, update_file)
│   │   ├── adapters/       scheduler and timer adapters
│   │   ├── policies/       built-in default policies (boi-lifecycle, capture, etc.)
│   │   └── docs/           hex-events documentation
│   ├── sse/
│   │   └── topics/      ← SSE topic manifests (content.comments, system.agents, etc.)
│   ├── scripts/         startup.sh, doctor.sh, upgrade.sh, quality-check.py,
│   │                    route-comment.py, hex-asset.py, hex-asset-discover.py, ...
│   │   ├── health/      fleet self-driving scripts (fleet-pulse, stalled-initiative,
│   │   │                mike-pending, budget-period-reset, backlog-promote,
│   │   │                agent-performance-review, fleet-scorecard-aggregate)
│   │   ├── comments-service/  comment widget (widget.js) + Python fallback server
│   │   └── sse-bus/           hex-events → SSE bridge script
│   ├── commands/        → copied to ~/hex/.claude/commands/ (Claude Code) and ~/hex/.hex/commands/ (doctor/tooling)
│   ├── skills/          memory/ (index+search+save), landings, hex-reflect, hex-decide,
│   │                    hex-debrief, hex-consolidate, hex-doctor, hex-checkpoint,
│   │                    hex-shutdown, hex-startup, hex-triage
│   ├── policies/        quality-spec-audit, quality-kr-check, quality-sweep,
│   │                    quality-gaming-alert — event-driven quality gates
│   └── reference/       core-agents/ — quality-antagonist and fleet agent charters
├── adapter/
│   └── policy-templates/  Installable hex-events policy templates for fleet health
│                          (fleet-pulse, stalled-initiative-monitor, mike-pending-escalator,
│                          budget-period-reset, agent-performance-review-weekly, ...)
├── templates/           Seeds for CLAUDE.md, AGENTS.md, me.md, todo.md, decision-template.md
├── docs/architecture.md System overview
└── tests/
    ├── events/          hex-events unit and integration tests
    └── ...              E2E, layout, and memory tests
```

---

## Testing

The test suite verifies installation, migration, skill discovery, and Codex parity. See [`docs/testing.md`](./docs/testing.md) for the full matrix and how to run locally.

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
| `tests/migrate/test-migrate.sh` | v1 → v2 migration correctness |
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

v0.13.2 fixes: **Session-start checkpoint resume, integration-check emit fix, and memory leak fix.**
- **Channel checkpoint resume**: `session-start.sh` now surfaces `projects/<topic>/checkpoint.md` for `hex-<topic>` channels. Sessions pick up where they left off automatically. Generalized blocker-flag scan (`.hex/state/blockers/*.flag`) and topic-regex sanitization included.
- **Integration-check emit fix**: `export _error_raw` bug — 11,948+ events/day were emitted with `error: null` because the env var wasn't propagating into the command-substitution subshell. Fixed. Emit-throttle added for persistent fail streaks (heartbeat every 60 checks; no more event spam for a single dead probe).
- **Memory vector leak fixed**: `memory_index.py` now cascade-deletes `vec_chunks` orphans on re-index. 82,377 orphan rows (58% of the vec table) had accumulated because FTS5 chunk deletion didn't cascade to the `vec0` virtual table. Fixed.
- **Honest hybrid search**: `memory_search.py` documents `_rrf_merge` as FTS-only (KNOWN GAP) — `--hybrid` was paying embedding+vec-query cost without fusing vec results into the score.
- **Doctor: two new health modules**: Memory Vector Search (surfaces sqlite-vec drift) and hex-events Policy Load Errors (surfaces broken policies that were previously invisible to doctor).

v0.13.1 fixes: **Doctor reliability and skip_llm WakeConfig.**
- **Doctor streaming**: Doctor command streams output live via `Stdio::inherit` (was buffered — appeared to hang on slow modules). `hex-doctor` bash script also switched to `tee | tail -n +5` streaming with full PIPESTATUS capture.
- **Path bug fix**: 3 orchestration scripts had stale `CLAUDE_DIR=$HEX_DIR/.claude` (should be `.hex`). Caused 5 spurious doctor ERRORs on standard installs. Fixed.
- **BOI daemon detection**: LaunchAgent-aware detection replaces `pgrep`-based check.
- **`skip_llm` WakeConfig**: Agents that exercise wake plumbing without needing LLM reasoning (e.g. health probes) can set `wake.skip_llm: true`. Harness bypasses shift loop and self-assessment; inbox still loads and `mark_delivered` fires. `state.json` inbox drain prevents unbounded growth on high-frequency skip_llm wakes.

v0.13.0 adds: **Fleet self-driving mechanisms and agent performance review.**
- **Fleet pulse watchdog**: `check-fleet-pulse.sh` emits `hex.agent.needs-attention` events for dormant agents. Composite liveness score with WARN/ERROR escalation tiers. Suppresses during budget lockout.
- **Stalled initiative monitor**: `check-stalled-initiatives.sh` detects initiatives with no progress signal in 48h (commit, act trail, or KR update). Sends drive-or-close directives to initiative owners. Anti-spam guard prevents re-fire within 24h.
- **Mike-pending board monitor**: `check-mike-pending.sh` tracks Mike-blocked items with tier labels (quiet/digest/direct-ping). Coalesced per-run alerts with DM fallback.
- **Budget period auto-reset**: `budget-period-reset.py` rolls `cost.current_period.start` forward when a period expires. 5x runaway safety gate emits ERROR alert instead of clearing an out-of-control agent.
- **Agent performance review**: `agent-performance-review.py` produces per-agent quality/velocity/autonomy scorecards from critic reviews, BOI DB, audit trail, and Mike-pushback signals. Composite geometric mean (0.0–1.0) with cold-start handling.
- **Fleet scorecard aggregate**: `fleet-scorecard-aggregate.py` runs per-agent reviews and produces fleet-wide top/bottom 5, biggest movers, and Mike-pushback heatmap. Outputs a single coalesced digest — no per-agent pings.
- **Policy templates**: `adapter/policy-templates/` gains fleet-pulse, stalled-initiative-monitor, mike-pending-escalator, budget-period-reset, and agent-performance-review-weekly templates for out-of-the-box fleet health wiring.
- **Note**: Backlog auto-promotion (`wake.rs`) was staged but reverted — supporting modules incomplete. Tracked for v0.14.0.

v0.12.0 adds: **Upgrade reliability, shell completions, failure-revive protocol, and doctor improvements.**
- **Shell completions**: `hex completions bash|zsh|fish` generates completion scripts for all subcommands. Install snippets in README.
- **Upgrade reliability**: 5-bug patch to `/hex-upgrade` — stale-symlink cleanup, RC file detection, hex-binary version sync via Cargo.toml. Docker E2E 101/101.
- **Doctor enhanced**: hex events daemon status absorbed into health checks; hex-binary version-sync check added (catches binary/Cargo.toml divergence).
- **Failure-revive protocol**: three-strike detection + spec-owner-resolver + build-failure-brief for automated BOI failure analysis.
- **Policy validator**: `dagu` added to VALID_ACTION_TYPES, fixing false-positive validation errors.
- **AGENTS.md**: Quick Start, Gotchas, and How to Modify sections; Layer 2 Mechanisms condensed to compact table. Cross-repo navigation links added.
- **Cleanup**: cc-connect/slack-bot scripts removed (7 files). Attack surface reduced.

v0.10.0 adds: **BOI v1.1.0 integration + containerized BOI E2E.**
- **BOI v1.1.0**: pipeline-v2 phases (clean spec-pre / task / spec-post separation), interactive `boi dashboard` TUI, spec-critique↔spec-improve quality loop, deterministic phases (commit/merge/cleanup) that skip Claude. Upgrade: run `install.sh` again.
- **Containerized BOI E2E**: `tests/core-e2e/` suites cover fresh install, upgrade (catches stale-symlink bugs), and doctor runtime checks. CI-gated via GitHub Actions core-e2e workflow.
- **Doctor expanded**: `check_17` now runs `boi --help`, `boi --version`, and `boi status` instead of file-existence checks. Each failure includes a repair hint.

v0.11.0 adds: **Full hex sync sweep — 93 atomic units.**
- **New subsystems**: spec-tool (spec browsing + critic-loop UI), vibe-to-prod skill, conjecture-criticism skill, hex-fleet (system health monitor + LaunchAgent), boi-pm (BOI process monitor + LaunchAgent), hex-overseer (self-tuning monitor layer), pulse dashboard with E2E test harness, comments-service, sse-bus.
- **Improvements**: shared `hex_utils.py` library; 7 metrics scripts (continuity, done-claim, frustration, loop-waste, etc.); 6 doctor-checks; 16 health-checks (agent memory, BOI dispatch, cc-connect, MCP servers, etc.); skills: memory, hex-event, hex-save, hex-switch, x-twitter, hex-ideate, hex-triage, hex-upgrade, hex-sync-base, secret-intake, boi-delegation; 30+ MCP integration health-check wrappers.

v0.10.1 fixes: **Releaser auto-unblock regression.**
- **Harness queue.rs**: `check_unblock_condition` now handles `message_reply` blocks — previously only `telemetry` and `timer` arms existed, so releaser blocks were silently permanent.
- **Harness wake.rs**: `blocked_since` is now stamped with the server clock on apply, preventing LLM-hallucinated future timestamps from corrupting SLA math.
- **Tests**: 4 new tests in `tests/queue_test.rs` covering the unblock path. Build break fixes: `hex_bytes::encode` alias and `hex_agent` → `hex` rename in integration tests.

v0.9.0 adds: **BOI v1.0.0 Rust binary + doctor runtime checks.**
- **BOI rewrite**: BOI is now a compiled Rust binary at `~/.boi/bin/boi`. Install clones and builds from source; `VERSIONS` pins `BOI_VERSION`.
- **Doctor runtime checks**: `check_17` now validates `boi --help`, `boi --version` (against `VERSIONS`), `boi status` (DB queryable), dangling-symlink detection, and the full wrapper chain (`~/.boi/boi --help`). Each failure includes a repair hint.
- **Doctor unit tests**: `tests/test_doctor.bats` covers all new BOI checks (missing binary, dangling symlink, broken wrapper, version mismatch, status failure).

v0.8.0 adds: **Unified `hex` binary + 3 new primitives + hex-events merged inline.**
- **Unified binary**: `hex-agent` replaced by `hex` — single Rust binary with subcommands for agent fleet, HTTP/SSE server, asset registry, and comment system. `hex-agent` preserved as symlink for backward compat.
- **Asset registry**: unified `{type}:{id}` namespace for all hex artifacts (posts, proposals, specs, decisions, projects). Auto-discovery, periodic re-scan, CLI + HTTP API.
- **Unified comments**: single comment store, embeddable widget, LLM-classified routing to agents, action log with related assets.
- **SSE bus**: real-time event streaming with namespaced topics (`content.*`, `system.*`), wildcard subscriptions, topic manifests as self-documenting contract. hex-events bridged to SSE.
- **Telemetry**: append-only JSONL for all server requests and events, same pattern as agent harness.
- **hex-events merged**: standalone [hex-events](https://github.com/mrap/hex-events) repo (v0.2.0) merged into `system/events/`. hex-events repo archived.
- **Version system**: `Cargo.toml` is single source of truth, `env!("CARGO_PKG_VERSION")` embeds version at compile time. (v0.11.3 removed the `version.txt` sidecar.)
- **Migration**: install.sh handles upgrading from pre-0.8.0 (standalone `hex-agent` → `hex` + symlink). 122/122 E2E tests pass in Docker.

v0.3.0 adds: **Modular integration bundles + `hex-integration` CLI.** Every external surface (API, MCP, system service, refresh flow) lives in one directory under `integrations/<name>/` — manifest, probe, runbook, secrets schema, maintenance scripts, event policies, tests. `hex-integration install/uninstall/update/list/validate/status/probe/rotate` manages the lifecycle. Compile-step policy coupling: bundle event YAMLs compile into `~/.hex-events/policies/<name>-<stem>.yaml` with `# generated_from:` audit headers. See `docs/integrations.md` and `templates/integrations/_template/`.

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

## License

MIT. See [LICENSE](./LICENSE).
