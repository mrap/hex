---
name: deep-solve
description: >
  Use when a bug or system problem is hard, recurring, or high-stakes — the fix
  will be handed to someone else to implement, a previous fix did not hold, or
  getting it wrong is expensive. Also use when asked to "understand this problem
  before fixing it" or "figure out what's actually going on first", for
  post-incident engine or infrastructure fixes after repeat failures, when the
  request is shaped like "have a team understand X and then propose solutions",
  and on explicit /deep-solve. Not for a single-cause bug you can already
  reproduce and fix — use diagnose or superpowers:systematic-debugging there.
version: 1.0.0
---

# deep-solve

Two phases, in order: **understand the problem, then propose a solution.**

Phase 1 produces a written PROBLEM DOCUMENT. Phase 2 produces a written SOLUTION
PROPOSAL judged against it. Both persist to disk. Implementation is a separate handoff.

The failure this prevents: a team designing a fix for a problem nobody wrote down,
so the fix gets judged against a fuzzy memory instead of stated constraints.

## When not to use

- Single-cause bug you can reproduce and fix now → `diagnose`, `superpowers:systematic-debugging`.
- Choosing between known options with no mechanism left to establish → `conjecture-criticism`, `hex-decide`.

**Versus `conjecture-criticism`:** that skill works the solution space only — agents
propose approaches and cross-critique each other, nothing is persisted, there is no
evidence phase. deep-solve is problem-first: parallel evidence readers with citations,
adversarial verification of the causal mechanism, a persisted problem document, and
judges scoring candidates against constraints *derived from that document*. Reach for
`conjecture-criticism` when the problem is already understood and only the choice is
open. Phase 2 here borrows its blind-generation idea and adds a judge panel plus a
convergence write-up.

## Orchestration

- **Primary — Workflow tool**, when available: adapt `references/deep-solve-workflow.js`
  (complete, both phases, generic placeholders).
- **Fallback — Agent tool fan-out**: same seats, same models, same prompts
  (`references/seat-prompts.md`). Issue each fan-out as ONE message containing multiple
  Agent calls so they genuinely run in parallel, and pass each seat's returned text
  verbatim into the next seat.

## Seats

| Phase | Seat | Model | Count |
|---|---|---|---|
| 1 | Evidence readers (code path, forensics, contract, prior art) | sonnet | 4 |
| 1 | Synthesis / finalize author | **frontier (opus)** | 1 |
| 1 | Citation verifier | sonnet | 1 |
| 1 | Mechanism refuter | **frontier (opus)** | 1 |
| 1 | Completeness critic | sonnet | 1 |
| 2 | Solution designers (blind, one persona each) | sonnet | 3–4 |
| 2 | Judges | sonnet | 3 |
| 2 | Convergence author | **frontier (opus)** | 1 |

**Model discipline (hex standing order 3b).** Set `model` explicitly on every seat. An
unset model silently inherits the caller's frontier model — that is how fan-out burns
credit. Frontier judgment is bought at exactly three seats, for these reasons:

- **Synthesis / finalize** holds four disjoint evidence streams at once, has to notice
  where they contradict, and has to leave thin evidence marked thin. Weaker models
  average conflicting inputs into confident prose — the exact failure the document exists to prevent.
- **Mechanism refuter** must out-think the author who wrote the draft and find the code
  path they missed. A refuter weaker than the author confirms whatever it is shown.
- **Convergence** grafts ideas across incompatible designs and defends a ranking against
  stated constraints instead of picking the longest candidate. That judgment is what the
  whole exercise is buying.

Everything else — reading code and reporting citations, generating one design from one
stated persona, scoring against explicit criteria — is bounded, checkable work. Sonnet.

## Phase 1 — understand the problem

1. **Frame.** One paragraph: what is believed broken, the concrete incidents or symptoms,
   where the code and the evidence live. Every downstream seat inherits this verbatim.
   Where a suspected cause already exists — a backlog entry, someone's hypothesis, last
   week's theory — the frame names it as the claim under test, with its source. A frame
   that states the cause as settled fact is inherited as ground truth by all eight
   downstream seats, and the refuter has nothing left to attack.
