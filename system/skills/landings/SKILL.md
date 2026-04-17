---
name: daily-landings
description: >
  Plan daily landings — concrete end-of-day outcomes prioritized by who you're
  blocking, what's blocking you, and your own deliverables. Also supports weekly
  target setting, mid-day check-ins, and open thread tracking. Use at the start
  of every work day after data pulls are complete. Also use when the user says
  "plan my day", "what should I land today", "daily landings", "set landings",
  "update landings", "how are my landings", "landing check-in", "set weekly
  landings", "weekly targets", or "weekly check-in".
version: 1.0.0
---
<!-- # sync-safe -->

# Daily Landings

## Overview

Landings are **outcomes, not tasks.** "Ship the auth flow PR" not "work on auth." They represent what the user wants to have **achieved** by end of day.

## Persistence

- Landings are saved to `landings/YYYY-MM-DD.md` (one file per day, history preserved).
- On morning creation: write the full landings file.
- On check-in/update: re-read the file, update status on each landing, write back.
- The landings file is the source of truth for the day's priorities.

## Weekly Targets Layer

Weekly targets are set on Monday mornings. They represent **end-of-week outcomes** that guide daily landing selection throughout the week.

### File Structure
- Weekly file: `landings/weekly/YYYY-WXX.md` (ISO week number)
- One file per week, persisted for history

### Weekly Targets Format
Each weekly target uses `W{n}` prefix:
```markdown
### W1. {outcome}
**Status:** Not Started | In Progress | Done | At Risk | Dropped | Stalled | Deprioritized
**Daily touchpoints:** Mon, Wed, Fri
{narrative about current state}
```

### Weekly Modes

**Mode A: Monday Morning — Set Weekly Targets**
Run when no `landings/weekly/YYYY-WXX.md` exists for the current week, or when user says "set weekly landings" / "weekly targets".

Procedure:
1. Read `todo.md` — active projects, this week section, waiting-on-others
2. Read `projects/*/context.md` — current state and blockers
3. Read last week's weekly file (if exists) — carry over incomplete targets
4. Present open workstreams grouped by project
5. Ask: **"What are your weekly targets?"**
6. Structure targets with W1-Wn numbering and daily touchpoint suggestions
7. Add an End-of-Week Checklist with concrete verification criteria
8. Persist to `landings/weekly/YYYY-WXX.md`

**Mode B: Mid-Week Check-In**
Run when user says "weekly check-in" or proactively on Wednesday/Thursday.

Procedure:
1. Read current weekly file
2. Assess each target: On Track / At Risk / Done / Dropped
3. Surface anything that changed since Monday (new blockers, scope shifts)
4. Update the file with status + notes in "Mid-Week Check-In" section
5. Present summary table

### Integration with Daily Landings

During daily morning creation, the daily-landings skill MUST:
1. Read the current week's `landings/weekly/YYYY-WXX.md` (Phase 0)
2. Surface weekly targets alongside open loops (Phase 2)
3. When presenting topics for landing selection (Phase 4), annotate which topics map to weekly targets
4. When formatting final landings (Phase 6), add a `**Weekly target:** W{n}` line to any daily landing that maps to a weekly target

## Modes

### Mode 1: Morning Creation (default at startup)

Run this when no `landings/YYYY-MM-DD.md` exists for today, or when the user explicitly says "set landings" / "plan my day".

### Mode 2: Check-In / Update

Run this when `landings/YYYY-MM-DD.md` already exists and the user says "update landings", "how are my landings", "landing check-in", or similar. Also run proactively mid-session if significant progress has been made.

---

## Morning Creation Procedure

### Phase 0: Load Weekly Targets

Read `landings/weekly/YYYY-WXX.md` for the current ISO week.
- If it exists: load weekly targets as context for daily planning.
- If it does NOT exist and today is Monday: trigger weekly target setting first (Mode A above), then continue.
- If it does NOT exist and today is NOT Monday: note "No weekly targets set this week" and continue normally.

### Phase 1: Gather Raw Material

Read from:
- `todo.md` — active projects, open loops, waiting-on-others, backlog
- Today's calendar (`raw/calendar/`) if available
- Recent messages (`raw/messages/`) if available
- `projects/*/context.md` — current project state
- `me/me.md` — role context

### Phase 2: Surface Open Loops by Topic

Group all open items by **topic** (not by project or source). A "topic" is a coherent thread of work that may span projects, people, and message threads. Merge related items — if two things are part of the same workstream, they're one topic.

