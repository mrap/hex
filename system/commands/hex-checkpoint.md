# /hex-checkpoint — Mid-Session Save

Persist context so a future session can resume.

## Steps

1. **Distill pass:** Scan recent messages for notable context not yet written to files. Write any findings immediately:
   - Person info → `people/{name}/profile.md`
   - Project updates → `projects/{project}/context.md`
   - Decisions → `me/decisions/{topic}-YYYY-MM-DD.md`
   - Tasks → `todo.md`

2. **Write handoff file:** Create `raw/handoffs/YYYY-MM-DD-HHMMSS.md` with:
   - Current task and status
   - Key decisions made this session
   - Files modified
   - Open questions
   - Next steps

3. **Update landings:** If today's landings file exists, update status on any items that changed.

4. **Rebuild memory index:**
   ```bash
   python3 .hex/skills/memory/scripts/memory_index.py
   ```

5. **Report:** "Checkpointed. Context saved to `raw/handoffs/`. Memory index rebuilt."
