---
name: hex-debrief
description: Debrief hex on what changed — projects, org signals, relationships, career. Run weekly or when hex prompts you.
allowed-tools: ["Bash", "Read", "Write", "Edit"]
---
<!-- # sync-safe -->

# Weekly Context Sync

Walk through each section below and update the relevant files. This is a structured weekly review to keep the agent's persistent knowledge fresh and catch drift before it compounds.

## Steps

### 1. Check in on projects
For each project in todo.md "Now" section, ask:
- What happened this week?
- Any status changes? (phase shifts, blockers, decisions made)
- Any new people involved?
- Update `todo.md` and `projects/*/context.md` with changes.

### 2. Org signals
- Any org changes, reorgs, or leadership shifts?
- Any signals from leadership that affect scope or priorities?
- Any team composition changes?
- Update `people/*/profile.md` if needed.

### 3. Relationship updates
- Any new observations about collaborators or stakeholders?
- Any new people you're working with?
- Any friction or wins worth noting?
- Create new `people/*/profile.md` files for new contacts.

### 4. Career check-in
- How's the career narrative shaping up?
- Any feedback from manager or peers?
- Any wins to document?
- Update `me/learnings.md` and `me/decisions/` as needed.

### 5. The juice
- What's the gossip this week?
- Any gut feelings or observations?
- Anything that feels off?
- Write sensitive context to `me/decisions/` (never shared with teams).

### 6. Weekly targets review
- Review current weekly targets in `landings/weekly/`
- Close out completed targets, carry over incomplete ones
- Set next week's targets if it's Friday

### 7. Log it
- Write distilled notes to the appropriate files.
- Rebuild memory index:
```bash
python3 $HEX_DIR/.hex/skills/memory/scripts/memory_index.py
```

### 8. Reminder
Tell the user: "Dump context anytime. Don't wait for next week's sync. The more I know, the more useful I am."
