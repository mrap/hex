---
name: hex-reflect
description: >
  Session reflection and self-improvement. Reviews the full conversation to
  extract learnings, identify failure patterns, and produce concrete upgrades
  to hex's operating model. Generates standing orders, learnings entries, skills,
  and behavioral rules — auto-applied after adversarial critic validation.
  Use at session end, on-demand via /hex-reflect, or when the session had
  corrections or pushback.
version: 1.0.0
---
<!-- # sync-safe -->

# Session Reflection

## Philosophy

Error correction is the engine of progress. Every session is an experiment:
conjectures are tested against reality (user feedback), and failures reveal
gaps in the operating model. A session without reflection is a session without
compounding — the same mistakes recur, the same friction persists, the same
opportunities are missed.

This skill treats each conversation as empirical data. Corrections are not
embarrassments; they are the highest-value signal available. The goal is not
to avoid mistakes but to ensure no mistake happens twice for the same reason.
Knowledge grows through conjecture and criticism (Deutsch): the reflection
protocol is criticism applied to the agent's own behavior, producing conjectures
(fixes) that are then tested by future sessions.

The measure of a good operating model is not the absence of errors but the
speed at which errors are permanently resolved.

## When This Activates

### Auto-trigger
- At session end, before `/hex-shutdown` — the shutdown command should invoke
  or prompt for reflection
- When the session contained corrections: user pushed back, said "no", redirected,
  or expressed frustration
- When the session contained non-obvious problem-solving that should become a skill

### Manual trigger
- User invokes `/hex-reflect`
- User says "reflect", "what did we learn", "what should change", "session review"

### Skip (do not trigger)
- Sessions that were purely informational (no actions taken)
- Sessions where the user explicitly says "skip reflection"
- Sessions shorter than 5 exchanges with no corrections

## Protocol

Execute these steps in order. Do not skip steps. Do not reorder.

1. **Announce.** Print: "Running session reflection..."
2. **Load history.** Read `evolution/reflection-log.md` to get previously tracked issues,
   recurrence counts, and the highest R-NNN ID. Read the Standing Orders table in
   `$HEX_DIR/CLAUDE.md` and `$HEX_DIR/me/learnings.md` for cross-referencing.
3. **Phase 1: Extract issues.** Scan the full conversation using the 6 issue categories
   (Corrections, Friction, Missed opportunities, New conventions, Solved problems,
   Quality gaps). Follow the Extraction Procedure: chronological scan, proactive scan,
   structured records, de-duplication, severity ordering.
4. **Recurrence check.** Match extracted issues against the reflection-log history. Flag
   recurring issues and update recurrence counts (Phase 3, section 3.3).
5. **Adversarial critic.** For each issue, answer the 4 critic questions: Is it real?
   Is the root cause accurate? Is the fix sufficient? What's missing? Remove false
   positives, revise weak root causes, check against existing standing orders. Run the
   meta-pattern analysis across all issues.
6. **Phase 2: Generate fixes.** For each issue that survived the critic, generate a
   concrete fix using the fix templates (standing-order, learnings, skill, claude-md,
   behavioral-rule). Each fix has exact text, target file, location, and rationale.
7. **Phase 3: Define verification.** For each fix, write a recurrence test. Prepare
   the reflection-log entry with new R-NNN IDs. Compute session and cumulative metrics.
8. **Present reflection report.** Output the full report in the Output Format (below).
   Include the review checklist with every proposed fix.
9. **Phase 4: Auto-apply.** Apply all fixes that passed the adversarial critic. The critic
   IS the quality gate. If a fix causes problems, it will be caught by recurrence tracking
   and escalated automatically.
10. **Apply fixes.** Apply each fix using the correct tool and order
    (CLAUDE.md edits → standing orders → learnings → skills → behavioral rules →
    evolution entries). Verify each write with a read-back check.
11. **Update evolution engine.** Write to reflection-log.md, changelog.md, and
    (if recurring) observations.md and suggestions.md. Deduplicate before writing.
    Rebuild memory index if the script exists.
11a. **Write eval signal.** After updating the evolution engine, write the session
    reflection to a JSON file and call session-delta.py to persist eval_records:
    ```bash
    # session_reflection.json must contain: session_id, session_date,
    # sos_violated (list), sos_held (list), sos_not_tested (list)
    python3 "$HEX_DIR/evolution/eval/session-delta.py" \
      --input /tmp/hex-reflect-session.json \
      --output /tmp/hex-reflect-delta.json \
      --db "$HEX_DIR/.hex/memory.db"
    ```
    The JSON file should be written to `/tmp/hex-reflect-session.json` before
    calling session-delta.py. If memory.db does not exist, skip this step silently.
12. **Report.** Print: "Reflection complete. Applied N/M fixes." List what was applied
    and what was skipped.

## Phase 1: Issue Extraction

This is the core analysis engine. Scan the full session conversation and identify
issues across 6 categories. Use your own reasoning to analyze the conversation —
do not rely on regex or keyword matching. Read for intent, tone, and context.

### Issue Categories