For each topic:

| Loop | Status | Last Activity |
|------|--------|---------------|

If weekly targets are loaded, annotate each topic with its weekly target mapping:

| Loop | Status | Last Activity | Weekly Target |
|------|--------|---------------|---------------|

### Phase 3: Surface Relevant Messages

For each topic, attach any recent messages (last 48 hours) that are relevant. Flag:
- Anyone who needs the user explicitly
- Blocker updates (resolved or new)
- Team member status changes (out, blocked)

### Phase 4: Present to User for Landing Selection

If weekly targets are loaded, show them first:

**This week's targets:**
| # | Target | Status | Days Remaining |
|---|--------|--------|----------------|

Then present the grouped topics and ask: **"What are your landings for today? Consider which weekly targets need progress."**

Do NOT pre-select landings. The user decides.

### Phase 5: Prioritize and Structure the Landings

Once the user states their landings, apply this priority framework:

1. **L1: Others blocked on you** — Things blocking people who depend on your output from making progress today. Unblocking others is the highest-leverage action you can take.
2. **L2: You're blocked on others** — Dependencies you need to chase to unblock your own work. Cross-team handoffs, API contracts, design decisions.
3. **L3: Your deliverables** — Your own work product: code, docs, decisions that need to land. Important for credibility but lower priority than unblocking.
4. **L4: Strategic** — Relationship-building, visibility work, process improvements, career plays. Important but flexible timing.

Within each tier:
- Order by number of dependencies (more downstream impact = higher)
- Order by how core to active project goals

### Phase 6: Format the Final Landings

For each landing:

```markdown
### L{n}. {Landing outcome statement}
**Priority:** {L1/L2/L3/L4} — {one line on why this tier}
**Weekly target:** W{n} — {target name} (if applicable)
**Status:** Not Started

| Sub-item | Owner | Action | Status |
|----------|-------|--------|--------|
```

### Phase 7: Suggest Morning Action Sequence

After landings are locked, propose a numbered sequence for the morning based on what can be done immediately (async messages, quick reviews) vs. what requires meetings or deep work.

### Phase 8: Set Meeting Outcomes

For each meeting today, define a concrete outcome that maps to a landing. If a meeting doesn't map to any landing, recommend skipping.

| Time | Meeting | Maps to Landing | Proposed Outcome | Skip? |
|------|---------|-----------------|------------------|-------|

### Phase 9: Persist

Write the final landings to `landings/YYYY-MM-DD.md` using this structure:

```markdown
# Daily Landings — YYYY-MM-DD (Day)

## Focus
{One-line summary of the day's theme}

## Weekly Targets (WXX — Day N of 5)
| # | Target | Status | Notes |
|---|--------|--------|-------|

## Landings

### L1. {outcome}
**Priority:** L1 — {reason}
**Weekly target:** W{n} — {target name}
**Status:** Not Started

| Sub-item | Owner | Action | Status |
|----------|-------|--------|--------|

### L2. {outcome}
...

## Morning Sequence
1. {action}
2. {action}
...

## Meeting Outcomes
| Time | Meeting | Landing | Outcome | Skip? |
|------|---------|---------|---------|-------|

## Open Threads
### T1. {Thread name}
**State:** {current state}
**Next action:** {what to do next}

## Changelog
- HH:MM — Landings set
```

---

## Check-In / Update Procedure

### Step 1: Load Current Landings

Read `landings/YYYY-MM-DD.md` for today.

### Step 2: Gather Progress Signals

Check:
- Recent messages for updates since last check-in
- Any work items completed or reviewed
- Meeting outcomes (if meetings happened)
- Any new blockers surfaced

### Step 3: Update Each Landing

For each landing, update:
- **Status**: `Not Started` → `In Progress` → `Done` / `Blocked` / `Dropped`
- Sub-item statuses (use `Done ✓` for completed sub-items)
- Add notes on what changed

### Step 4: Surface New Items

If new urgent items appeared (messages, meetings, escalations), flag them:
- Does this warrant a new landing?
- Does this change priority of existing landings?

### Step 5: Persist Updates

Write the updated landings back to `landings/YYYY-MM-DD.md`. Add a changelog entry at the bottom:

```markdown
## Changelog
- HH:MM — L1 status → In Progress (sent contract to reviewer)
- HH:MM — Added L5: {new landing} (escalation from standup)
```

### Step 6: Present Summary

Show a quick status table:

| # | Landing | Status | Next Action |
|---|---------|--------|-------------|
