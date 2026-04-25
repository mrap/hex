---
name: hex-upgrade
description: Upgrade hex to the latest version from hex
---

# /hex-upgrade — Upgrade Hexagon

Pull the latest scripts, skills, commands, and hooks from hex.

## Step 1: Run the upgrade script

```bash
bash $AGENT_DIR/.hex/scripts/upgrade.sh
```

If the user passed arguments (e.g., `--dry-run`, `--local PATH`), forward them:
```bash
bash $AGENT_DIR/.hex/scripts/upgrade.sh ARGUMENTS
```

## Step 2: Handle CLAUDE.md template changes

If the upgrade script reports that the CLAUDE.md template has changed:

1. Read the new template from the upgrade cache:
   ```
   $AGENT_DIR/.claude/.upgrade-cache/templates/CLAUDE.md.template
   ```

2. Read the current `$AGENT_DIR/CLAUDE.md`

3. Detect the user's `{{NAME}}` and `{{AGENT}}` values from the current CLAUDE.md:
   - `{{NAME}}` = the name used throughout (e.g., "Mike Rapadas")
   - `{{AGENT}}` = the agent name from the file index section or title

4. Merge intelligently:
   - Apply structural changes from the new template (new sections, updated protocols, fixed instructions)
   - **Preserve** user additions: custom standing orders (rows beyond the defaults), custom sections, any content the user added
   - **Preserve** the Environment Paths section if the user customized it
   - Substitute `{{NAME}}`, `{{AGENT}}`, and `{{DATE}}` with the detected values

5. Show the user a summary of what changed in CLAUDE.md and ask for confirmation before writing.

## Step 3: Rebuild memory index

After upgrade, rebuild the memory index to pick up any changes:
```bash
python3 $AGENT_DIR/.hex/skills/memory/scripts/memory_index.py
```

## Step 4: Report

Show a concise summary:
- What files were updated/added
- Whether CLAUDE.md was merged
- Any new commands or skills that were added
- Remind: "Run `/hex-startup` to load the updated configuration."

## First-Time Setup

If no `$AGENT_DIR/.hex/upgrade.json` exists, create one:

```json
{
  "repo": "https://github.com/mrap/hex.git",
  "last_upgrade": "YYYY-MM-DD"
}
```

After a successful upgrade, update `last_upgrade` to today's date.