| Category | What to look for | Detection signals |
|----------|-----------------|-------------------|
| Corrections | User explicitly corrected a claim or approach | "No, actually...", "That's not right", pushback, "you're wrong", providing documentation that contradicts a claim, redirecting after a wrong approach |
| Friction | Agent spun or took multiple attempts unnecessarily | Multiple tool calls for the same goal, backtracking, "let me try again", repeated failures before success, unnecessary complexity in approach |
| Missed opportunities | Agent should have acted proactively but didn't | User had to explicitly ask for something the agent should have done (skill creation, convention persistence, security vetting, proactive research) |
| New conventions | Rules or preferences user established during the session | "From now on...", "always...", "never...", naming conventions, file path conventions, workflow preferences, tool preferences |
| Solved problems | Non-obvious solutions that should become reusable knowledge | Multi-step investigation that found an undocumented approach, workaround for an API limitation, a technique that required significant research |
| Quality gaps | Output didn't meet user's quality bar | "Too verbose", "over-engineered", "simplify", rejection of a draft, "that's not what I meant", scaling back scope after over-delivery |

### Extraction Procedure

Follow these steps in order. Do not skip steps.

**Step 1: Chronological scan**

Read the full conversation from start to finish. For each user message, ask:
- Did the user correct a claim I made? (Category: Corrections)
- Did the user push back on my approach? (Category: Corrections or Quality gaps)
- Did the user redirect me to do something different? (Category: Quality gaps)
- Did the user express frustration or impatience? (Category: Friction or Quality gaps)
- Did the user have to ask me to do something I should have done proactively? (Category: Missed opportunities)
- Did the user state a preference or convention? (Category: New conventions)

For each agent action (tool calls, responses), ask:
- Did this succeed on the first try? If not, why not? (Category: Friction)
- Did I assert something confidently that turned out to be wrong? (Category: Corrections)
- Did I give up on an approach without exhausting alternatives? (Category: Corrections)
- Did I over-engineer or over-scope the response? (Category: Quality gaps)

**Step 2: Proactive scan**

After the chronological scan, make a second pass looking for things that *didn't* happen:
- Were any conventions stated but not persisted to CLAUDE.md or learnings? (Category: New conventions)
- Were any non-obvious problems solved but not captured as skills? (Category: Solved problems)
- Were any tools or dependencies installed without security vetting? (Category: Missed opportunities)
- Were any categories dismissed without real analysis? (Category: Corrections)

**Step 3: Extract structured records**

For each identified issue, create a record in this format:

```markdown
### Issue: [short descriptive title]
**Category:** [one of: Corrections | Friction | Missed opportunities | New conventions | Solved problems | Quality gaps]
**Evidence:** [direct quote from the conversation or specific description of what happened — be concrete, not vague]
**Root cause:** [why the agent behaved this way — must be specific and actionable, not generic like "didn't try hard enough"]
**Fix type:** [one of: standing-order | learnings | skill | claude-md | behavioral-rule | mechanism]
**Severity:** [one of: high | medium | low]
```

Severity definitions:
- **high** — Caused real harm, significant delay, or required the user to provide information the agent should have found. The agent was confidently wrong, or gave up when a solution existed.
- **medium** — Caused friction or annoyance. The agent's approach worked but was suboptimal, verbose, or over-engineered.
- **low** — Suboptimal but minor. A missed proactive action or a convention that should be persisted.

**Step 4: De-duplicate**

Merge issues that share the same root cause into a single issue with multiple evidence points.
For example, if the agent gave up too easily on two different API calls, that's one issue
("gives up too easily") with two evidence points, not two separate issues.

**Step 5: Order by severity**

Sort the final issue list: high severity first, then medium, then low.
Within the same severity, order by impact (which issues cost the most time or trust).

## Phase 2: Fix Generation

For each issue that survives the Adversarial Critic (see below), generate a concrete,
applicable fix. Every fix must be exact text ready to write — not a summary, not a
suggestion, not a paraphrase. Fixes are auto-applied after passing the adversarial critic.

### Pre-Generation: Read Current State

Before generating any fixes, read these files to avoid duplicates and get current numbers:

1. **Read `$HEX_DIR/CLAUDE.md`** — Find the Standing Orders table. Note the highest
   standing order number (the `#` column). New standing orders use N+1.
2. **Read `$HEX_DIR/me/learnings.md`** — Note the existing section headings. New
   learnings go under the correct section, or create a new section if none fits.
3. **Read `$HEX_DIR/evolution/observations.md`** — Check for existing entries that
   match the identified issues. Update rather than duplicate.

### Fix Templates

Each fix type has a specific template. Use the exact format — hex parses these files
with consistent formatting expectations.

---

#### Fix Type: `standing-order`

A new row in the Standing Orders table in CLAUDE.md.

**Template:**
```markdown
| N+1 | **Rule text.** Explanation with enough context that a fresh session understands why. | YYYY-MM-DD |
```

**Format rules:**
- `N+1` is the next number after the current highest standing order
- Rule text is bold. Explanation follows in the same cell, not bold.
- Date is the date the reflection identified the issue
- The rule must be actionable: an imperative ("Always X", "Never Y", "Before Z, do W")
- Max one sentence for the rule, one sentence for the explanation

**Example:**
```markdown
| 18 | **Try 3 approaches before declaring impossible.** When a first attempt fails, research harder. "I don't know how" ≠ "it can't be done." | 2026-03-11 |
```

**Target file:** `$HEX_DIR/CLAUDE.md`
**Location:** Append as last row in the Standing Orders table (before the "To add a new rule:" instruction line)

---

#### Fix Type: `learnings` (Learnings entry)

A new bullet under the appropriate section of me/learnings.md.

**Template:**
```markdown
- Observation text. Evidence or context in parenthetical. (YYYY-MM-DD)
```

