# Problem document — required-section contract

The Phase 1 output. Persisted as `<slug>-problem-YYYY-MM-DD.md`.

These nine sections appear in this order, with these headings. A section with nothing
in it says so ("no prior fixes touched this area") — an empty heading is a finding, a
missing heading is a defect.

Every mechanism claim carries a `file:line` citation from the evidence readers. Where
the readers disagreed, or the evidence is thin, the document says so in place rather
than smoothing it over.

---

```markdown
# [PROBLEM TITLE — the symptom, not the suspected cause]

## Summary

[Plain language, short enough to paste into a message as-is. What breaks, how often, what it costs. A reader who
stops here knows whether to care.]

## Symptom & blast radius

[Incident evidence. One row per occurrence.]

| Incident | Date | Where | Cost | Outcome | Current state |
|---|---|---|---|---|---|
| [id] | [date] | [workspace/component] | [wall-clock, tokens, operator time] | [what happened] | [recovered / still stranded] |

[Then: what is affected beyond these incidents — the population at risk, not just the
observed sample.]

## Mechanism

[The code-cited walkthrough: entry point → the step where behavior diverges from intent
→ the terminal state. Every step cites `path/to/file.rs:120` with a short verbatim
excerpt. Name precisely what the failing path receives as input and why that input
cannot change on its own. If a healthy sibling path exists that handles the same case
correctly, contrast the two — the divergence is usually the finding.]

## Why existing fixes-to-date didn't cover it

[Preempt "wasn't this already fixed?". Name each prior fix, campaign, or release that
plausibly covered this area, and state precisely what it did and did not reach, with
citations.]

## Existing machinery a fix can build on

[Every mechanism already in the codebase that a fix would reuse or extend, with
citations. This is what keeps Phase 2 from inventing what already exists.]

## Hard constraints any fix must respect

[Derived strictly from what the readers found, not from taste. State-machine guards,
crash-recovery predicates, idempotence requirements, resource lifecycles, loudness
doctrine, budget semantics. Phase 2's judges score against this section, so a
constraint stated vaguely here becomes a criterion scored vaguely there.]

## Candidate fix directions

[2–4 entries. Each entry has exactly these four fields:]

### [Direction name]
- **Sketch:** [what it does, 2–3 sentences]
- **Reuses:** [which existing machinery, cited]
- **Main risk:** [the thing most likely to go wrong]
- **Open questions:** [what is unknown]

[No section ranks these entries. No entry is marked recommended, preferred, or leading.
Ordering is arbitrary and the document says so.]

## Open design questions

[Numbered. What the implementation team has to decide that this investigation could not.]

## Evidence appendix

[Forensic tables, key code excerpts, query outputs, and the raw findings any claim above
rests on. Also: which evidence lenses came back empty, and what that leaves unverified.]
```

---

## Section notes

**Summary stays short enough to paste into a message as-is** — a ceiling to respect, not a target to hit.

**Mechanism is the load-bearing section.** It is the one the frontier refuter attacks. If
it cannot be written with citations, the evidence phase is not finished — go back and run
another reader rather than writing a plausible-sounding narrative.

**Hard constraints feed Phase 2 directly.** Judges score candidates against this section.
Write each constraint so that "does this design satisfy it?" has a checkable answer.

**Candidate fix directions is the final content section.** Its job is to show the
solution space is non-empty and to hand Phase 2 a starting map. Its job is not to choose.
