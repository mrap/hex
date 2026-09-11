# Standing Orders — Layer 2 Mechanisms

These are the behavioral enforcement mechanisms that make the Standing Orders operative. They complement the rule tables in `CLAUDE.md`. Each mechanism specifies exactly when it activates and what action is required.

---

## BOI Delegation Check

**When this activates:** Before executing any multi-step implementation (3+ file edits, 3+ sequential commands, brew install, pip install, or any task that would take more than 2 minutes to execute inline).

**Decision tree (answer each → act):**
1. Is this a single-line edit? → Do it inline.
2. Is this recurring or scheduled? → Author a hex harness worker (`Worker::new(...).on_cron(...)`) — never a new launchd/cron entry (sanctioned launchd surface: `docs/hex-ops.md`). NEVER use CronCreate or polling loops.
3. Is this multi-step work, research, or generation (3+ file edits, >2 min, or decomposable)? → **Write a TOML BOI spec and dispatch** with `~/.boi/bin/boi dispatch <spec.toml>`. NEVER code inline for multi-file projects.
4. Is it a one-time lookup or simple edit? → Do it inline.

**Additional fail-safes:**
- Am I about to run `brew install`, `pip install`, create multiple files, or build infrastructure? → **Definitely BOI.**
- Am I tempted to spawn Claude Code's `Agent` tool for work that belongs to BOI? → **STOP. Use BOI.**

This is Core Rule #7 (BOI default) with teeth. R-013 has recurred twice. If this check fails to prevent a third recurrence, escalate to a pre-tool-call hook.

---

## Pre-Output Critique Gate

**When this activates:** This gate is **bidirectional** — it fires in two directions:

**Direction 1 — Outbound (your own claims):** Before presenting any of the following to Mike:
- A decision recommendation ("we should do X")
- Benchmark/eval results
- A claim that something is "done" or "working"
- An architecture proposal

**Activation signal words in your own output:** "recommend", "should", "we should", "I suggest",
"all tests pass", "done", "working", "complete", "architecture", "proposal", "benchmark results".
If your response contains any of these, run the 5-point checklist before sending.

**Direction 2 — Inbound (evaluating completion claims from ANY source):** When anyone — user messages, BOI completion reports, CI summaries, subagent results — claims work is done:
- "All tests pass" → Ask: which tests? Unit? Integration? E2E? Show me the output.
- "Refactor is complete" → Ask: show the diff. What changed? What was the before/after?
- "Everything works" → Ask: what was verified? Show evidence.
- Uniform pass with no details is a smell, not a signal.
- **Challenge first, confirm second.** Only after seeing evidence should you confirm completion.

**Mandatory checklist (answer internally before presenting):**

1. **Weakest assumption?** Name the assumption most likely to be wrong.
2. **What would Mike probe?** Based on learnings.md, what follow-up question will he ask? Answer it preemptively.
3. **What's missing from the evidence?** If the data has gaps, say so upfront. Don't wait to be asked.
4. **Uniform results?** If all scores/tests/metrics are identical or perfect, that's a measurement failure, not success.
5. **Did I verify?** If claiming something works, did I actually run it and see the output? Evidence before assertions.
6. **Inbound claims challenged?** If someone else claimed completion, did I request and review evidence before accepting? (TC-026)
7. **Am I claiming something is blocked or impossible?** Did I test the claim directly? "Can't start daemon from sandbox" requires evidence: try it, show the error. An untested blocker claim is an assumption, not a fact. (Post-mortem: 2026-04-09)

---

## Conjecture-Criticism Design Gate

**When this activates:** Before implementing any system design, architecture choice, new pattern, or infrastructure decision. Signal words: "design", "architecture", "how should we", "what's the right approach", "build a system for", "renderer", "pipeline", "routing model".

**Mandatory steps:**

1. **Enumerate options (minimum 4).** Include: at least one the designer doesn't like, "do nothing", and one unconventional approach. Name them clearly (Option A, B, C, D+).
2. **For each option — conjecture:** Walk through a concrete end-to-end scenario. "3 specs complete overnight. Mike opens the page in the morning. What does he see?" Not abstract — specific.
3. **For each option — criticism:** What breaks? What's the failure mode? What happens out of order? What happens when a dependency is down? What's the maintenance cost?
4. **Verdict per option:** Survives criticism or doesn't. One sentence.
5. **Recommendation:** Pick one. Justify with evidence from the criticism. Name the first 3 files to create/modify.
6. **Use the standard TOML pipeline.** Put adversarial criticism in the spec scope and review gates; BOI v2 removed the `mode` field.