**Format rules:**
- One bullet per observation
- End with the date in parentheses
- Place under the correct existing section heading:
  - Communication Style, Presentation Style, Decision-Making, Work Patterns,
    Design & Architecture Thinking, Content & Brand Strategy, Leadership Patterns,
    What Motivates Them, Debugging Lessons, Environment & Tooling Preferences,
    Delegation & Autonomy Preferences, UX & Product Preferences,
    Agent Interaction Signals, Agent Failure Patterns, Workflow Preferences (Specific),
    How They Process Information, What They Value From the Agent, What Frustrates Them,
    Stewardship & Standards, Self-Improvement & Autonomy, Meta-Learning Approach,
    Entrepreneurial Identity
- If no section fits, create a new `## Section Name` heading at the end of the file
- Check for existing bullets that say the same thing. If one exists, update it instead

**Example:**
```markdown
- Over-engineers solutions when simple functionality is requested. "What's the simplest thing that could work?" before designing systems. (2026-03-11)
```

**Target file:** `$HEX_DIR/me/learnings.md`
**Location:** Under the section heading that best matches the observation

---

#### Fix Type: `skill`

A new SKILL.md file for a reusable capability discovered during the session.

**Template:**
```yaml
---
name: skill-name
description: >
  One-line description of what this skill does and when to use it.
version: 1.0.0
---

# Skill Title

## When This Activates
- Trigger conditions

## Protocol
1. Step one
2. Step two
...
```

**Format rules:**
- YAML frontmatter between `---` markers
- Name is kebab-case
- Description is one sentence
- Protocol is numbered steps, imperative voice
- Include a `## When This Activates` section with concrete trigger conditions

**Target file:** `$HEX_DIR/.hex/skills/<skill-name>/SKILL.md`
**Location:** New file (create directory if needed)

---

#### Fix Type: `claude-md`

An edit to CLAUDE.md that is not a standing order (e.g., updating a section, adding a
behavioral instruction, modifying a protocol).

**Template:**
```
**old_string:** [exact text to find in CLAUDE.md]
**new_string:** [exact replacement text]
```

**Format rules:**
- Provide the exact old_string and new_string for the Edit tool
- old_string must be unique in CLAUDE.md (include enough surrounding context)
- If appending to a section rather than replacing, specify: "Append after: [section heading]"
- The edit must be minimal — change only what's needed, preserve surrounding context

**Target file:** `$HEX_DIR/CLAUDE.md`
**Location:** Specified by old_string context or section heading

---

#### Fix Type: `behavioral-rule`

A specific behavioral instruction written as an imperative. These go into the most
appropriate file depending on scope.

**Template:**
```markdown
- **Rule:** [imperative instruction: "Always X when Y", "Never Z without W"]
- **Scope:** [where this rule applies: all sessions | specific skill | specific workflow]
- **Target file:** [where to write it]
- **Location:** [section heading or "append to end"]
```

**Format rules:**
- The rule must be a single imperative sentence
- Scope determines where it lives:
  - All sessions → CLAUDE.md (Standing Orders or relevant section)
  - Specific skill → That skill's SKILL.md
  - Specific workflow → The relevant command .md file
- If the behavioral rule is broad enough for Standing Orders, prefer `standing-order` fix type instead

---

#### Fix Type: `mechanism`

A Layer 2 enforcement mechanism. Mechanisms are code or structural changes that make it
impossible (or difficult) to repeat a mistake, unlike standing orders which are advisory.

**When to use instead of standing-order:** When the same issue has recurred (recurrence >= 1)
despite an existing standing order. The standing order failed to prevent the behavior;
a mechanism enforces it structurally.

**Template:**
```markdown
- **Mechanism:** [description of the enforcement: hook, validation gate, pre-check, etc.]
- **Tier:** [1-6, referencing the layer2-mechanisms doc]
- **Prevents:** [R-NNN issue(s) this mechanism blocks]
- **Implementation:** [where it lives: hook script, CLAUDE.md gate, etc.]
- **Test:** [concrete test: what input should be blocked/flagged]
```

**Decision tree:**
- Recurrence count 0 → use standing-order or learnings (first offense, advisory is fine)
- Recurrence count 1 → consider mechanism (advisory failed once)
- Recurrence count 2+ → **require mechanism** (advisory has failed repeatedly)

**Target file:** Varies. See the layer2-mechanisms doc for the tier system.

---

### Fix Record Format

For each issue, generate a fix record in this format:

```markdown
#### Proposed Fix for: [Issue Title]

**Fix type:** [standing-order | learnings | skill | claude-md | behavioral-rule]
**Target file:** [absolute path using $HEX_DIR]
**Location:** [section heading, table position, or "new file"]
**Exact text:**
```
[the complete, ready-to-write text — not a summary]
```
**Rationale:** [one sentence: why this fix prevents recurrence of this specific issue]
```

### Review Checklist

After generating all fixes, present them as a numbered review checklist:

```markdown
## Proposed Fixes — Review Checklist

| # | Issue | Fix Type | Target File | Status |
|---|-------|----------|-------------|--------|
| 1 | [title] | standing-order | CLAUDE.md | auto-applied |
| 2 | [title] | learnings | me/learnings.md | auto-applied |
| ... | ... | ... | ... | ... |
```

Group fixes by type for easier review:
1. Standing orders (changes to CLAUDE.md rules)
2. Learnings (observations about the user)
3. Skills (new reusable capabilities)
4. CLAUDE.md edits (non-standing-order changes)
5. Behavioral rules (imperatives for specific scopes)

**Note:** Fixes that pass the adversarial critic are auto-applied. If a fix causes problems,
it will be caught by recurrence tracking and escalated automatically.

## Phase 3: Verification & Tracking

