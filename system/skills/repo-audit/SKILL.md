---
name: repo-audit
description: >
  Use when asked to audit a repository, assess codebase health, find what's
  wrong with a codebase, identify technical debt, or produce a prioritized
  improvement plan for an existing project. Also use when a repo feels
  untrustworthy — red CI, stale docs, mystery failures — and the user wants a
  grounded picture before investing, or says "upgrade this project", "what
  should we fix here", "how bad is this code".
tags: audit, code-quality, repo, technical-debt, improvement-plan, verification
---
<!-- # sync-safe -->

# Repo Audit

## Overview

Audit a repository as a principal engineer whose reputation depends on every claim being true. The output plans real engineering work, so **a false finding costs more than a missed one** — every claim is grounded in files you actually read, attacked before it's reported, and rated against the project's own quality bar.

Read-only: never modify code during an audit. Running builds, tests, linters, and dependency-audit tools is allowed and encouraged.

## When to use

- "Audit this repo" / "how healthy is this codebase" / "what should we improve"
- Inheriting or returning to a codebase and deciding where to invest
- Before a refactor, to build the evidence-backed backlog

**When NOT to use:** reviewing a single PR or diff (use a code-review skill), debugging one known bug, or security-only assessments (run a dedicated security review).

## Quick reference

| Phase | Effort | Output |
|---|---|---|
| 0 Calibrate | 10% | Repo Map + quality-bar statement |
| 1 Hunt | 40% | ≤25 candidate findings, each FACT or JUDGMENT |
| 2 Attack | 15% | CONFIRMED / DOWNGRADED / RETRACTED + attrition stats |
| 3 Strategy | 15% | 2–4 themes, do-NOT-fix list, done signals |
| 4 Plan | 20% | Milestones M0–M3, quick wins, top-3 sketches |

## The process

If no target repo was specified, ask which one. Work the phases in order.

### Phase 0 — Calibrate

Before judging anything:
1. **What this is:** purpose, users, maturity (prototype / personal tool / internal service / production / library). Read README, docs, manifests, CI config, and `git log --oneline -30` to see where active development happens.
2. **What "good" means here:** write down, in 3 sentences, the quality bar this project should be held to. A weekend prototype, a personal automation tool, and a production service have different bars. Every later severity rating is calibrated against THIS bar, not an abstract enterprise standard.
3. **Where the core is:** the ~20% of code doing 80% of the work (entry points, hot paths, most-touched files: `git log --format= --name-only -200 | sort | uniq -c | sort -rn | head -20`). Depth goes there; the periphery gets a lighter pass, and you say so.
4. **Process state, not just code:** local-vs-origin divergence (`git status`, `git log --oneline @{u}..HEAD` and `HEAD..@{u}`), recent CI results if accessible, whether any deployed artifact or downstream consumer pins a stale version. Live operational drift outranks latent code smells; drift discovered outside the repo (a consumer, a deploy) belongs in the report, explicitly flagged as cross-repo.
5. **House conventions:** naming, error-handling idiom, module layout, test style — recommendations must fit the existing culture.

Output: a "Repo Map" — purpose, stack, quality-bar statement, architecture sketch, core-vs-periphery split, conventions, surprises.

### Phase 1 — Hunt

Audit the dimensions below. With a subagent/Task tool, run dimensions as parallel subagents, each receiving the Repo Map; otherwise sequentially. Spend effort proportional to risk for THIS project — skip or compress dimensions that don't apply.

