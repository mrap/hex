<!-- # sync-safe -->
# hex v1 → v2 Migration Guide

If your hex instance has `.claude/scripts/`, `.claude/skills/`, `.claude/hooks/` etc., you're on the v1 layout and `/hex-upgrade` will NOT pull foundation v0.2.1+ content cleanly (the path references drift). You need to migrate to v2 first.

## One-liner migration

From your hex instance directory, run:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/mrap/hex-foundation/v0.2.2/bootstrap-migrate.sh)
```

That's it. The bootstrap will:

1. **Preflight** — verify git is clean, detect v1 layout, confirm you're in a hex instance
2. **Back up** — copy `.claude/` → `.claude.v1-backup-<timestamp>/`
3. **Migrate** — move hex-owned content to `.hex/`, rewrite path references, update hook paths in `settings.json`, create `.agents/skills` symlink, seed optional configs
4. **Commit** — one migration commit on your current branch
5. **Validate** — run `doctor.sh`, `startup.sh`, and `upgrade.sh --dry-run` to confirm everything works
6. **Auto-rollback** — if anything fails mid-run, restore from backup automatically

**Duration:** ~30 seconds for typical instances. Your `.claude/memory.db` (which is usually 100s of MB) is moved via a single `mv`, not re-indexed.

## What changes

### Before (v1)
```
your-hex-instance/
├── .claude/
│   ├── scripts/       ← hex-owned
│   ├── skills/        ← hex-owned
│   ├── hooks/         ← hex-owned
│   ├── commands/
│   ├── memory.db      ← hex-owned
│   ├── settings.json  ← Claude Code config, hooks point to .claude/hooks/
│   └── (other hex stuff)
```

### After (v2)
```
your-hex-instance/
├── .claude/              ← narrowed to Claude Code discovery
│   ├── commands/         ← mirror of .hex/commands/ (Claude Code looks here)
│   ├── settings.json     ← hooks now point to .hex/hooks/
│   └── settings.local.json
├── .hex/                 ← NEW: hex-owned implementation
│   ├── scripts/
│   ├── skills/
│   ├── hooks/
│   ├── commands/         ← source of truth
│   ├── memory.db
│   └── (everything hex)
├── .agents/skills → ../.hex/skills   ← NEW (symlink)
├── .codex/config.toml                 ← NEW (for Codex CLI, if installed)
```

Anything in `.claude/` that the migrator doesn't recognize (custom dirs, companion state) stays put unless you pass `--force`.

## Preconditions

- `git status` is clean (commit or stash first)
- You're on a branch (not detached HEAD)
- `git`, `curl`, `python3` installed
- You're NOT currently running a Claude Code session in this instance (kill it first — live hooks fire against paths that are about to move)

## Environment variables

You can override bootstrap defaults:

| Variable | Default | Purpose |
|---|---|---|
| `HEX_DIR` | current directory | Target hex instance |
| `HEX_FOUNDATION_VERSION` | `v0.2.2` | Foundation tag to pull migrator from |
| `HEX_FOUNDATION_REPO` | `https://raw.githubusercontent.com/mrap/hex-foundation` | Fork-friendly override |

Example (migrating from a custom fork):
```bash
HEX_DIR=/path/to/hex \
HEX_FOUNDATION_REPO=https://raw.githubusercontent.com/you/your-fork \
bash <(curl -fsSL https://raw.githubusercontent.com/you/your-fork/v0.2.2/bootstrap-migrate.sh)
```

## Rollback

If you need to undo the migration after it completes:

```bash
bash .hex/scripts/migrate-v1-to-v2.sh --rollback --hex-dir "$(pwd)"
```

This restores from the most recent `.claude.v1-backup-*` and moves the migrated `.hex/` aside. You then need to `git reset --hard <pre-migration-sha>` to undo the migration commit.

## What the migrator does NOT do

- **Does not touch runtime data**: `me/`, `projects/`, `people/`, `evolution/`, `landings/`, `raw/`, `todo.md` are left alone.
- **Does not touch companion repos**: `~/.boi/`, `~/.hex-events/` stay exactly as they are.
- **Does not update LaunchAgents**: plists in `~/Library/LaunchAgents/` are NOT inspected or modified. (In the reference migration, none referenced `.claude/` paths anyway — companion daemons live elsewhere.)
- **Does not push anywhere**: all changes stay local on your branch. You merge/push when you're ready.

## Troubleshooting

### "Hybrid state detected"
You have both `.claude/scripts/` and `.hex/scripts/`. A previous migration attempt partially succeeded. Either:
- Complete it manually (inspect which dir has the latest content)
- Or roll back: `bash migrate-v1-to-v2.sh --rollback` then start over

### "Unknown entries under .claude/"
The migrator found files/dirs in `.claude/` that aren't in the standard hex v1 manifest. By default it refuses to proceed. Options:
- Move them elsewhere first if they shouldn't be there
- Pass `--force` to leave them in place (they'll stay in `.claude/`)

### Doctor warnings after migration
Warnings like "AGENTS.md missing" are pre-existing gaps, not migration failures. Run:
```bash
bash .hex/scripts/doctor.sh --fix
```

### Unexpected failure mid-run
The bootstrap auto-rolls-back. You're restored to the pre-migration state. Open an issue with the error log.

## After migration

- `/hex-upgrade --dry-run` should now be plain rsync against foundation v0.2.2
- Bump your `VERSIONS` file: `HEX_FOUNDATION_VERSION=v0.2.2`
- You can delete `.claude.v1-backup-*` after a week of confirmed stability