Every fix must be testable. If a fix can't be verified by observing future sessions, it's
too vague. This phase defines recurrence tests, tracks fix effectiveness over time, and
escalates when fixes fail.

### 3.1 Define Recurrence Tests

For each fix generated in Phase 2, write a recurrence test:

```markdown
**Recurrence test:** "If [specific observable behavior] occurs again in a future session, this fix failed."
```

The test must be concrete and observable — not aspirational. Good and bad examples:

| Good recurrence test | Bad recurrence test |
|---------------------|---------------------|
| "If the agent declares something impossible without trying 3 approaches" | "If the agent doesn't try hard enough" |
| "If user has to ask for a skill to be created after a non-obvious problem is solved" | "If the agent isn't proactive enough" |
| "If a convention stated in conversation is not persisted by session end" | "If the agent forgets things" |

The test should reference the specific behavior, not the general category.

### 3.2 Reflection Log Entry Format

After the reflection report is presented and fixes are applied, write an entry
to `evolution/reflection-log.md` in this format:

```markdown
## [YYYY-MM-DD] Session Reflection

### Issues Identified: N
### Fixes Proposed: N
### Fixes Applied: N

| ID | Issue | Fix Type | Fix Applied | Recurrence Count | Status |
|----|-------|----------|-------------|-----------------|--------|
| R-NNN | Short issue title | standing-order | YYYY-MM-DD | 0 | monitoring |
| R-NNN | Short issue title | learnings | YYYY-MM-DD | 0 | monitoring |
| R-NNN | Short issue title | skill | — | — | skipped |
```

**ID assignment:** IDs are globally unique across the entire reflection log. Format: `R-NNN`
where NNN is a zero-padded sequential number. Read the existing log to find the highest ID
and increment from there. If the log is empty, start at R-001.

**Status values:**
- `monitoring` — Fix was applied, watching for recurrence
- `resolved` — No recurrence after 5+ sessions since fix applied
- `inadequate` — Issue recurred after fix was applied (recurrence >= 1)
- `needs-escalation` — Recurrence count >= 3, fix needs to be stronger
- `skipped` — User chose not to apply this fix

### 3.3 Recurrence Tracking

On each reflection run, before extracting new issues, scan for recurrence of previously
logged issues:

**Step 1: Load history**
Read `evolution/reflection-log.md` and collect all entries with status `monitoring` or
`inadequate`.

**Step 2: Match new issues against history**
For each new issue identified in Phase 1, check if it matches a previously logged issue.
Matching criteria:
- Same root cause pattern (not just same category)
- Similar evidence (agent exhibited the same behavior)
- The previous fix should have prevented this but didn't

**Step 3: Update recurrence counts**
If a match is found:
1. Increment the `Recurrence Count` for the matched entry
2. Change status to `inadequate` (if not already)
3. Append a dated note to the log entry: `- YYYY-MM-DD: Recurred. [brief evidence]`
4. Flag the new issue as "recurring" in the Phase 1 output

**Step 4: Escalation rules**
When recurrence reaches thresholds, escalate the fix:

| Recurrence Count | Current Fix Type | Escalation |
|-----------------|-----------------|------------|
| 1 | Any | Mark `inadequate`, propose strengthened fix |
| 2 | behavioral-rule | Escalate to standing-order |
| 2 | learnings | Escalate to standing-order or behavioral-rule |
| 3+ | standing-order | Escalate to skill or hook (automated enforcement) |
| 3+ | Any | Status → `needs-escalation`, flag for user's attention |

Escalation means the existing fix is insufficient. The new fix must be qualitatively
different — not just rewording the same rule. Examples:
- A standing order that keeps being ignored → a skill that enforces the behavior procedurally
- A learnings entry that doesn't change behavior → a standing order that's checked every session
- A behavioral rule that's too easy to skip → a pre-commit hook or startup check

### 3.4 Resolution

Mark an issue as `resolved` when:
- The fix has been applied AND
- At least 5 reflection runs have occurred since application AND
- The issue has not recurred (Recurrence Count still 0)

Resolution is automatic — the skill checks on each run and updates status.

### 3.5 Metrics Summary

After processing recurrence and new issues, compute and report:

```markdown
### Session Metrics
- New issues this session: N
- Recurring issues detected: N
- Fixes proposed: N
- Fixes requiring escalation: N

### Cumulative Metrics
- Total issues identified (all time): N
- Total fixes applied (all time): N
- Recurrence rate: X% (issues with recurrence >= 1 / total issues with applied fixes)
- Resolution rate: X% (resolved issues / total issues with applied fixes)
- Top recurring categories: [list of categories with most recurrences]
- Average time to resolution: N sessions
```

These metrics are reported in the reflection output and also written to the bottom of
`evolution/reflection-log.md` (updated each run, not appended — only one Metrics section
exists at the end of the file).

## Phase 4: Auto-Apply

Fixes that pass the adversarial critic are automatically applied. The critic is the
quality gate — not a human approval step. This phase presents the reflection report,
applies fixes, verifies them, and logs everything. If a fix causes problems, recurrence
tracking will catch it and escalate automatically.

### 4.1 Present the Reflection Report

After Phase 1 (extraction), the Adversarial Critic, Phase 2 (fix generation), and Phase 3
(verification criteria) are complete, present the full reflection report to the user.

The report follows the Output Format section (below). Key elements:
- Summary: N issues found, M fixes proposed
- Each issue with category, severity, evidence, root cause, critic assessment
- Each proposed fix with exact text, target file, and location
- Review checklist table with Apply / Skip / Modify columns

Output the full report as a markdown message so the user can read it.

### 4.2 Apply Fixes

