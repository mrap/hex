<!-- # sync-safe -->
# hex-foundation

A minimal, installable template for the hex agent system — a persistent AI workspace for Claude Code that accumulates context, learns your patterns, and improves itself over time.

**For:** engineers on Claude Code who are tired of their agent starting from zero every session.

---

## Quick start

```bash
git clone https://github.com/mrap/hex-foundation /tmp/hex-setup
bash /tmp/hex-setup/install.sh
cd ~/hex && claude
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

## Project layout (this repo)

```
hex-foundation/
├── install.sh           Single install entrypoint
├── VERSIONS             Pinned boi / hex-events versions
├── system/              → becomes ~/hex/.hex/ on install
│   ├── scripts/         startup.sh, doctor.sh, upgrade.sh, today.sh, path-mapping.sh
│   ├── commands/        → copied to ~/hex/.claude/commands/ (Claude Code slash commands)
│   ├── skills/          memory/ (index+search+save), landings, hex-reflect, hex-decide,
│   │                    hex-debrief, hex-consolidate, hex-doctor, hex-checkpoint,
│   │                    hex-shutdown, hex-startup, hex-triage
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