- **Correctness & error handling:** swallowed errors, unchecked results, race conditions, partial-failure states, resource leaks, missing edge cases on hot paths.
- **Architecture:** boundary violations, god modules, circular deps, leaky or unused abstractions, scalability cliffs.
- **Security:** secrets in code or history, injection, unsafe deserialization, permissions, auth gaps, dependency CVEs (run the ecosystem's tool if installed: `npm audit` / `cargo audit` / `pip-audit` / `govulncheck`).
- **Tests:** core behavior with NO test, tests that assert nothing, tests coupled to internals, missing failure-path tests.
- **Performance:** only where it matters — hot paths, unbounded growth (queues, files, memory), blocking calls in async contexts.
- **Operability & DevEx:** can a newcomer build and run it from the README alone? CI gaps, lint enforcement, logging quality, silent failure modes.
- **Docs & drift:** docs that contradict code (cite both sides), dead instructions, undocumented critical behavior.

Hard rules:
- Every finding: what, where (file:line), concrete consequence ("if X happens, Y breaks" — not "violates best practice"), severity (Critical/High/Medium/Low) calibrated to the Phase 0 bar.
- Cite line numbers only from a Read you actually performed; re-check after any re-read. Never invent a file:line.
- Label each finding FACT (verified by reading the code) or JUDGMENT (design opinion).
- Subagents return ONLY structured findings — `severity | file:line | what | concrete consequence` — max 15 per dimension, so results merge cleanly.
- Cap: 25 candidate findings. Prefer 12 load-bearing over 25 that pad.
- Record strengths too — what must be preserved.

### Phase 2 — Attack your own findings

This separates a useful audit from a plausible-sounding one. For every Critical and High finding (and any finding a task will be built on):
1. Re-open the cited file and try to REFUTE it. A guard you missed? "Dead code" called via reflection/config/CLI? "Missing test" covered by an integration test elsewhere? A "race" that can't fire given how the code is invoked?
2. Verify empirically where possible: run the build, the test suite, the linter; grep for callers. Cite the command and its result.
3. Verdict: CONFIRMED (say what you checked) / DOWNGRADED (real but less severe — explain) / RETRACTED (count; exclude from the report body).
4. Tag findings flagged independently by more than one dimension pass — independent confirmation is a cheap confidence signal.

Report attrition: "N candidates → M confirmed, K downgraded, J retracted."

Two attack classes to check explicitly:
- **By-design is not a finding.** Standard platform conventions reported as bugs or
  vulnerabilities (honoring `https_proxy`, reading `~/.netrc`, a local dev tool shelling out
  to configured package managers) — flag only when the *implementation* adds risk beyond the
  convention itself.
- **Already rejected.** If a prior audit report exists for this repo, read its rejected/
  retracted ledger FIRST and don't re-litigate entries unless the code changed. Likewise,
  record this audit's retractions and do-NOT-fix entries with one-line reasons so the next
  audit (human or agent) doesn't re-discover them.

### Phase 3 — Strategy

1. Name the 2–4 root themes explaining most confirmed findings (repos have a few systemic causes, not 30 independent problems).
2. Per theme: target state + the principle behind it.
3. **The do-NOT-fix list** (mandatory): plausible-sounding improvements you recommend AGAINST, with reasons (effort vs. payoff, maturity, risk). An audit with no rejected ideas hasn't thought about cost.
4. Measurable "done" signals per theme (CI gate exists, suite green in <N min, zero Criticals).

### Phase 4 — Plan

Tasks an engineer or coding agent could pick up cold:
- Each task: title, one-paragraph description, files affected, acceptance criteria (verifiable, ideally a command), effort (S <2h / M half-day / L 1–2 days / XL needs breakdown), risk of the change itself, dependencies.
- **Order by leverage** = impact ÷ effort, discounted by confidence (FACT > JUDGMENT) and by
  the fix's own risk. Tiebreakers: anything that unblocks other tasks floats up (verification
  baseline, characterization tests — M0 encodes this); HIGH-confidence security floats above
  equivalent-leverage non-security; prefer tasks whose fix has a clean verification story —
  executors (human or agent) succeed at those.
- **Unverified findings get investigate-tasks, not fix-tasks.** A JUDGMENT or smell that
  survived Phase 2 without empirical confirmation becomes "investigate X, confirm or retract"
  — never a speculative code change.
- Milestones: M0 safety net (tests/CI needed before refactoring safely) → M1 correctness & security → M2 leverage (makes future work cheaper) → M3 polish.
- Quick wins (high impact, S effort) listed separately.
- Top 3 tasks: brief implementation sketch (approach, key steps, gotchas).

## Deliverable

One document, in this order: **Executive Summary** (health grade A–F with one-line justification, top 3 risks, top 3 opportunities, attrition stats; skimmable before the detail sections) · **Repo Map** · **Confirmed Findings** (by theme, sorted by severity, each with file:line, consequence, FACT/JUDGMENT, verification note; then Strengths) · **Strategy** (themes, do-NOT-fix list, done signals) · **Task Plan** (milestone table, quick wins, top-3 sketches) · **Coverage & Open Questions** (what got lighter review, what you couldn't verify and why, decisions needing a human).

Save it to a file — don't leave it only in chat. Write it outside the audited repo (audits are read-only) unless the user asks for it in-repo (e.g. `docs/audits/YYYY-MM-DD-repo-audit.md`); writing the report in-repo is the one permitted write. If the user wants the plan executed, the task format maps directly onto tickets or agent-delegation specs; offer to convert M0/M1.

## Red flags — stop and fix before delivering

- A severity justified by "best practice" instead of a concrete consequence → rewrite or downgrade.
- A finding citing a line you never read → delete it.
- Zero retractions in Phase 2 on a sizable codebase → you didn't attack hard enough. (On a small or genuinely clean repo, say that explicitly — never invent retractions to fill the stat.)
- An empty do-NOT-fix list → you haven't weighed cost.
- Recommendations that ignore the Phase 0 quality bar (enterprise gates for a weekend prototype) → recalibrate.
- Padding healthy dimensions → one sentence and move on.