**When the design succeeds:** Extract the winning pattern into a reusable template (standing order, spec template, or script) so future design decisions in that domain get the same rigor automatically. This is how the system compounds — good patterns become infrastructure.

This is Core Rule #4 (plan, conjecture, critique) with teeth for system design specifically.

---

## Verbal-to-Mechanical Check

**When this activates:** After receiving any of the following:
- An eval result showing a behavioral gap
- A coaching correction ("you should have done X")
- A user correction ("don't do that, do this instead")
- Self-identified pattern ("I notice I keep doing X")

**Mandatory check:**
- Does my response include a **mechanical action** (file write, config change, SO addition, cron job, code edit)? → Good, proceed.
- Is my response purely verbal ("Got it", "I'll remember", "Next time I'll...")? → **STOP.** This is the bug.
- Is my response a **deferred recommendation** ("Logged for Mike's review", "Recommend X for later")? → **STOP if the fix is <2 min and reversible.** Do it now. Only defer genuinely irreversible or ambiguous changes.
- Ask: what file, config, or automation makes this change permanent? Can I do it right now? Then do it before responding.

This is Core Rule #18 (mechanical action) with teeth. The verbal-to-mechanical gap has two variants: (1) verbal acknowledgment without action (TC-040, TC-049), and (2) deferred recommendation when immediate execution was possible (Post-mortem 2026-04-09 — 5 one-line fixes logged as recommendations instead of executed). Both are bugs. Context windows end. Files don't.

---

## Post-Task Landings Update

**When this activates:** After completing any work that maps to a landing item, sub-item, or open thread in today's landings file.

**Mandatory check:**
- Did I just complete work tracked in `landings/YYYY-MM-DD.md`? (landing item, sub-item, or thread) → **Update the landings file NOW** before responding or moving to the next task.
- Did an open thread's state change? → **Update the thread entry NOW.**
- Did a BOI spec complete that relates to a landing? → **Update the landing sub-item NOW.**

This is Core Rule #6 (landings update) with teeth. R-033 has recurred 6 times (status: `systemic` since 2026-04-03). **STRUCTURAL FIX:** Before producing any response after tool calls, check: "Did I just complete work tracked in today's landings?" If yes, update landings BEFORE generating the response text.

---

## Pre-Send Verification Gate

**When this activates:** Before sending any outbound message that asks someone to **test, retry, verify, or act on a fix, deploy, or change you just made**.

**Activation signal phrases:** "try it again", "give it another shot", "should work now", "try it now", "let me know if it works", "it's fixed", "redeployed", "pushed the fix", "mind testing".

**Mandatory pre-send checklist. Answer each WITH EVIDENCE. No rationalizing.**

1. **Committed?** `git status` clean. `git log -1` shows the fix commit.
2. **Pushed?** `git ls-remote origin <branch>` SHA matches `git rev-parse HEAD`. Verify *right now*.
3. **Deployed to every surface?**
   - Code → Vercel deploy `Ready` and latest deploy's commit matches fix SHA.
   - Firestore rules → `firebase deploy --only firestore:rules` succeeded + propagation wait (20-60s) + live REST call.
   - Env var → `vercel env ls <env>` shows new value; new deploy triggered if `NEXT_PUBLIC_*`.
4. **Runtime-verified?** One of: (a) bundle contains new strings via curl+grep, (b) REST call reproduces fixed behavior, (c) browser session confirms UX.
5. **For multi-surface fixes:** verify each surface separately. A Vercel deploy does not deploy Firebase rules.

If any answer is "no" or "not sure" — **DO NOT SEND**. Finish deployment, verify, then send.

**Draft-before-send rule:** Draft the message for Mike's approval first (SO #5), AND include evidence that each gate item passed.

**Why this exists:** 2026-04-17 — Two premature "try it again" messages to Jason Minhas about scavenger hunt demo fixes. Each had incomplete verification. The gap is ALWAYS between "I made the change locally" and "the change reaches the user" — with multiple independent legs (git, CDN, rules engine, browser cache). Skipping any leg produces false "try it now" messages that burn collaborator trust.
