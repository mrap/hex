---
name: hex-doctor
description: >
  Validate hex agent structure and repair issues. Runs health checks against
  the expected directory layout, reports findings by severity, and auto-fixes
  safe issues. Also handles data migration from backup directories on first
  launch after bootstrap migration. Use when the user says "hex doctor",
  "check health", "fix my hex", "something is broken", or on first launch
  when .hex/migrate-from exists.
version: 1.0.0
---
<!-- # sync-safe -->

# Hex Doctor

Validates and repairs your hex agent installation.

## Modes

### Health Check (default)

Run when the user invokes hex-doctor or when the startup health check detects issues.

1. Run `hex doctor run --fix` to auto-fix all scriptable issues
2. Parse the output for any unfixed errors (checks that doctor.sh cannot fix: .hex/, skills/, CLAUDE.md, AGENTS.md)
3. Handle LLM-fixable issues:
   - **AGENTS.md missing**: Generate from CLAUDE.md (requires understanding format differences between Claude and Codex)
   - **Complex symlink decisions**: Prompt the user if `.agents/skills/` exists as a real non-empty directory (doctor.sh warns and skips this case)
4. Present summary of all findings (from doctor.sh output + any LLM fixes applied)

### Migration (when .hex/migrate-from exists)

Run automatically on first launch after bootstrap migration.

1. Read `.hex/migrate-from` to find backup directory
2. Verify backup directory exists and is readable
3. Migrate user data from backup to current agent:
   - `me/` (me.md, learnings.md, decisions/)
   - `projects/*/`
   - `people/*/`
   - `raw/` (transcripts, messages, calendar, docs, captures)
   - `evolution/` (observations, suggestions, changelog, metrics)
   - `landings/` (daily + weekly)
   - `todo.md`
   - `teams.json`
   - `.hex/memory.db` (then re-index)
   - `.hex/settings.local.json`
   - User-created custom skills (skills not in the template)
4. Run health check to verify migration
5. Present migration report
6. Remove `.hex/migrate-from` breadcrumb
7. Rebuild memory index

## Health Checks

`hex doctor run --fix` owns the check registry (`system/harness/src/doctor/checks/` in hex-foundation) — that list is the source of truth, not a copy here. It currently runs checks across three categories:

- **Health** — structural: `.hex/`, skills present and populated, git init, symlinks, memory/telemetry/vector-search liveness, script permissions, binary/interpreter availability.
- **Config** — settings and credentials: CLAUDE.md/AGENTS.md presence and freshness, Codex parity (CLI, version, API key, AGENTS.md sections), `me/me.md`, `todo.md`, LLM preference, `settings.json`, timezone.
- **Registry** — skill/bin metadata: orphaned entries, stale policy.

Each check reports Pass, Warn, Fail, Fixed, or Skip. Read the actual list and statuses from the script's own output for each run — the registry changes across releases, so report what `hex doctor run --fix` says, not a hardcoded checklist.

## Migration Data Handling

When migrating user data from backup:

- **Copy, don't move.** The backup stays intact until the user explicitly deletes it.
- **Read before writing.** Check if the destination already has content (from a partial previous migration). If so, skip that item and report it.
- **Verify after each item.** After copying, verify the file exists and is readable at the destination.
- **Re-index memory.** After migration, run `hex memory index --full` to rebuild the search index with correct paths.

## Output Format

```
Hex Doctor — Health Check
━━━━━━━━━━━━━━━━━━━━━━━━

  ✓ .hex/ exists
  ✓ .git/ initialized
  ✓ .hex/ exists (19 skills)
  ✓ .agents/skills/ linked
  ✓ CLAUDE.md (25KB)
  ✓ AGENTS.md present
  ⚠ .codex/config.toml missing — created
  ✓ me/me.md has user data
  ✓ todo.md exists
  ✓ memory.db valid (154 chunks)
  ✓ No broken symlinks
  ✓ Scripts executable
  ✓ .hex/llm-preference = codex

  Result: 12 passed, 1 fixed, 0 errors
```

## Migration Output Format

```
Hex Doctor — Migration
━━━━━━━━━━━━━━━━━━━━━━

  Backup: /home/user/myagent.backup-2026-03-18-143022

  Migrating user data...
  ✓ me/me.md (1.2KB)
  ✓ me/learnings.md (4.5KB)
  ✓ me/decisions/ (3 files)
  ✓ projects/ (12 projects)
  ✓ people/ (3 profiles)
  ✓ raw/ (89 captures, 45 transcripts)
  ✓ evolution/ (4 files)
  ✓ landings/ (45 daily, 6 weekly)
  ✓ todo.md (8.1KB)
  ✓ teams.json
  ✓ memory.db → re-indexed (312 chunks)

  Running health check...
  Result: 15 passed, 0 errors

  Migration complete. Backup preserved at:
  /home/user/myagent.backup-2026-03-18-143022

  You can delete it when you're confident everything migrated correctly.
```
