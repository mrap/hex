---
name: hex-ideate
description: >
  Structured ideation primitive for hex. Ingests evolution/observations.md,
  todo.md backlog, project contexts, and raw/captures/ — then thematically
  clusters them into candidate experiments mapped to the Experiment Velocity
  framework (test, don't select). Outputs ranked opportunity spaces with
  experiment hypothesis templates ready to dispatch. Use when you want to
  identify what to work on next from the accumulated signal in the system,
  rather than picking problems ad-hoc.
trigger: /hex-ideate
---

# hex-ideate — Systematic Ideation from System Signal

## When to use

Invoke `/hex-ideate` when:
- The active queue feels arbitrary or disconnected from goals
- There's accumulated signal in evolution/ or captures/ that hasn't been processed into experiments
- Weekly planning needs a structured input
- The Experiment Velocity framework needs new candidates

## Phase 1: Signal Ingestion

Read ALL of the following sources. Don't summarize yet — just collect the raw signal:

1. **`evolution/observations.md`** — every open/unresolved observation (friction points, patterns, opportunity gaps)
2. **`todo.md` → ## Now and ## Backlog** — items that are stale (>14 days without progress), blocked, or marked "REVISIT"
3. **`raw/captures/` → last 30 days** — everything captured (tweets, articles, raw ideas)
4. **`projects/*/context.md`** — project state for: hex, hex-ui, brand, job-search, boi, zwerk. Flag any project with no recent progress.
5. **`projects/brand/experiments.md`** — experiment verdicts and open hypotheses
6. **`me/learnings.md`** — recurring patterns about how Mike works, what he resists, what compounds for him

Collect items into a raw list: `[source] [item title] [age/last-touched] [category hint]`

---

## Phase 2: Thematic Clustering

Group the raw list into **opportunity spaces** — not tasks, but themes where multiple signals converge.

Each cluster gets:
- **Name:** one short phrase (not a task)
- **Signal count:** how many items from different sources point at this theme
- **Why it matters:** one sentence connecting it to an OKR or ongoing initiative
- **Tension:** what's the blocker or open question at the center of this theme

Aim for 4-8 clusters. Smaller is better — don't force every item into a cluster.

**Example clusters:**
- "Agent fleet activation gap" (observations + todo + Trevin CE signal all point at idle agents problem)
- "Brand distribution engine not built yet" (experiments ready, no execution started)
- "BOI reliability debt" (multiple failure modes documented, no systematic fix in flight)

---

## Phase 3: Map to Experiment Velocity Framework

For each cluster, apply the framework: **don't select problems, test them**.

The test of a cluster is: can I state a falsifiable hypothesis and a 5-day verdict criteria?

For each cluster, generate an **Experiment Candidate**:

```
## Cluster: [name]
Opportunity: [one sentence — what could change if this cluster is addressed]
Hypothesis: If [action], then [measurable outcome] within [timeframe]
Experiment format: [what to build / do / test]
Verdict criteria:
  - SCALE if: [specific measurable outcome]
  - KILL if: [specific failure signal]
Cost estimate: [low/medium/high — time + token spend]
Linkage: [which OKR or initiative does this drive?]
```

---

## Phase 4: Rank and Route

Score each experiment candidate on three axes (1-5):
- **Signal density** (how many sources converge on this?)
- **Velocity potential** (how fast can we get a verdict?)
- **Initiative alignment** (how directly does it drive a current KR?)

Sort descending by sum. Top 3 are **Active Candidates**.

For each Active Candidate:
- If it maps to an open BOI spec: point to the spec
- If it needs a new BOI spec: generate the spec inline (YAML format, mode: execute or generate)
- If it's a single edit: do it inline

---

## Phase 5: Output

Produce a structured ideation report:

```markdown
# hex-ideate — [DATE]

## Signal ingested
- [n] observations from evolution/
- [n] items from todo.md
- [n] captures from raw/
- [n] project context gaps

## Clusters identified
[list with signal counts]

## Top 3 Experiment Candidates

### 1. [Cluster name]
[full experiment candidate block]

### 2. [Cluster name]
[full experiment candidate block]

### 3. [Cluster name]
[full experiment candidate block]

## Routing
- [Candidate 1] → [dispatch to BOI / do inline / link to existing spec]
- [Candidate 2] → [...]
- [Candidate 3] → [...]

## Dormant clusters (save for later)
[list of lower-ranked clusters with brief rationale for deprioritizing]
```

Save report to `projects/ideation/hex-ideate-YYYY-MM-DD.md`.

---

## Constraints

- Do NOT pick winners. The framework says test, not select. Your job is to make experiments runnable, not to decide which one is right.
- If a cluster has no falsifiable hypothesis, it's an observation, not an experiment. Keep it in the dormant list.
- A cluster with >3 signals from different sources is a strong signal even if it doesn't feel urgent.
- OKR alignment is a secondary filter — don't reject high-signal clusters just because they're not on the current quarter's OKR list. Surface them as dormant.

---

## Related

- `projects/brand/experiments.md` — active brand distribution experiments
- `evolution/observations.md` — raw observation input
- `projects/boi/context.md` — BOI spec routing
- Experiment Velocity framework: "don't select problems, test them" (from `project_wealth_generation.md` memory)
