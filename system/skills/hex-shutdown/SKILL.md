---
name: hex-shutdown
description: >
  Fire-and-forget session close. Quick distill pass, deregister session, done.
  Reflection and memory indexing run in the background via Stop hooks.
---
<!-- # sync-safe -->

# /hex-shutdown — Close Session

## Design

This command is fire-and-forget. Heavy work (reflection, transcript parsing,
memory index rebuild) runs automatically in the background via Stop hooks after
the session ends. The shutdown command itself only does quick, inline work.

Background processes triggered by Stop hooks:
- `backup_session.sh` — saves transcript to raw/transcripts/
- `session-reflect.sh` — runs reflection via `claude -p`, applies fixes, logs to evolution/reflection-log.md

## Steps

1. **Quick distill pass**: Scan the current conversation for any context that hasn't been written to files yet. Check for:
   - Decisions made but not logged to decisions/
   - Person info mentioned but not in people/
   - Project updates not written to projects/
   - Action items not in todo.md
   Write anything found to the correct location. Skip if the session was very short (< 5 exchanges) with no meaningful context.

2. **Deregister session**: Pass the session ID that was printed during startup. If you don't remember it, run `bash $HEX_DIR/.hex/scripts/session.sh check` to list active sessions, identify yours by start time, and pass that ID.

```bash
bash $HEX_DIR/.hex/scripts/session.sh stop <SESSION_ID>
```

3. **Report**: "Session closed. Reflection and cleanup will run in the background."

That's it. Do not invoke hex-reflect inline. Do not run transcript parsing or memory index rebuild. These happen automatically via Stop hooks when the session ends.
