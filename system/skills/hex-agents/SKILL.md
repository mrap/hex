---
name: hex-agents
description: Create, operate, and manage autonomous hex agents. Use when deciding whether work needs an agent, creating new agents, waking agents, checking fleet health, or troubleshooting agent issues. Invoke BEFORE creating any agent or charter file.
---

# hex-agents — Agent Fleet Operations

Use this skill whenever you're about to create, modify, wake, or reason about autonomous agents. Agents are the most expensive and complex primitive in hex — use them deliberately.

## When to Use This Skill

Invoke when you hear:
- "create an agent for…" / "we need an agent that…"
- "should this be an agent?"
- "check on the agents" / "fleet health"
- "wake / halt / dissolve an agent"
- "why isn't agent X doing Y?"

## The Decision: Agent vs BOI vs hex-events vs Inline

Pick the simplest primitive that works. Never escalate when a lighter tool fits.

| Situation | Use | Why |
|-----------|-----|-----|
| Deterministic reaction to an event (no judgment) | **hex-events policy** | Policies are cheap, stateless, instant |
| One-shot multi-step work (3+ files, research, generation) | **BOI spec** | Workers are disposable, spec-driven |
| Single edit, quick lookup, < 2 min | **Inline** | Don't dispatch what you can do in 30 seconds |
| Persistent concern that compounds context over time | **Agent** | Agents have memory, queue, budget, initiative tracking |

### Agent-Worthy Signals (need 2+ to justify)

- The work recurs on a schedule and benefits from accumulated context
- It needs to track initiatives across multiple wakes
- It needs to send/receive messages to other agents
- It has its own success metrics (KPIs) that it measures and tunes against
- It requires judgment that improves with experience (not just rule-following)
- Dissolving it would lose valuable accumulated state

### NOT Agent-Worthy

- "Run this check every hour" → hex-events policy
- "Refactor these 8 files" → BOI spec
- "Research the top 5 options for X" → BOI spec
- "Monitor this URL for changes" → hex-events policy + shell action
- "Do this thing once" → inline or BOI

### The Test

If you can describe when the agent would be dissolved, it's probably project-scoped — spawn it with the factory and dissolve it when the project ships. If it serves a persistent operational concern that compounds indefinitely, it's a core agent.

## Core Agents

Agents can be marked `core: true` in their charter. Core agents are load-bearing for system operation — the system actively protects them:

- `hex-agent fleet` shows a `●` marker next to core agents
- `hex-agent fleet` warns if any core agent is HALTED
- `hex-agent list --core` returns only core agent IDs
- Doctor check_7 verifies all core agents are active — a halted core agent is an ERROR, not a warning
- The watchdog attempts auto-recovery for halted core agents

### When to mark an agent as core

Mark `core: true` when removing the agent would degrade the system's ability to operate, heal, or improve itself. Examples: an ops agent that runs health checks, a coordinator that drives workstreams, a fleet manager that monitors agent performance.

Do NOT mark project-scoped agents as core. If the agent is tied to a specific project or initiative, it's not core — it's spawnable and dissolvable.

### Reference Set and Drift Detection

Hex ships with reference charters for core agents at `.hex/reference/core-agents/`. The system detects drift:

```bash
hex-agent check-core             # Compare actual vs reference — shows missing/broken
hex-agent restore-core           # Restore missing core agents from reference
```

`restore-core` never overwrites existing charters — it only fills gaps. User modifications to core agent charters are preserved. User-created agents are never touched.

Doctor check_7 runs `check-core` automatically. If drift is detected:
- Missing core agents → ERROR with "run `hex-agent restore-core`"
- Broken core agents → ERROR with "run `hex-agent check-core` for details"

### Modifying core agents

Changing a core agent's charter is allowed but should be done carefully. The doctor and fleet commands will catch broken charters immediately. If you need to halt a core agent temporarily, the system will warn on every `fleet` and doctor run until it's restored.

## Registration Protocol

**Charter file IS registration.** One file, one path, no fallbacks.

```
projects/<agent-id>/charter.yaml  ← this file existing = agent is registered
```

The harness discovers agents by scanning `projects/*/charter.yaml` at runtime. There are no hardcoded agent lists anywhere. No secondary registration step. No manual config updates.

### Rules (non-negotiable)

1. **Directory name = charter id = agent id everywhere.** `charter.id` must match the directory name exactly. The harness validates this and exits non-zero on mismatch.
2. **One canonical path.** `projects/{agent-id}/charter.yaml`. No prefix fallbacks, no aliases, no symlinks.
3. **No hardcoded agent lists.** Shell scripts use `hex-agent list` to discover agents. Never write agent IDs into script variables.
4. **Loud errors, never quiet.** Every validation failure prints a specific error and exits non-zero. Silent fallbacks are bugs.

## Charter Schema

```yaml
# projects/<agent-id>/charter.yaml

id: agent-id                    # REQUIRED — MUST match directory name exactly
name: Human-Readable Name       # REQUIRED
role: one-line role description  # REQUIRED
scope: >                         # optional
  What this agent owns, reads, and writes.
objective: >                     # optional
  What success looks like.
kpis:                            # optional
  - "metric: target"

wake:
  triggers:
    - timer.tick.1h
  responsibilities:
    - name: health-check
      interval: 3600
      description: What this recurring task does

authority:
  green: []                     # can do autonomously
  yellow: []                    # do + notify
  red: []                       # ask first

budget:
  wakes_per_hour: 4
  usd_per_day: 5.00
  usd_per_shift: 1.00

kill_switch: "~/.hex-<agent-id>-HALT"
escalation_channel: "#cos"
core: false                     # true = system-critical, protected by doctor + fleet

# Optional fields:
# version, parent, memory.max_size_kb, hooks.on_find/on_decide/on_act/on_verify
```