All fixes that passed the adversarial critic are applied automatically. No human approval
step is needed — the critic IS the quality gate. If a fix causes problems, it will be
caught by recurrence tracking in future reflections and escalated.

For each fix, apply it using the appropriate tool:

| Fix Type | Tool | Action |
|----------|------|--------|
| `standing-order` | Edit | Append row to Standing Orders table in `$HEX_DIR/CLAUDE.md`. Insert before the "To add a new rule:" instruction comment, or at the end of the table. |
| `learnings` | Edit | Add bullet under the correct section heading in `$HEX_DIR/me/learnings.md`. If adding to end of section, use the next section heading as anchor. |
| `skill` | Write | Create new directory and SKILL.md at `$HEX_DIR/.hex/skills/<name>/SKILL.md`. |
| `claude-md` | Edit | Apply the exact old_string → new_string replacement in `$HEX_DIR/CLAUDE.md`. |
| `behavioral-rule` | Edit | Write the rule to the specified target file and location. |
| Evolution entries | Edit | Append entries to `evolution/changelog.md`, `evolution/observations.md`, or `evolution/suggestions.md` as specified. |

**Apply in this order** (dependencies may exist between fixes):
1. CLAUDE.md edits (non-standing-order) — these may change context for standing orders
2. Standing orders — append to the table
3. Learnings entries — add observations
4. Skills — create new skill files
5. Behavioral rules — write to target files
6. Evolution entries — log everything

### 4.3 Post-Apply Verification

After applying each fix, verify it landed correctly:

**Step 1: Read-back check**
After each Edit or Write, use the Read tool to read the target file and confirm:
- The edit is present at the expected location
- Surrounding content was not corrupted
- Formatting is consistent with the rest of the file

If a read-back check fails, report the failure and log it. Do not continue applying
fixes for that target file. Other fixes may still proceed.

**Step 2: Log to reflection-log.md**
Append a new dated section to `evolution/reflection-log.md` with:
- Date
- Issues identified count
- Fixes proposed count
- Fixes applied count
- The tracking table with IDs, issue titles, fix types, and statuses

Use the format defined in Phase 3, section 3.2. Assign new R-NNN IDs by reading the
current highest ID in the log and incrementing.

For skipped fixes, log them with status `skipped` and Fix Applied = `—`.

**Step 3: Log to changelog.md**
For each applied fix, append an entry to `evolution/changelog.md`:
```markdown
- **[reflection]** Applied fix for [issue title] (re: [root cause summary]) (YYYY-MM-DD)
```

This matches the existing changelog format (e.g., `- **[bug-fix]** description (re: issue) (date)`).

**Step 4: Update evolution engine files**
If any issues are recurring (matched against previous reflection-log entries in Phase 3):
- Update `evolution/observations.md` with the friction entry (see Evolution Engine Integration)
- Write `evolution/suggestions.md` entries for applied fixes

**Step 5: Rebuild memory index**
Run the memory indexer to ensure new learnings and skills are discoverable:
```bash
python3 $HEX_DIR/.hex/skills/memory/scripts/memory_index.py
```
If the memory index script doesn't exist or fails, skip this step silently — it's
non-critical and the index may not be configured yet.

### 4.4 Report Summary

After all fixes are applied and verified, output the report:

```
Reflection complete. Applied N/M fixes.

Applied:
- [fix type] [issue title] → [target file]
- [fix type] [issue title] → [target file]
...

Rejected by critic:
- [issue title] (reason: [critic reason])
...

Failed:
- [issue title] (error: [what went wrong])
...

Logged to evolution/reflection-log.md (IDs: R-NNN through R-NNN).
```

The user sees the report but is not asked to approve. The report is informational.

## Adversarial Critic

The critic is a mandatory self-review step that runs between Phase 1 (issue extraction) and
Phase 2 (fix generation). Its purpose: prevent false positives, challenge lazy root causes,
and ensure fixes are genuinely effective. The critic is adversarial — it tries to kill each
issue. Only issues that survive the critic reach Phase 2.

Do not skip the critic to save time. A false positive that reaches the user's review erodes
trust in the reflection system. A bad fix that gets applied creates noise in CLAUDE.md.
The critic is the quality gate.

### Critic Procedure

For each issue identified in Phase 1, answer these four questions. Be honest — the goal
is accuracy, not a high issue count.

#### Question 1: Is this a real issue?

Challenge the issue's validity:
- Did the user actually push back, or is this a misread of the conversation?
- Is the "evidence" a correction, or just a normal clarification or discussion?
- Could this be standard conversation flow rather than a failure? (e.g., user refining
  requirements is not a correction; user saying "no, that's wrong" is)
- Is the agent interpreting a preference as a failure? (e.g., user choosing option B
  over option A isn't an error — it's a decision)
- Would a reasonable observer reading the transcript agree this is a failure?

**Kill criterion:** If the evidence doesn't clearly show the agent made an error, was
wrong, missed something, or fell below quality bar — remove the issue. Label it
`removed (false positive)` in the critic output.

#### Question 2: Is the root cause accurate?

Challenge the stated root cause:
- Does this root cause actually explain why the behavior happened?
- Is there a deeper root cause? (e.g., "didn't research enough" might be caused by
  "treats first failure as definitive" — the deeper cause is the real one)
- Is the root cause specific enough to generate a fix? A root cause like "wasn't careful
  enough" is useless. "Pattern-matches on surface similarity instead of measuring actual
  cost" is actionable.
- Is the root cause shared with other issues? (If so, they should be merged in Phase 1's
  de-duplication step — send them back for merging)

