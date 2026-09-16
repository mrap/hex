---
name: hex-upgrade
description: Upgrade hex to the latest version from hex
---

# /hex-upgrade — Upgrade Hexagon

Pull the latest scripts, skills, commands, and hooks from hex.

## Step 1: Run the upgrade command

```bash
hex upgrade
```

If the user passed arguments (e.g., `--dry-run`, `--local PATH`), forward them:
```bash
hex upgrade ARGUMENTS
```

## Step 2: Handle AGENTS.md template changes

If the upgrade script reports that the AGENTS.md template has changed:

1. Read the new template from the upgrade cache:
   ```
   $HEX_DIR/.claude/.upgrade-cache/templates/AGENTS.md.template
   ```

2. Read the current `$HEX_DIR/AGENTS.md` (CLAUDE.md is a symlink to it)

3. Detect the user's `{{NAME}}` and `{{AGENT}}` values from the current AGENTS.md:
   - `{{NAME}}` = the name used throughout (e.g., "Your Name")
   - `{{AGENT}}` = the agent name from the file index section or title

4. Merge intelligently:
   - Apply structural changes from the new template (new sections, updated protocols, fixed instructions)
   - **Preserve** user additions: custom standing orders (rows beyond the defaults), custom sections, any content the user added
   - **Preserve** the Environment Paths section if the user customized it
   - Substitute `{{NAME}}`, `{{AGENT}}`, and `{{DATE}}` with the detected values

5. Show the user a summary of what changed in AGENTS.md and ask for confirmation before writing.

## Step 2b: Merge required hooks into settings.json

Hooks declared in the foundation manifest must reach existing instances too, not only fresh installs:
```bash
python3 "$HEX_DIR/.hex/scripts/hex-hooks-merge" "$HEX_DIR/.hex/hooks/required-hooks.json" "$HEX_DIR/.claude/settings.json"
```
It prints one line per hook it added (idempotent; nothing printed means already wired). Report any additions in Step 4.

## Step 3: Rebuild memory index

After upgrade, rebuild the memory index to pick up any changes:
```bash
hex memory index
```

## Step 4: Report

Show a concise summary:
- What files were updated/added
- Whether AGENTS.md was merged
- Any new commands or skills that were added
- Whether VERSIONS was updated (upgrade.sh syncs `HEX_FOUNDATION_VERSION` and `BOI_VERSION` from Cargo.toml automatically)
- Note: the updated configuration loads automatically on the next hex launch (lean recency prime via `SessionStart`). hex is session-less — there is no startup command to run.

## First-Time Setup

If no `$HEX_DIR/.hex/upgrade.json` exists, create one:

```json
{
  "repo": "https://github.com/mrap/hex.git",
  "last_upgrade": "YYYY-MM-DD"
}
```

After a successful upgrade, update `last_upgrade` to today's date.
