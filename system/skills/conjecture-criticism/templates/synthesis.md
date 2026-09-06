# Synthesis -- Conjecture & Criticism

## Input

**Decision:** {{DECISION}}
**Tier:** {{TIER}}
**Agents:** {{AGENT_COUNT}}
**Perspectives:** {{PERSPECTIVES}}
**Rounds completed:** {{ROUNDS}}

## Agent Proposals

{{PROPOSALS}}

## Cross-Critique Results

{{CRITIQUES}}

## Debate Results (if deep tier)

{{DEBATES}}

## Synthesis Task

1. Tally scores from cross-critiques
2. Check agent final positions (did any concede?)
3. Identify the winner (highest aggregate + no disqualifying flaws + most concessions toward it)
4. Identify the runner-up
5. Extract killed conjectures and the specific criticism that eliminated them
6. Produce output in the format matching the tier

## Verdict-First Output

### Verdict
[Recommendation, stated plainly]

### Why This Wins
[Key reasons drawing from cross-critique results]

### Key Criticisms That Shaped This
[Strongest criticisms and how they influenced the verdict]

### Runner-Up
[Second-best option and why it lost]

## Scorecard (deep tier or on request)

| Option | {{PERSPECTIVE_1}} | {{PERSPECTIVE_2}} | {{PERSPECTIVE_3}} | Concessions | Verdict |
|--------|---|---|---|---|---|

## Killed Conjectures (deep tier)

### [Option]: [Name]
**Eliminated because:** [Specific criticism from cross-critique]
**Instinct candidate:** "When [trigger], avoid [approach] because [reason]"

## Learning Artifacts

```yaml
- id: [generated-from-decision]
  trigger: "[when pattern]"
  action: "[avoid/prefer pattern]"
  confidence: 0.5
  domain: "[domain]"
  source: "conjecture-criticism"
  evidence: "[date] [context]"
```
