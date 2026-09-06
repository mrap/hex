---
name: conjecture-criticism
description: >
  Adversarial analysis via conjecture and criticism. Spawns parallel agents to
  independently generate competing approaches, then each agent critiques all
  others. Consensus emerges from cross-criticism. Use when making recommendations
  with real alternatives, evaluating options, or when the user says "analyze",
  "evaluate", "criticize", "compare options", "what am I missing", or "poke holes".
version: 1.0.0
---

# Conjecture & Criticism

## Philosophy

Knowledge is created through conjecture and criticism, not authority or induction
(David Deutsch, The Beginning of Infinity). Every recommendation is a conjecture.
Every conjecture deserves genuine criticism from independent perspectives. The
strongest idea is the one that survives.

## When This Activates

### Auto-trigger (baked-in via the standing order on plan, conjecture, critique)
- The agent is about to recommend one option over alternatives
- The domain has real consequences (architecture, strategy, process, tool selection)
- Multiple viable approaches exist

### Manual trigger
- User invokes `/analyze`, `/evaluate`, or `/criticize`
- User asks "what am I missing", "poke holes", "compare options"

### Skip (do not trigger)
- Tactical execution with a single clear path
- Questions with factual answers
- Tasks where the user has already decided and wants execution

## Depth Tiers

| Tier | When | Agents | Rounds | Output |
|------|------|--------|--------|--------|
| Quick | Low stakes, tactical | 0 (inline self-check) | 1 | Verdict with brief "considered X but Y" |
| Moderate | Multiple viable alternatives, real trade-offs | 3 | 2 (generate + all-to-all critique) | Verdict-first + key criticisms |
| Deep | Architecture, strategy, process, or significant disagreement | 4+ | 3+ (generate + all-to-all + targeted debate) | Verdict + scorecard + killed conjectures |

**Escalation rule:** If moderate-tier agents disagree significantly after
cross-critique (no convergence on a winner), deep tier would normally apply.
Stop at moderate tier and present the disagreement to the user. Deep tier is
not available (see Phase 5).

**Calibration over time:** If depth-tier selection is consistently wrong —
moderate escalates to deep repeatedly, or a quick verdict gets revisited —
record it in `$HEX_DIR/evolution/observations.md` per the Improvement Engine (AGENTS.md).

## Protocol

### Phase 1: Frame the Problem

1. State the decision to be made
2. Identify the domain (tool selection, architecture, strategy, process, content)
3. Select the depth tier
4. Announce: "Running conjecture-criticism at [tier] depth."

### Phase 2: Assign Perspectives

Analyze the decision domain and assign context-adaptive perspectives. Rules:
- Perspectives must be genuinely distinct (not three ways of saying "is it good")
- At least one perspective must be adversarial to the leading conjecture
- At least one perspective must consider what everyone else is likely to miss
- State assigned perspectives upfront so the user can see how the problem is framed

**Example perspective sets by domain:**

| Domain | Perspectives |
|--------|-------------|
| Tool/library selection | Simplicity, Ecosystem maturity, Migration cost |
| Architecture | Scalability, Operational complexity, Time-to-ship |
| Strategy | Risk exposure, Opportunity cost, Second-order effects |
| Process design | Adoption friction, Failure modes, Maintenance burden |
| Content/brand | Audience resonance, Differentiation, Sustainability |

These are examples, not fixed. Derive perspectives from the actual decision.

### Phase 3: Generate (Parallel Agents)

Spawn N parallel sub-agents using the Agent tool. Each agent receives:
- The decision context
- Their assigned perspective
- Instructions to independently generate their best approach

Use the agent-prompt template. All agents run in parallel with no shared context.
This ensures genuinely independent thinking.

**Critical:** Each agent generates from their perspective AND provides a clear
thesis for why their approach is best. They have skin in the game.

### Phase 4: Cross-Critique (Parallel Agents)

Each agent receives ALL other agents' proposals and critiques them.

- At moderate tier (3 agents): 3 agents each critique 2 proposals = 6 critique
  passes, all running in parallel. Wall-clock time = 1 round.
- At deep tier (4+ agents): same pattern, more passes.

Use the cross-critique template. Each agent:
1. Scores every other proposal from their perspective (1-5)
2. Identifies the strongest competitor and why
3. Identifies the weakest competitor and the specific flaw
4. States whether they still believe their own proposal is best, or concede

### Phase 5: Debate (Deep Tier Only)

> **Status: Deferred.** The debate-round template is not yet implemented. Deep tier auto-escalation is disabled until this is shipped. Use moderate tier maximum.

After all-to-all critique, identify points of disagreement. Spawn targeted
debate rounds ONLY between agents that disagreed.

- No wasted rounds on settled points
- Each debate agent receives their own assessment, the opposing assessment,
  and the specific point of contention
- Use the debate-round template

**Convergence rule:** If all agents agree on a winner after any round, stop.
**Deadlock rule:** If no convergence after 3 rounds, present the deadlock
to the user with each position's strongest argument. Let the user decide.

### Phase 6: Synthesize

The main agent collects all outputs and produces:

**Verdict-first output (default):**
```
## Verdict
[Recommendation, stated plainly]

## Why This Wins
[Key reasons, drawing from cross-critique results]

## Key Criticisms That Shaped This
[The strongest criticisms and how they influenced the verdict]

## Runner-Up
[Second-best option and why it lost]
```

**Scorecard (on request or for deep tier):**
```
| Option | [Perspective 1] | [Perspective 2] | [Perspective 3] | Aggregate | Verdict |
|--------|---|---|---|---|---|
```

**Killed conjectures (deep tier):**
```
## Killed Conjectures
### [Option]: [Name]
**Eliminated because:** [Specific criticism from cross-critique]
**Lesson:** [Reusable insight]
```

### Phase 7: Capture Recurring Patterns

Killed conjectures and the criticism that eliminated them already appear in the
Phase 6 synthesis output. If the same killed approach or criticism pattern
recurs across 3+ decisions, record it in `$HEX_DIR/evolution/observations.md` —
the same Improvement Engine path every other repeated pattern uses (see
AGENTS.md). This skill has no separate learning system.
