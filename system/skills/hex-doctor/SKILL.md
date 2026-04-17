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

Run when the user invokes hex-doctor or when hex-startup detects issues.

1. Run `bash $HEX_DIR/.hex/scripts/doctor.sh --fix` to auto-fix all scriptable issues
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

Run these checks in order. For each, report the result and fix if possible.

| # | Check | Severity | Auto-fix |
|---|-------|----------|----------|
| 1 | `.hex/` directory exists | error | mkdir |
| 2 | `.git/` initialized | error | git init |
| 3 | `.hex/` directory exists with skills | error | cannot fix (re-run bootstrap) |
| 4 | `.hex/skills/` has skill directories | error | cannot fix (re-run bootstrap) |
| 5 | `.agents/skills/` linked to `.hex/skills/` | error | create symlink |
| 6 | `CLAUDE.md` exists and is >1000 bytes | error | cannot fix |
| 7 | `AGENTS.md` exists | warning | generate from CLAUDE.md |
| 8 | `.codex/config.toml` exists | warning | create with CLAUDE.md fallback |
| 9 | `me/me.md` exists and has content | info | report only |
| 10 | `todo.md` exists | warning | create skeleton |
| 11 | `memory.db` exists | warning | rebuild index |
| 12 | No broken symlinks in agent dir | error | remove and recreate |
| 13 | All `.sh` scripts in `.hex/scripts/` are executable | warning | chmod +x |
| 14 | `.hex/llm-preference` exists | warning | detect CLI and create |
| 15 | No stale `.hex/llm-preference` conflicts | warning | resolve to canonical location |

## Migration Data Handling

When migrating user data from backup:

- **Copy, don't move.** The backup stays intact until the user explicitly deletes it.
- **Read before writing.** Check if the destination already has content (from a partial previous migration). If so, skip that item and report it.
- **Verify after each item.** After copying, verify the file exists and is readable at the destination.
- **Re-index memory.** After migration, run `python3 .hex/skills/memory/scripts/memory_index.py --full` to rebuild the search index with correct paths.

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