### Validation (harness-enforced)

- `id` required, non-empty, must match directory name
- `budget.usd_per_day` must be positive
- `budget.usd_per_shift` must be positive
- `budget.wakes_per_hour` must be > 0
- `kill_switch` required

Invalid charters cause `hex-agent fleet` to exit non-zero with a specific error. No agent shows status until all charters validate.

## Creating a New Agent

### Step 1: Decide

Confirm at least 2 agent-worthy signals from the list above. If you can't name them, use BOI or hex-events instead.

### Step 2: Write the charter

Create `projects/<agent-id>/charter.yaml` following the schema above.

### Step 3: Wire the trigger policy

Create `~/.hex-events/policies/<agent-id>-agent.yaml`:

```yaml
name: <agent-id>-agent
description: Wake policy for <agent-id>
rules:
  - name: scheduled-wake
    trigger:
      event: timer.tick.1h
    actions:
      - type: shell
        command: >
          HEX_DIR=$HEX_DIR
          $HEX_DIR/.hex/bin/hex-agent wake <agent-id>
          --trigger timer.tick.1h
          --payload '{{ event | tojson }}'
```

### Step 4: Verify

```bash
hex-agent fleet              # agent appears, charter validates
hex-agent list               # agent ID in the list
hex-agent status <agent-id>  # shows state
```

### Step 5: Activate (if halted)

New agents are active by default — the HALT check only fires if the kill switch file exists. To start an agent in a halted state (recommended for safety), create the HALT file first:

```bash
touch ~/.hex-<agent-id>-HALT    # start halted (recommended)
rm ~/.hex-<agent-id>-HALT       # activate when ready
```

The factory script (`hex-agent-spawn.sh`) creates the HALT file automatically. Manually-created agents are active immediately unless you create the file yourself.

## Dissolving an Agent

```bash
rm -rf projects/<agent-id>/
rm -f  .hex/bin/<agent-id>-wake.sh
rm -f  ~/.hex-events/policies/<agent-id>-agent.yaml
rm -f  ~/.hex-<agent-id>-HALT
```

Verify: `hex-agent fleet` — agent should no longer appear.

## CLI Reference

```bash
hex-agent fleet                    # Fleet overview — all agents with status + core markers
hex-agent list                     # Agent IDs, one per line (for scripts)
hex-agent list --core              # Core agent IDs only
hex-agent status <agent-id>        # Single agent detail
hex-agent wake <id> --trigger <e>  # Run one shift
hex-agent check-core               # Compare fleet against reference core set
hex-agent restore-core             # Restore missing core agents (never overwrites)
hex-agent message <from> <to> \
  --subject "..." --body "..."     # Async inter-agent message
```

## Wake Lifecycle

1. **Charter load** — reads `projects/{id}/charter.yaml`, validates schema + id match
2. **HALT check** — if kill switch file exists, audit "halted", exit without touching state
3. **State load** — reads or initializes `projects/{id}/state.json`
4. **Inbox** — reads messages from `.hex/messages/{id}.jsonl`, clears inbox
5. **Queue promotions** — scheduled items promoted if due, blocked items unblocked if resolved
6. **Nothing actionable?** — empty active queue → audit "wake-skip", save, exit
7. **Shift loop** — invoke Claude with charter + state, apply structured response (trail, queue updates, memory, messages), repeat until queue drained or shift budget hit
8. **State save** — atomic write to `state.json`
9. **Audit** — `wake-complete` with invocation count, cost, trail size

## Anti-Patterns

| Anti-pattern | Why it's wrong | Do this instead |
|-------------|----------------|-----------------|
| Hardcoded agent ID lists in scripts | Drift, phantom agents | `hex-agent list` |
| Prefix fallback chains (`hex-{id}`, `{id}`) | Identity splits across state/audit/charter | One path: `projects/{id}/` |
| Silent `.ok()` / `2>/dev/null` on agent ops | Invisible failures | Log to stderr, exit non-zero |
| Charter `id` ≠ directory name | State and audit use different identities | Harness rejects this — fix the mismatch |
| Creating agents for one-shot work | Wasted budget, fleet bloat | BOI specs |
| Agents without KPIs | No way to measure if it earns its cost | Define outcomes in charter |
| Agent messages with unchecked sends | Lost messages look "sent" in audit | Check result, audit failures separately |

## Troubleshooting

| Symptom | Check |
|---------|-------|
| Agent not in `fleet` | Does `projects/{id}/charter.yaml` exist? |
| `fleet` exits non-zero | Error message names the broken charter |
| Agent never wakes | hex-events policy exists? Trigger event fires? Kill switch: `ls ~/.hex-{id}-HALT` |
| Agent wakes but does nothing | `hex-agent status {id}` — active queue empty? Scheduled items configured? |
| Cost growing fast | Check `usd_per_shift` in charter vs actual spend in status |
| Messages not delivered | Check `.hex/messages/{id}.jsonl` and stderr for SEND FAILED |
