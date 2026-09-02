# Solution proposal — required-section contract

The Phase 2 output, written by the frontier convergence seat. Persisted as
`<slug>-solution-YYYY-MM-DD.md`, next to the problem document, and linking to it by path.

---

```markdown
# [SOLUTION TITLE] — proposal

**Problem document:** [path]
**Designers:** [n] blind candidates · **Judges:** [n] independent

## Verdict

[Plain language. What to build and the one reason it wins, stated so it can be argued with.]

## How it works

[The winning design in enough detail to implement: the change, where it lands, the new
control flow, what state it touches. Cite the existing machinery it extends, using the
problem document's citations.]

## Grafts

[Ideas taken from candidates that did not win. One row each — this is where the panel
earns its cost.]

| Idea | From candidate | Why it was grafted in |
|---|---|---|

## Constraint satisfaction

[One row per constraint from the problem document's Hard constraints section. Every
constraint gets a row; none are dropped.]

| Constraint | How this design satisfies it |
|---|---|

## Rejected alternatives

[Every candidate that did not win, plus any direction the panel raised and killed. A
rejection with no specific reason is not a rejection.]

### [Candidate name] — rejected
- **Sketch:** [one or two sentences]
- **Rejected because:** [the specific failure — which constraint it violates, which
  scenario breaks it, what it costs. Not "less elegant".]

## Judge panel

| Candidate | Judge 1 | Judge 2 | Judge 3 | Notes |
|---|---|---|---|---|

**Disagreements:** [Where judges split, name the split explicitly: who preferred what and
on which criterion. Then state the tiebreak — which constraint from the problem document
decides it, and why. If the judges did not split, say so; unanimity is a result worth
recording.]

**Independent convergence:** [If two or more blind designers arrived at substantially the
same design, record it here. Independent agreement is evidence for the design and belongs
in the proposal, not in a footnote.]

## Risks and falsifiers

[What this design bets on. For each: what observation would prove the bet wrong. This is
what the implementer watches for.]

## Open questions for implementation

[Numbered. What the implementer still has to decide, and what is deliberately left open.]

## Handoff

[What the implementing team needs: files in scope, the verification that proves the fix
works, what must not regress. In hex, this is the material for a BOI spec's
`[contract].scope` — the spec references this document and the problem document by path
rather than restating them.]
```

---

## Notes

**The verdict is a recommendation, not a decision.** It goes to a human who can reject it.
Write it so rejecting it is easy: the reason it wins is stated in one line and can be
argued with.

**Every candidate gets a rejection entry**, including ones that were close. The rejected
alternatives section is the most-reread part of the document six months later, when
someone asks "why didn't we just...".

**Never average judge scores into a silent winner.** If the panel split, the split is
content.
