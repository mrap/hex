---
name: hex-fork-session
description: Fork the current thread into its own hex session WITH its context already loaded. Use when the operator says "fork this into its own session", "spin this off", "give this its own session", "fork this fork". The new session boots with a handoff injected at SessionStart and starts its first task; nothing for the operator to re-explain. Pair of `hex-new <name>` in a tmux session (which starts a blank session).
---

# hex-fork-session

Principle: when the operator asks to fork the current thread into another session, that session must start up with the context already loaded. The handoff is written and staged automatically, never asked about. No questions, no "should I write a handoff?" Do all of this in one turn.

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
cat <<'HANDOFF' | "$HEX_DIR"/.hex/scripts/hex-fork-session <name> --purpose "<one line for the sessions registry>" --kickoff "<work queue item 1, verbatim, as an instruction>"
<handoff markdown>
HANDOFF
```

Add `--model sonnet` for mechanical or capture lanes, or `--model haiku --effort low` for trivial ones; add `--launch CMD` to override `HEX_FORK_LAUNCH` for this one call.

What the launcher does: stages the handoff at `$HEX_DIR/.hex/run/handoffs/<name>.md`; starts the detached tmux session; runs the launcher command (see Wiring below); the SessionStart hook (`"$HEX_DIR"/.hex/scripts/hex-handoff-inject`, declared in `system/hooks/required-hooks.json`) prints the handoff into the new session's context and archives it to `$HEX_DIR/projects/hex-ops/handoffs/<name>-<stamp>.md`; verifies the Claude Code banner; sends the `--kickoff` prompt (pass work-queue item 1 verbatim so the session starts on the task instead of re-deriving context; default is the generic "Start on the first item in the handoff work queue."); registers the session in `$HEX_DIR/projects/hex-ops/sessions.md` if that registry exists. Non-zero exit with the pane contents if anything fails.

4. Report one line: name + `tmux attach -t <name>`. Then continue the original thread here.

## Model choice
Default Opus (same as `hex-new`). Mechanical or capture lanes run on a mid-tier model: pass `--model sonnet`. Trivial lanes: `--model haiku --effort low`.

## Wiring (once per instance)
- Wiring comes from `system/hooks/required-hooks.json` (foundation). `install.sh` and `hex upgrade` merge it into `$HEX_DIR/.claude/settings.json`. Do not hand-edit `settings.json`.
- The hook resolves the session name from `HEX_SESSION_NAME` when set, otherwise from the tmux session name. A custom `HEX_FORK_LAUNCH` must keep the runtime inside the tmux session the launcher created, or the hook cannot resolve the name.
- Launcher command is `hex-new @name@` by default; override with `HEX_FORK_LAUNCH` or `--launch`.
- Trust model: a staged handoff is operator-written input; the only writer of the staging directory is this launcher (`hex-fork-session`).

## Exit codes

| Code | Meaning | Recovery |
|---|---|---|
| 0 | Ok. Session up, briefed, and registered (if a registry exists). | None. |
| 2 | Usage error, or the handoff piped on stdin was empty. | Fix the invocation and retry. |
| 3 | A session with this exact name already exists. | Pick another name. |
| 4 | No Claude Code banner seen within the timeout. The session is killed and the handoff is archived at the printed path. | Read the pane, fix `HEX_FORK_LAUNCH`, then re-fork from the archived file. |
| 5 | The kickoff was typed but not submitted. The session is left up and briefed. | Run `tmux send-keys -t <name> Enter` and re-check. |
| 6 | The handoff was never consumed by the SessionStart hook. The session is killed and the handoff is archived at the printed path. | Run `hex upgrade`, then `hex-fork-session <name> < <archived path>`. |
| 7 | A handoff is already staged for that name, at the printed path. | Consume or remove it first. |

## Notes
- Name collision: the launcher refuses (exit 3); report it, never kill the existing session.
- Tests: `python3 -I -B tests/test_fork_session.py` (fake tmux; stage, launch, inject, archive, kickoff, register).