**Revision criterion:** If the root cause is vague, generic, or wrong, revise it. Write
the revised root cause and mark the issue `revised (root cause updated)`.

#### Question 3: Is the fix sufficient?

Challenge the proposed fix type and direction:
- Would this fix actually prevent recurrence of this specific issue?
- Is the fix too narrow? (Catches this exact case but not the pattern. e.g., "add
  tweet.fields=article to X API calls" is too narrow — the pattern is "exhaust API
  documentation before declaring a feature missing")
- Is the fix too broad? (Creates unnecessary overhead. e.g., "research every claim for
  10 minutes before stating it" is too broad — the fix should target confident assertions
  about impossibility specifically)
- **Critical check: existing rules.** Read the current Standing Orders in CLAUDE.md.
  Does an existing standing order already cover this issue? If so:
  - Why didn't the existing rule prevent this issue?
  - The fix should address *why the existing rule was insufficient* (reinforce, clarify,
    or escalate the existing rule) — **not add a duplicate rule**
- Is the fix type appropriate? (e.g., a behavioral rule that says "always do X" might be
  better as a standing order if it applies to all sessions)

**Revision criterion:** If the fix wouldn't prevent recurrence, or if an existing rule
already covers the issue, revise the fix type and direction. Mark the issue
`revised (fix updated)`.

#### Question 4: What's missing?

After critiquing all individual issues, do a completeness check:
- Are there issues in the session that Phase 1 missed?
- Are there conventions the user stated that weren't captured as issues?
- Are there problems the agent solved non-obviously that should become skills?
- Are there security, performance, or reliability concerns that were glossed over?

If the critic finds missing issues, add them back to the Phase 1 output with category,
evidence, root cause, fix type, and severity. Mark them `added by critic`.

### Existing Rules Cross-Reference

Before finalizing any issue, read the current Standing Orders table in `$HEX_DIR/CLAUDE.md`
and the entries in `$HEX_DIR/me/learnings.md`. For each issue, explicitly check:

1. Is there an existing standing order that should have prevented this? Note the order number.
2. Is there an existing learnings entry that describes this pattern?
3. If yes to either:
   - The issue still stands (the rule/entry exists but failed to prevent the behavior)
   - But the fix must address **why the existing rule didn't work**, not add a duplicate
   - Options: strengthen the wording, add specificity, escalate to a skill, add a checklist

Document this cross-reference in the critic output for each issue:
```markdown
**Existing rules check:** [None / Standing order #N overlaps — fix addresses why it was insufficient]
```

### Meta-Pattern Analysis (Completeness Check)

After all individual issues are critiqued, step back and look for patterns across issues:

- Do 2+ issues share the same root cause? → They should be merged, or the shared root
  cause should get its own fix (the individual fixes may be symptoms)
- Do issues cluster in one category? → The category itself may be a meta-pattern
  (e.g., 3 "Corrections" issues about incorrect claims suggest a systemic verification gap)
- Is there a progression? (e.g., issue A led to issue B, which led to issue C) →
  Fix the upstream cause, not just the downstream symptoms
- Are there anti-patterns? (e.g., the agent's default response to uncertainty is to
  assert confidence rather than express uncertainty)

If a meta-pattern is found, add it as a separate high-severity issue with:
- Evidence: the 2+ issues that form the pattern
- Root cause: the underlying systemic cause
- Fix: a systemic fix (often a standing order or skill rather than a learnings entry)

### Critic Output Format

For each issue, the critic appends its assessment:

```markdown
**Critic assessment:** [real | revised | removed | added by critic]
**Critic notes:** [1-2 sentences explaining the assessment]
**Existing rules check:** [None | Standing order #N — explanation]
```

For removed issues:
```markdown
**Critic assessment:** removed (false positive)
**Critic notes:** [why this isn't a real issue — be specific]
```

For the meta-pattern analysis:
```markdown
### Meta-Pattern: [title]
**Evidence:** Issues [#, #, #] share root cause pattern: [description]
**Systemic fix:** [the broader fix that addresses all of them]
```

### Critic Self-Check

Before finalizing, the critic asks itself:
1. Am I being too lenient? (Letting weak issues pass to inflate the count?)
2. Am I being too harsh? (Killing valid issues because the evidence isn't dramatic?)
3. Did I actually check the existing standing orders, or did I assume I know them?
4. Did I look for missing issues, or just critique what was already found?

If in doubt on any issue, mark it `needs human review` rather than removing it. Let the user decide.

## Evolution Engine Integration

The reflection skill feeds into hex's existing evolution pipeline. Each evolution file
has a specific role and format. The skill writes to these files at specific points in
the protocol — never speculatively, always after the adversarial critic validates them
(except for reflection-log.md, which is the skill's own tracking file).

### Integration Point 1: `evolution/observations.md`

**When:** After a fix is applied AND the issue has been seen in 2+ sessions (recurrence
detected in Phase 3). Single-occurrence issues do not get observations entries — they
are tracked in reflection-log.md until they recur.

**Action:** Create or update a friction entry in `$HEX_DIR/evolution/observations.md`.

**Format** (matches existing entries):
```markdown
## [Issue title]
**Category:** reflection-pattern
**Status:** open
**Occurrences:** N
**Log:**
- YYYY-MM-DD: [evidence summary from this session]
- YYYY-MM-DD: [evidence summary from previous session]
**Impact:** [how this affects productivity/quality — one sentence]
**Notes:** [relevant context: what fix was tried, why it didn't work]
```

**Field rules:**
- **Category** is always `reflection-pattern` for reflection-sourced observations
- **Status** starts as `open`. Changes to `resolved` when the reflection-log entry is resolved.
- **Occurrences** matches the Recurrence Count from reflection-log.md + 1
- **Log** entries are chronological. Each entry is one line: `- YYYY-MM-DD: brief evidence`.
- **Impact** must be concrete ("Wastes 10 minutes per session on re-research" not "bad")
- **Notes** should reference the reflection-log IDs: `Tracked as R-NNN in reflection-log.md`

### Integration Point 2: `evolution/suggestions.md`

**When:** When fixes are generated in Phase 2. Every proposed fix also gets a suggestion
entry so the evolution engine has a record of what was proposed.

**Action:** Append a suggestion entry to `$HEX_DIR/evolution/suggestions.md`.

**Format** (matches existing entries):
```markdown
## [YYYY-MM-DD] Suggestion: [fix name — short descriptive title]
- **What:** [the fix — one sentence description]
- **Why:** Pattern observed N times (reflection log: R-NNN)
- **How:** [fix type: standing order / learnings entry / skill / CLAUDE.md edit / behavioral rule]
- **Expected benefit:** [specific improvement — what changes if this fix is applied]
- **Status:** applied
```

**Status lifecycle:**
- `applied` — Fix has been applied and verified
- `rejected-by-critic` — Adversarial critic removed this fix

Update the status after Phase 4 (auto-apply). All fixes that pass the critic are applied
immediately.

**Note:** Only create suggestion entries for fixes in the current session. Do not create
suggestion entries for previously applied fixes being tracked for recurrence.

### Integration Point 3: `evolution/changelog.md`

**When:** After each fix is successfully applied in Phase 4.

**Action:** Append a changelog entry to `$HEX_DIR/evolution/changelog.md`.

**Format** (matches existing entries):
```markdown
- **[reflection]** [description of what was changed] (re: [root cause summary]) (YYYY-MM-DD)
```

**Rules:**
- One entry per applied fix
- The tag is always `[reflection]` (distinct from `[bug-fix]`, `[skill]`, etc.)
- The `(re: ...)` section names the root cause, not the fix
- Date is the date of application, not the date of the session where the issue was found

### Integration Point 4: `evolution/reflection-log.md`

**When:** At the end of every reflection run, regardless of whether fixes were applied.

**Action:** Append a dated section to `$HEX_DIR/evolution/reflection-log.md` and update
the Metrics section at the bottom.

This is the skill's primary tracking file. The format is defined in Phase 3, section 3.2.
The reflection-log is always written — it tracks issues even when fixes are skipped.

**Rules:**
- Append new entries above the `## Metrics` section
- Replace (not append) the `## Metrics` section with updated cumulative numbers
- IDs are globally sequential: read the highest existing R-NNN and increment

### Integration Point 5: Deduplication

Before writing to any evolution file, check for existing entries that match the new content.
Duplication wastes the user's attention and clutters the evolution pipeline.

**Deduplication procedure:**

**Step 1: Check observations.md**
Before creating a new observation entry, read `$HEX_DIR/evolution/observations.md` and
search for entries with:
- The same or similar title (fuzzy match)
- The same category (`reflection-pattern`)
- The same root cause pattern

If a match is found:
- **Do not create a new entry.** Instead, update the existing entry:
  - Increment `Occurrences`
  - Append a new dated line to `Log`
  - Update `Notes` if new context is available
  - Update `Status` if appropriate

**Step 2: Check suggestions.md**
Before creating a new suggestion, read `$HEX_DIR/evolution/suggestions.md` and check
for existing suggestions that propose the same fix.

**Step 3: Check changelog.md**
Changelog entries are append-only and never deduplicated. Each application is a unique
event. However, if the same fix is applied multiple times (due to recurrence), include
a note: "re-applied (previous: YYYY-MM-DD)"

**Step 4: Check reflection-log.md**
Reflection-log entries are always unique (each reflection run gets its own section).
However, when matching new issues against historical entries for recurrence tracking
(Phase 3, section 3.3), use the reflection-log as the primary source of truth.

## Output Format

The reflection report follows this exact structure. Every field is required unless marked
optional. Use this as a template — copy and fill in the values.

```markdown
# Session Reflection — YYYY-MM-DD

## Summary
[1-2 sentences: N issues found across K categories, M fixes proposed. Note any recurring
issues or escalations.]

## Issues

### Issue: [short descriptive title]
**Category:** [Corrections | Friction | Missed opportunities | New conventions | Solved problems | Quality gaps]
**Severity:** [high | medium | low]
**Evidence:** [direct quote or concrete description — not vague]
**Root cause:** [specific, actionable cause]
**Critic assessment:** [real | revised | removed | added by critic] — [1-2 sentence explanation]
**Existing rules check:** [None | Standing order #N — why it was insufficient]
**Recurrence:** [new | recurring (R-NNN, count: N)]

#### Proposed Fix
**Fix type:** [standing-order | learnings | skill | claude-md | behavioral-rule]
**Target file:** [absolute path using $HEX_DIR]
**Location:** [section heading, table row, or "new file"]
**Exact text:**
\```
[the complete, ready-to-write text]
\```
**Rationale:** [one sentence: why this prevents recurrence]
**Recurrence test:** "If [specific observable behavior], this fix failed."

---

[repeat for each issue]

## Meta-Patterns (optional)

### Meta-Pattern: [title]
**Evidence:** Issues [#, #, #] share root cause: [description]
**Systemic fix:** [the broader fix addressing all of them]

## Fixes Summary

| # | Issue | Fix Type | Target File | Status |
|---|-------|----------|-------------|--------|
| 1 | [title] | standing-order | CLAUDE.md | auto-applied |
| 2 | [title] | learnings | me/learnings.md | auto-applied |
| ... | ... | ... | ... | ... |

## Session Metrics
- New issues this session: N
- Recurring issues detected: N
- Fixes proposed: N
- Fixes requiring escalation: N

## Cumulative Metrics
- Total issues identified (all time): N
- Total fixes applied (all time): N
- Recurrence rate: X%
- Resolution rate: X%
- Top recurring categories: [list]

## Reflection Log Entry

[The entry to write to evolution/reflection-log.md — pre-formatted with R-NNN IDs,
tracking table, and status values, ready to append.]
```

### Format Rules

- **Evidence must be concrete.** Quote the conversation or describe the exact behavior.
- **Root causes must be actionable.** If you can't derive a fix from the root cause, it's
  too vague. Rewrite until a fix naturally follows.
- **Exact text must be copy-pasteable.** The text in the fix should work if pasted directly
  into the target file with no editing. Include proper markdown formatting.
- **One issue per heading.** Do not combine multiple issues under a single `### Issue:` block
  even if they share a root cause.
- **Horizontal rules** (`---`) separate issues for readability.
- **Metrics are computed, not estimated.** Read the reflection-log to compute actual totals.
  If the log is empty (first run), cumulative metrics equal session metrics.

## Execution Mode

hex-reflect runs as a **post-session Stop hook** via `claude -p`. This is fully decoupled
from the interactive session — it cannot be interrupted by context window limits, user
navigation, connection drops, or session timeouts.

### Architecture: Stop Hook + `claude -p`

```
Interactive Session                     Post-Session (background)
┌──────────────┐                       ┌──────────────────────┐
│ User works   │                       │ Stop hook fires      │
│ Session ends │──Stop event──────────>│ 1. backup_session.sh │
│              │                       │    (saves transcript) │
└──────────────┘                       │ 2. session-reflect.sh│
                                       │    (runs claude -p   │
                                       │     with transcript  │
                                       │     + reflect prompt)│
                                       │ 3. Applies fixes     │
                                       │ 4. Logs results      │
                                       └──────────────────────┘
```

### How It Works

1. **Transcript saved first.** The existing `backup_session.sh` Stop hook copies the
   latest `.jsonl` transcript to `raw/transcripts/`. This already runs on every session
   stop.

2. **Reflection runs second.** A new Stop hook entry runs `session-reflect.sh`, which:
   - Reads the saved transcript from `raw/transcripts/`
   - Constructs a prompt containing the full reflection protocol + transcript content
   - Calls `claude -p --dangerously-skip-permissions` in the agent working directory
   - The `claude -p` session executes the reflection protocol and applies fixes directly
   - Output is logged to `evolution/reflection-log.md`

3. **Backgrounded.** The reflection script runs in the background (`nohup ... &`) so it
   doesn't block the Stop hook or terminal. If the user starts a new session before
   reflection finishes, there's no conflict — reflection writes to evolution files, not
   active session state.

4. **Error handling.** If `claude -p` fails, the script logs the error and retries once.
   If it still fails, the raw transcript is preserved for manual reflection next session.

### Why This Approach

| Requirement | How It's Met |
|------------|--------------|
| Not blocking | Runs after session closes, backgrounded |
| Resilient | Stop hook fires regardless of how session ends; transcript is already saved |
| Has conversation content | Reads the saved `.jsonl` transcript |
| Can write to hex files | `claude -p` runs in the agent directory with full file access |

### Mid-Session Reflection (Checkpoint)

When reflection is triggered mid-session (via `/hex-checkpoint`), it uses the same mechanism
but runs via `Task` with `run_in_background: true` instead of waiting for a Stop hook. The
background subagent has access to conversation context and runs the reflection protocol
asynchronously.

### Script Location

`$HEX_DIR/.hex/scripts/session-reflect.sh` — the shell script that orchestrates
the post-session reflection.


## Configuration (reflect-config.yaml)

Behavior is controlled by `evolution/reflect-config.yaml` in the target repo.
If the file is not found, fall back to the hardcoded defaults listed below — do not crash.

```yaml
# evolution/reflect-config.yaml — canonical location
version: "1.0"
pattern_detection:
  recurrence_threshold_for_suggestion: 3   # default: 3
  recurrence_threshold_for_escalation: 5   # default: 5
  recurrence_window_days: 30               # default: 30
fix_generation:
  auto_apply_after_critic: true            # default: true
  max_so_additions_per_session: 3          # default: 3
  so_requires_min_severity: "medium"       # default: medium
  layer2_mechanism_threshold: 2            # default: 2
triggers:
  skip_if_below_n_exchanges: 5            # default: 5
output:
  tiered_output: true                     # default: true
  quick_issue_threshold: 3               # default: 3
  deep_issue_check_interval_days: 30     # default: 30
```

To load the config in any script (with fallback to defaults if not found):

```python
import yaml, os
cfg_path = os.path.join(os.environ.get('HEX_DIR', '.'), 'evolution/reflect-config.yaml')
try:
    with open(cfg_path) as f:
        cfg = yaml.safe_load(f) or {}
except FileNotFoundError:
    cfg = {}  # use defaults if config not found

pd = cfg.get('pattern_detection', {})
recurrence_threshold = pd.get('recurrence_threshold_for_suggestion', 3)
escalation_threshold = pd.get('recurrence_threshold_for_escalation', 5)
```
