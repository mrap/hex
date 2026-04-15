# hex Architecture

## Overview

hex is a persistent AI agent system. It installs as a workspace directory (`~/hex/` by default) that any AI agent can work in. The workspace accumulates context, learns patterns, and improves itself over time.

## Components

| Component | Location | Purpose |
|-----------|----------|---------|
| CLAUDE.md | `~/hex/CLAUDE.md` | Operating model for Claude Code. Standing orders, learning engine, improvement engine. |
| AGENTS.md | `~/hex/AGENTS.md` | Simplified operating model for non-Claude agents. |
| Memory | `~/hex/.hex/memory.db` | SQLite FTS5 database. Stores explicit memories and indexed file chunks. |
| User data | `~/hex/me/`, `projects/`, `people/` | User's context, learnings, decisions, relationships. Never touched by upgrades. |
| Evolution | `~/hex/evolution/` | Self-improvement engine. Observations, suggestions, changelog. |
| System | `~/hex/.hex/` | Scripts, skills, templates. Overwritten on upgrade. |

## Data Flow

```
User message
  → Agent reads CLAUDE.md (operating model)
  → Agent searches memory (memory_search.py)
  → Agent responds + persists context to files
  → Agent records observations to me/learnings.md
  → Improvement engine detects patterns in evolution/observations.md
  → Suggestions surface in evolution/suggestions.md
  → Approved improvements get implemented and tracked in evolution/changelog.md
```

## Upgrade Boundary

| Directory | Owned by | Touched by upgrade? |
|-----------|----------|---------------------|
| `.hex/` | System | Yes (overwritten) |
| `CLAUDE.md` | System + User | System zone replaced, user zone preserved |
| `AGENTS.md` | System + User | Same as CLAUDE.md |
| `me/`, `projects/`, `people/` | User | Never |
| `evolution/`, `landings/`, `raw/` | User | Never |
| `todo.md`, `specs/` | User | Never |

## Memory System

Two storage mechanisms in one database:

1. **Explicit memories** (`memories` table) — Facts saved by the agent via `memory_save.py`. Synced to FTS5 via triggers.
2. **File chunks** (`chunks` FTS5 table) — Workspace markdown files chunked by heading via `memory_index.py`. Incremental indexing with content hashing.

Search queries both and merges results.
