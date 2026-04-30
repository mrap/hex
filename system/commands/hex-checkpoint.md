---
name: hex-checkpoint
description: >
  Non-blocking checkpoint. Quick distill pass, handoff file, todo update, landings refresh. Suggest compact when done.
  Reflection runs in the background via session-reflect.sh — never blocks the session.
---
# sync-safe

# /hex-checkpoint — Checkpoint and Continue

Persist context from the current conversation, then suggest compact if the user wants a fresh context window.
Heavy work (reflection, memory index rebuild) is dispatched to the background
so the user can keep working immediately.

## Arguments

The user may pass a focus directive (what they want to work on next). Use it as the compact summary.

## Step 1: Quick distill pass

Scan the conversation for unpersisted context and write it to files:

- Decisions made (write to `me/decisions/` or `projects/*/decisions/`)
- People mentioned (write to `people/*/profile.md`)
- Project updates (write to `projects/*/context.md`)
- New tasks or priority changes (update `todo.md`)
- Patterns noticed (update `evolution/observations.md`)

If the session segment was very short (< 5 exchanges) with no meaningful context, skip and note "Distill skipped (short segment)."

## Step 2: Dispatch background reflection

Run reflection asynchronously so it does not block the checkpoint. Use the Task
tool with `run_in_background: true` to dispatch a subagent that invokes the
reflection script:

```
Task (run_in_background: true):
  "Dispatch session reflection in the background."
  subagent_type: general-purpose
  prompt: |
    Run the session reflection script. Execute:
    bash $HEX_DIR/.hex/scripts/session-reflect.sh
    This will process the current transcript and apply reflection fixes.
    Do not wait for or report the result.
```

If the session has been very short (< 5 exchanges) with no corrections or
pushback, skip reflection entirely and note "Reflection skipped (short segment,
no corrections)."

Do NOT invoke `/hex-reflect` inline. Do NOT wait for reflection to complete.

## Step 3: Write handoff file

Write a structured handoff to `raw/handoffs/YYYY-MM-DD-HHMMSS.md`:

```markdown
# Session Handoff — YYYY-MM-DD HH:MM

## What We Did
- (bullet list of accomplishments)

## Key Decisions
- (any decisions made with reasoning)

## Open Threads
- (anything in progress or unresolved)

## Next Focus
- (what the user wants to work on next)

## Files Modified This Session
- (list of files created or changed)
```

## Step 4: Update todo.md

Make sure todo.md reflects current state. Move completed items, add new ones discovered during the session.

## Step 5: Update daily landings

Get today's date: `bash $HEX_DIR/.hex/scripts/today.sh`

Read today's landings file at `landings/YYYY-MM-DD.md`. If it exists, update it:
- Mark any landing items completed during this session segment as Done
- Update status of in-progress items
- Update open threads with current state
- Append a changelog entry: `- HH:MM — Checkpoint: [brief summary of what changed]`

If no landings file exists for today, skip this step.

## Step 6: Suggest compact (optional)

Tell the user: "Checkpointed. Reflection dispatched to background."

Then suggest: "If you want to compact for a fresh context window, run:"

Provide the compact command with a focused summary:
- The next focus area (from arguments or from the handoff)
- A pointer to the handoff file
- Key files to re-read after compact

Example:
> `/compact [Next focus]. Handoff at raw/handoffs/[filename]. Re-read: todo.md, landings/YYYY-MM-DD.md`
