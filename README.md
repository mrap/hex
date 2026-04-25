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
- Rust / cargo ([rustup.rs](https://rustup.rs)) — required to build the hex-agent harness from source; without it, install attempts a pre-built binary download (fails hard if neither works)

The installer also clones two companion repos into `~/.boi` and `~/.hex-events`. Versions pinned in [`VERSIONS`](./VERSIONS).

### Install options

```bash
bash install.sh              # installs to ~/hex
bash install.sh ~/my-hex     # custom location
```

To use a fork of the companions, set `HEX_BOI_REPO` and/or `HEX_EVENTS_REPO` before running install.

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
- **[`~/.hex-events`](https://github.com/mrap/hex-events)** — reactive event policies

### Auto-configured by install.sh

- **Claude Code hooks** — `install.sh` writes `PreToolUse`, `PostToolUse`, `Stop`, and `SessionStart` hooks into `.claude/settings.json` automatically. No manual hook setup required.
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
4. Merges `CLAUDE.md`: system zone replaced, user zone preserved
5. Runs `doctor.sh`

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

hex-agent (the multi-agent harness) is **core infrastructure**, not optional.
It drives all agent wakes, fleet management, cost tracking, gate validation,
and initiative execution. Without it, hex is a collection of scripts with
no orchestration.

Requirements: Rust toolchain (for building from source) or a supported
platform for pre-built binaries (macOS arm64/x86_64, Linux x86_64).

The harness binary is built or downloaded automatically by `install.sh`. If the
harness cannot be built or downloaded, install warns and continues — core
scripting still works, but agent wakes and fleet management require the binary to
be present. Run `hex-doctor` to verify harness status after install.

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
├── VERSIONS             Pinned boi / hex-events versions
├── system/              → becomes ~/hex/.hex/ on install
│   ├── scripts/         startup.sh, doctor.sh, upgrade.sh, quality-check.py, ...
│   ├── commands/        → copied to ~/hex/.claude/commands/ (Claude Code slash commands)
│   ├── skills/          memory/ (index+search+save), landings, hex-reflect, hex-decide,
│   │                    hex-debrief, hex-consolidate, hex-doctor, hex-checkpoint,
│   │                    hex-shutdown, hex-startup, hex-triage
│   ├── policies/        quality-spec-audit, quality-kr-check, quality-sweep,
│   │                    quality-gaming-alert — event-driven quality gates
│   ├── reference/       core-agents/ — quality-antagonist and fleet agent charters
│   └── version.txt
├── templates/           Seeds for CLAUDE.md, AGENTS.md, me.md, todo.md, decision-template.md
├── docs/architecture.md System overview
└── tests/               E2E, layout, and memory tests
```

---

## Testing

The test suite verifies installation, migration, skill discovery, and Codex parity. See [`docs/testing.md`](./docs/testing.md) for the full matrix and how to run locally.

Key test files:

| Test | What it verifies |
|------|-----------------|
| `tests/test_skill_frontmatter.sh` | Every SKILL.md has valid YAML frontmatter (name + description) |
| `tests/test_skill_refs.sh` | All paths referenced inside SKILL.md resolve after a fresh install |
| `tests/test_skill_discovery.sh` | Claude Code discovers all 11 shipped skills and invokes at least 3 |
| `tests/test_skill_discovery_codex.sh` | Codex equivalent of skill discovery test |
| `tests/test_e2e.sh` | Full install + doctor + upgrade lifecycle |
| `tests/migrate/test-migrate.sh` | v1 → v2 migration correctness |

To run the full suite locally:

```bash
# Static tests (no API key needed)
bash tests/test_skill_frontmatter.sh
bash tests/test_skill_refs.sh

# Live tests (requires ~/.hex-test.env with ANTHROPIC_API_KEY)
bash tests/eval/run_eval_docker.sh --live    # Linux Docker
bash tests/eval/run_eval_macos.sh            # macOS Tart
```

---

## Roadmap

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

## License

MIT. See [LICENSE](./LICENSE).
