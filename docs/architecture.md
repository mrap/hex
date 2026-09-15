# Hex System Architecture

> Entry point for the hex system. ~5 minute read.

## What is Hex?

Hex is a Claude Code + BOI workspace: an opinionated agent runtime layer
plus a delegation engine. There is no agent fleet, no initiative loop, no
session-lifecycle daemon, no charter-driven autonomy tiers. Those framings
were demolished — see the stale-references audit
(`$HEX_DIR/projects/system-improvement/audits/`).

Two components, plainly:

- **Claude Code (the runtime)** — reads `AGENTS.md` / `CLAUDE.md` and works
  inside this repo. All persistence is plain files on disk.
- **BOI (the delegation engine)** — `~/.boi/`. Dispatches TOML spec files
  to fresh Claude Code workers running in isolated git worktrees. BOI is the
  only delegation surface; no other queue, bus, or scheduler.

Everything else is files, plus a thin Rust harness for memory / doctor
checks (`system/harness/`, exposed as `boi dashboard` and a handful of
`hex memory …` subcommands).

---

## System Diagram

> BOI dispatch is paused (2026-09-15, Standing Order 6). The orchestrator box below is parked; multi-step work runs in Claude Code (worktree, Sonnet subagents, or a `Workflow`).

```
  ┌──────────────────────────────────────────────────────────────────┐
  │  Claude Code session                                             │
  │    reads AGENTS.md → executes → edits files in-place             │
  │    dispatches multi-step work to BOI                             │
  └────────────────────────┬─────────────────────────────────────────┘
                           │ boi dispatch <spec.toml>
                           ▼
  ┌──────────────────────────────────────────────────────────────────┐
  │  BOI (~/.boi/)                                                   │
  │    daemon ──► worker N ──► fresh Claude Code (isolated worktree) │
  │    TOML spec queue → iterate → verify → integrate                │
  └──────────────────────────────────────────────────────────────────┘
```

---

## Components

| Component | Role | Location | Verify |
|-----------|------|----------|--------|
| BOI | Delegation engine — dispatch TOML specs to Claude Code workers | `~/.boi/` | `~/.boi/bin/boi dashboard` |
| hex harness | Rust binary: memory DB, doctor checks, dashboard | `system/harness/` | `cargo build --release -p hex-harness` |
| Skills | Shell + markdown skill bundles, read on demand | `system/skills/` | `ls system/skills/` |
| Templates | What `hex new` / `hex upgrade` writes into a workspace | `templates/` | — |

---

## Data Flow

| Data | Lives in | Notes |
|------|----------|-------|
| Spec queue | `~/.boi/boi.db` | BOI internal state |
| Spec files | `specs/` (or anywhere on disk) | TOML — never YAML |
| Agent rules | `AGENTS.md` (canonical), `CLAUDE.md` (symlink) | Standing Orders live here |
| Memory index | `.hex/memory.db` (SQLite FTS5) | Full-text + embedding search over workspace markdown |
| Decisions | `me/decisions/*.md`, `projects/*/decisions/*.md` | Plain markdown, dated |
| Todo | `todo.md` | Single source of truth for priorities |

There is no session-marker directory, no audit jsonl, no KR snapshots,
no approach library, no pivot trail, no cost ledger jsonl. References
to those files in older docs are stale.

---

## Sessions

A Claude Code session is one conversation. There is no startup hook,
no shutdown protocol, no checkpoint/handoff dance, and no scheduled
daily briefing. Just: open a session, read `AGENTS.md`, do the work,
write files, close the session. State persists in files on disk;
nothing is held in a daemon between sessions.

If a session is getting long, the runtime decides when to compact —
you do not write a handoff file by hand.

---

## Spec IDs

Specs and audits use Crockford base32 IDs (e.g. `Sh9f0hty0`), not
the legacy `q-NNN` numbering. BOI assigns them at dispatch.

---

## Replacing Components

| Component | Swappable? | Notes |
|-----------|-----------|-------|
| BOI | In principle | Any engine that dispatches TOML specs to Claude Code workers in worktrees |
| Skills | Yes | Add/remove under `system/skills/<name>/SKILL.md` |
| Runtime | Yes | Any runtime that reads `AGENTS.md` and exposes file edit + shell works |

---

## Design Principles

1. **Files over daemons.** Persistence is plain markdown + SQLite. No long-running state machines.
2. **Verify behavior, not file existence.** A spec's verify must run the script and assert on output, not `test -f`.
3. **Integration before dispatch.** Define the contract (what writes, what reads, what schema) before firing a BOI spec at a shared component.
4. **Mechanical enforcement, not textual rules.** A rule that is only prose in `AGENTS.md` is documentation, not enforcement; if it must be enforced, wire a harness check.

---

## Tombstoned Concepts

The following concepts appear in older docs / commits and are **not live**.
If you find a reference, it is stale — flag it for the next audit pass.

- Session lifecycle (FRESH → ACTIVE → WARMING → HOT → CHECKPOINT)
- The deleted session-lifecycle slash commands (startup / shutdown / checkpoint / save / reflect)
- Scheduled daily briefing
- Agent fleet, fleet-lead, fleet-pulse, fleet-scorecard
- Initiatives, KRs, key-result snapshots, pattern library, pivot trail
- Charter-driven autonomy tiers (A0–A4), `charter.yaml`
- L1–L4 feedback loops, self-improvement cycle, `hex-initiative-loop-v2.py`
- SSE event bus, policy engine, comments-service
- The legacy shell release pipeline script (deleted; releases are `hex release cut` — see [versioning.md](versioning.md))
- `boi status`, `boi version` (use `boi dashboard`, `boi --version`)

---

## Deep Dives

Per-subsystem architecture documents live in [architecture/](architecture/) — start
with the [registry & standard of practice](architecture/README.md), which defines how
these docs stay verified against the code (machine-readable `verified-against:`
headers; docs ride the change; registry pinned by a harness test).

| Subsystem | Doc |
|-----------|-----|
| Memory pipeline (index · distill · recall · consolidation · maintain · backup) | [architecture/memory.md](architecture/memory.md) |

---

## Further Reading

| Doc | Contents |
|-----|----------|
| [capabilities-map.md](capabilities-map.md) | What hex can do, by domain |
| [testing.md](testing.md) | Test matrix and how to run it |
| [versioning.md](versioning.md) | Version source of truth and bump flow |
| [hex-ops.md](hex-ops.md) | Operational scripts reference |
| [iii-hex.md](iii-hex.md) | iii engine mental model and trigger config — read when wiring iii workers or triggers |
| [doc-maintenance.md](doc-maintenance.md) | Doc routing map + verified-claim checks — read before any documentation pass |
