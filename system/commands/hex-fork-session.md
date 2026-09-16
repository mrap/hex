---
name: hex-fork-session
description: Fork the current thread into its own hex session WITH its context already loaded. Use when the operator says "fork this into its own session", "spin this off", "give this its own session", "fork this fork". The new session boots with a handoff injected at SessionStart and starts its first task; nothing for the operator to re-explain. Pair of hex-new-session (which starts a blank session).
---

# hex-fork-session

Principle (operator correction, 2026-09-16): "When I ask you to fork into another session, I expect it to start up with the context already. The handoff has to be done for us automatically." No questions, no "should I write a handoff?" Do all of this in one turn.

## Procedure

1. Pick a kebab-case name from the topic (or use the one the operator gave).
2. Write the handoff from the current thread. Template (under ~60 lines, paths not bodies):

```
# Handoff: <name> (forked from <this session> on <date>)
## Why this session exists
<2-3 sentences: the ask, in the operator's words where possible>
## Read first, in order
<files with one-line why each>
## State
<done / running / pending, with ids and paths>
## Work queue
1. <first concrete task, small enough to start now>
2. ...
## Rules that bite here
<the 2-4 standing orders that matter for this work>
## Decisions already made (do not reopen)
<bullets>
```

3. Pipe it to the launcher (use a heredoc delimiter other than EOF if your shell block already uses EOF):

```bash
cat <<'HANDOFF' | "$HEX_DIR"/.hex/scripts/hex-fork-session <name> --purpose "<one line for the fleet table>" --kickoff "<work queue item 1, verbatim, as an instruction>" [--model sonnet] [--effort medium]
<handoff markdown>
HANDOFF
```

What the launcher does: stages the handoff at `.hex/run/handoffs/<name>.md`; starts the detached tmux session; runs `hex-new <name> --fresh`; the SessionStart hook (`"$HEX_DIR"/.hex/scripts/hex-handoff-inject`, declared in `system/hooks/required-hooks.json`) prints the handoff into the new session's context and archives it to `projects/hex-ops/handoffs/<name>-<stamp>.md`; verifies the Claude Code banner; sends the `--kickoff` prompt (pass work-queue item 1 verbatim so the session starts on the task instead of re-deriving context; default is the generic "Start on the first item in the handoff work queue."); registers the session in `projects/hex-ops/sessions.md`. Non-zero exit with the pane contents if anything fails.

4. Report one line: name + `tmux attach -t <name>`. Then continue the original thread here.

## Model choice
Default Opus (same as hex-new). `--model sonnet` for mechanical or capture lanes (SO 3b spirit); `--model haiku --effort low` for trivial lanes.

## Wiring (once per instance)
- `settings.json` SessionStart command must include `; "$HEX_DIR"/.hex/scripts/hex-handoff-inject` (see `system/hooks/required-hooks.json`).
- The session name reaches the hook via `HEX_SESSION_NAME` (export it in your `hex-new` launcher) or, failing that, the tmux session name.
- Launcher command is `hex-new @name@ --fresh` by default; override with `HEX_FORK_LAUNCH` or `--launch`.

## Notes
- Name collision: the launcher refuses (exit 3); report it, never kill the existing session.
- Tests: `python3 -I -B tests/test_fork_session.py` (fake tmux; stage, launch, inject, archive, kickoff, register).
