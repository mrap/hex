# Evolution Engine

The evolution engine is hex's self-improvement loop. It turns repeated friction into permanent fixes.

## Cycle

```
Observe → Log → Propose → Verify
```

1. **Observe** — Notice patterns during work. What keeps going wrong? What questions get asked repeatedly? Where does the agent waste time?

2. **Log** — Record the friction in a retrospective. Be specific: what happened, what the impact was, how often it occurs.

3. **Propose** — Draft a standing order, convention note, or workflow change that would prevent the friction. Save it to memory.

4. **Verify** — In subsequent sessions, check whether the change actually helped. If it didn't, revise or remove it.

## When to run a retrospective

- After completing a significant feature or fix
- When you notice the same mistake happening twice
- At natural stopping points (end of day, end of sprint)
- When a session felt particularly inefficient

## Retrospective Template

```markdown
# Retrospective — YYYY-MM-DD

## What went well
- (things that worked, were fast, or felt smooth)

## What caused friction
- (repeated questions, wrong assumptions, slow discovery)

## Patterns noticed
- (recurring themes across sessions)

## Proposed changes
- [ ] (new standing order, convention, or workflow adjustment)
- [ ] (memory to save for future reference)

## Verification plan
- (how to check if the changes helped in the next 2-3 sessions)
```

## Examples of evolution

**Friction:** Agent keeps forgetting the project uses pnpm instead of npm.
**Evolution:** Save to memory: "This project uses pnpm. Do not use npm commands." → Agent searches memory on package management questions → never makes this mistake again.

**Friction:** Agent writes tests in a different style than the existing test suite.
**Evolution:** Save convention to memory: "Tests use describe/it blocks with jest, arrange-act-assert pattern, factories in test/factories/." → Agent matches existing style automatically.

**Friction:** Deploy process has non-obvious steps.
**Evolution:** Save step-by-step deploy process to memory with the "deploy" tag → Agent can guide deploys accurately without re-discovery.
