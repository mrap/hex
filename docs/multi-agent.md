# Multi-Agent System

hex runs a fleet of autonomous agents. Each agent has a charter (what it does), state (what it's working on), and wakes on events to do its work. The **hex-agent harness** (a compiled Rust binary at `.hex/bin/hex-agent`) is the single entry point for all agent operations. Agents cannot bypass it.

## Registration

**Charter file IS registration.** Creating `projects/<agent-id>/charter.yaml` registers an agent. The harness discovers agents by scanning `projects/*/charter.yaml` at runtime. There are no hardcoded agent lists anywhere — no secondary registration step, no manual config updates.

Rules:
- Directory name must match `charter.id` exactly — the harness validates and rejects mismatches
- One canonical path: `projects/{agent-id}/charter.yaml` — no prefix fallbacks or aliases
- Shell scripts use `hex-agent list` to discover agents — never hardcoded IDs

## How It Works

Agents are event-driven. hex-events fires a trigger (timer tick, BOI completion, attention request). The trigger invokes `hex-agent wake <agent-id>`. The harness:

1. Loads the agent's charter and validates `charter.id` matches the directory name
2. Checks HALT file (kill switch) — if halted, audit and exit without touching state
3. Loads or initializes persistent state (`projects/<agent-id>/state.json`)
4. **Auto-promotes charter responsibilities** — any `wake.responsibilities` not yet in the scheduled queue are seeded with `next_due = now`, making them immediately actionable
5. Promotes due scheduled items and unblocked items to the active queue
6. Reads inbox (async messages from other agents)
7. Invokes Claude with the charter + state as context
8. Parses the agent's structured response
9. Validates every trail entry through gates (required fields per action type)
10. Persists state, delivers outbound messages, records cost
11. Loops until the active queue is drained or shift budget is hit

## Charter Auto-Promotion

Agents derive work from their charter — no manual state.json seeding required.

When an agent wakes, the harness compares `wake.responsibilities` in the charter against the scheduled queue. Any responsibility without a matching `s-<name>` entry is added immediately as a due scheduled item. On the first wake ever (fresh install, no state.json), all responsibilities seed and promote to active in the same wake — the agent is productive from day one.

**Rules:**
- First wake with a charter → all responsibilities seed, all due now, all promote to active
- Subsequent wakes → `promote_scheduled` manages timing (interval elapsed = promote)
- New responsibility added to charter → detected on next wake, auto-seeded
- Responsibility removed from charter → orphaned scheduled entry stays; harness re-runs it, agent ignores it (harmless)
- No state.json seeding scripts or manual queue population needed

The scheduled item ID is `s-<responsibility.name>`. Interval comes from `responsibility.interval` (seconds).

## Core Agents

Agents marked `core: true` in their charter are load-bearing for system operation. The system actively protects them:

- `hex-agent fleet` shows a `●` marker and warns if any core agent is HALTED
- `hex-agent check-core` compares the actual fleet against reference charters in `.hex/reference/core-agents/`
- `hex-agent restore-core` restores missing core agents from reference — never overwrites existing charters or user agents
- Doctor check_7 detects core agent drift and surfaces it as an ERROR

Reference charters ship with hex at `.hex/reference/core-agents/`. When the user's instance diverges (missing or broken core agent), the system detects it and offers `hex-agent restore-core` as a one-command fix.

## State Model

Each agent's state is a JSON file owned by the harness. Agents return structured output; the harness validates and persists. Key fields:

- **queue** -- three bins: `active` (work now), `blocked` (waiting on something), `scheduled` (recurring responsibilities)
- **trail** -- append-only log of everything the agent did, with gate-validated action types
- **memory** -- freeform JSON the agent uses to compound insights across wakes
- **inbox** -- async messages from other agents
- **cost** -- per-wake, per-period, lifetime USD tracking

## Budget & Cost

Every charter has a `budget` block with three fields:

```yaml
budget:
  wakes_per_hour: 12
  usd_per_day: 0      # 0 = unlimited
  usd_per_shift: 0    # 0 = unlimited
```

**How it works:**
- `0` = unlimited. No enforcement. The agent runs until its queue is drained.
- Any positive value = enforced cap. When cumulative cost within a single wake exceeds `usd_per_shift`, the shift loop breaks with a loud warning + audit entry.
- `usd_per_day`: tracked in state but not currently enforced as a hard stop. Serves as a baseline for cost reporting.
- `wakes_per_hour`: rate limit enforced by the hex-events policy, not the harness itself.
- Negative values are rejected at charter load.

**Cost tracking is always on**, regardless of budget settings. Every Claude invocation records tokens, USD, and duration to `.hex/cost/ledger.jsonl`. Use this for cost analysis without needing to set restrictive budgets.

**To uncap an agent:** set `usd_per_day` and `usd_per_shift` to `0`.
**To cap an agent:** set positive dollar values — the harness will stop the shift loop and log when the cap is hit.

## Action Types (Gates)

Every agent action must pass a gate -- the harness validates required fields before recording it:

| Type | Required fields | When to use |
|------|----------------|-------------|
| `observe` | what, noted | Read something, gathered data |
| `find` | finding, evidence | Formed a conclusion from observations |
| `decide` | decision, alternatives, reasoning | Chose a course of action |
| `act` | action, result | Executed something |
| `verify` | check, evidence, status | Checked if something worked |
| `delegate` | initiative_id, to, context | Transferred ownership of work |
| `park` | item_id, reason, resume_condition | Set something aside to return to later |
| `reframe` | abandoned, reason, new_framing | Threw out approach, started fresh |
| `message_sent` | to, subject, body | Sent an inter-agent message |
| `sync_started` | with, context | Initiated real-time agent collaboration |

Gates are types, not a sequence. Agents choose their own workflow. The harness guarantees the audit trail is complete.

## Inter-Agent Communication

- **message** -- async inbox. Agent sends a message, it lands in the recipient's inbox on their next wake. For FYI or requests with tracked responses.

## CLI

```bash
hex-agent fleet                                   # Fleet overview (core markers, status)
hex-agent list                                    # Agent IDs, one per line
hex-agent list --core                             # Core agent IDs only
hex-agent status <agent-id>                       # Single agent detail
hex-agent wake <agent-id> --trigger <event>       # Run agent shift
hex-agent check-core                              # Compare fleet against reference set
hex-agent restore-core                            # Restore missing core agents
hex-agent message <from> <to> --subject "..." --body "..."  # Async message
```

## Creating New Agents

1. Write `projects/<agent-id>/charter.yaml` (see any existing charter or `.hex/skills/hex-agents/SKILL.md` for schema)
2. Create `~/.hex-events/policies/<agent-id>-agent.yaml` with wake triggers
3. Verify: `hex-agent fleet` — agent appears, charter validates
4. Activate: `rm ~/.hex-<agent-id>-HALT` (agents start halted by default)

For the full decision framework (when to use agents vs BOI vs hex-events), see the hex-agents skill.

## Key Design Decisions

- **Charter-driven discovery.** No hardcoded lists. Charter exists → agent is registered.
- **Charter-driven initiative.** Agents derive work from `wake.responsibilities` — no manual state.json seeding. First wake = productive agent.
- **Agents don't write their own state.** The harness owns state.json. Agents return structured output; the harness validates and persists.
- **Loud errors, never quiet.** Every failure prints a specific error and exits non-zero. Audit and cost writes log to stderr on failure.
- **Shift model.** Agents work until their active queue is empty or budget is hit (if set). They don't choose when to stop. Budget of 0 = unlimited.
- **Core agent protection.** Core agents are detected, monitored, and restorable. The system knows its own critical path.
- **Cost tracking is automatic.** Every Claude invocation records tokens and USD to `.hex/cost/ledger.jsonl`.

## Testing

E2E tests run in Docker to isolate from real fleet state:

```bash
docker build -f tests/agent-harness/Dockerfile -t hex-agent-e2e .
docker run --rm hex-agent-e2e
```

## Source

- Rust harness: `.hex/harness/` (11 modules)
- Compiled binary: `.hex/bin/hex-agent`
- Agent skill: `.hex/skills/hex-agents/SKILL.md`
- Reference core charters: `.hex/reference/core-agents/`