2. **Evidence fan-out** — 4 parallel sonnet readers, distinct lenses: relevant code path
   traced end to end with file:line citations / empirical forensics from logs, DBs, and
   incident history / documented contract versus actual behavior / prior art and past
   fixes that touched this area. **Phase 1 is read-only** — readers edit no file in any
   git repo (read queries against databases are fine). This is what makes a wide fan-out
   into a live repo safe without worktrees.
3. **Synthesis** (frontier) writes the problem document to the required-section contract
   in `references/problem-document.md`. Its final content section is *Candidate fix
   directions*: 2–4 entries, each with sketch / what it reuses / main risk / open
   questions. No section ranks them and no entry is marked recommended.
4. **Verification** — 3 parallel verifiers, distinct lenses: citation accuracy (sonnet,
   checks every file:line against source), mechanism refuter (frontier, tries hard to
   disprove the causal claims), completeness critic (sonnet, what an implementation team
   would trip over).
5. **Finalize.** The same frontier seat that wrote the draft applies the corrections:
   fix wrong citations, state what is actually true wherever a claim was refuted, fold
   material gaps into the constraints and open-questions sections.
6. **Persist**, then surface to the user: the file path, the summary, and the open
   design questions. **Proceed straight into Phase 2** unless the user asked for a checkpoint.

## Phase 2 — converge on a solution

1. **Designers** — 3–4 parallel sonnet agents, each BLIND to the others. Each receives the
   finalized problem document and one design stance, and nothing else: minimal-diff /
   reuse-existing-machinery / first-principles redesign / operational-simplicity. A stance
   is a lens on the whole problem. Handing each designer a pre-chosen fix instead —
   "you take the retry fix, you take the salvage fix" — produces four advocates for four
   foregone conclusions and leaves the judges nothing to compare.
2. **Judges** — 3 independent sonnet agents. Each receives all candidates plus scoring
   criteria derived from the problem document's *Hard constraints* section — at minimum
   correctness and crash-safety, blast radius and simplicity, operability and loudness.
   Each judge scores every candidate and names the strongest and weakest.
3. **Convergence** (frontier) picks the winner, grafts the best ideas from the runners-up,
   records every rejected alternative WITH its reason, and writes the solution proposal to
   `references/solution-proposal.md`.

## Where the documents go

Two files, not one — a problem document that ends without a winner and a proposal that
picks one cannot be the same file. They land side by side, in the workspace that owns the
project: a hex instance keeps them under its `projects/` design folder; a code repo that
keeps design docs uses `docs/design/`. When the investigated repo is not the workspace you
are operating in, write to the operating workspace — writing into the investigated repo is
a repo write that needs a worktree (SO #7), and Phase 1 is read-only there anyway. Name
them `<slug>-problem-YYYY-MM-DD.md` and `<slug>-solution-YYYY-MM-DD.md`, using `date +%F`
for the date rather than assuming it.

## Failure modes

| Situation | What to do |
|---|---|
| Refuter REFUTES a core causal claim | Loop Phase 1 once: re-run the evidence reader whose lens covers the refutation, re-synthesize with the refutation as input. Once only — a second refutation goes to the user, not a third loop. |
| Designers converge on the same solution | Signal, not failure. Independent agreement is evidence for the design — the convergence author records it as such and still writes down what was not chosen. |
| Judges split | The convergence author names the disagreement explicitly and states the tiebreak reason: which constraint from the problem document breaks it. Never average scores into a silent winner. |
| An evidence lens comes back empty | Say so in the document's evidence appendix. An absent lens is a known gap, not a passed check. |

## Handoff

The skill ends at the converged proposal. Implementation ships separately — in hex, a BOI
spec whose `[contract].scope` references both documents by path.
