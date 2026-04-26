---
description: Monday weekly planning ritual — declare 3 Meta + 3 Personal weekly lands, each tagged to a perf goal.
---

# /hex-plan-week

Runs the Monday planning cadence. Sets this week's landings against the semester's perf goals.

## When to run

- Monday morning, first session of the week
- Any time there's no `landings/weekly/YYYY-Wxx.md` for the current ISO week
- When Mike's journal or CoS flags "drifting without weekly lands"

## What this command does

1. **Read current state:** `me/perf/2026-h1.md` + `me/perf/2026-h1-personal.md` (the active semester's perf goals) + last week's weekly file (lands + outcomes).
2. **Identify current ISO week** (`date +%Y-W%V`).
3. **If weekly file already exists for this week:** load it, identify any blank/stub lands, prompt to fill.
4. **If weekly file does NOT exist:** copy `landings/weekly/_TEMPLATE.md` → `landings/weekly/<this-week>.md`, prepopulate date header and perf-goal pointers.
5. **Ask Mike (interactive, one question at a time):**
   - Review each perf goal: still active? anything changed?
   - Name 3 Meta weekly lands. For each: which perf goal does it serve? what's the success signal for end-of-week?
   - Name up to 3 Personal weekly lands. Same questions.
   - Any Open Threads from last week that carry forward?
   - Schedule map: Mike names his deep-work blocks for the week; tag each with M# or P#.
6. **Write the filled file.** Commit as `weekly: W<NN> lands declared` to local repo (don't push — per branch rules, main push is Mike's call).
7. **Cross-link to daily landings:** any existing daily landings file for today gets a pointer to the weekly.
8. **Post summary to `#cos`:** "W<NN> set: {M1 title} / {M2 title} / {P1 title} / …".

## Constraints

- Cap at 3 Meta + 3 Personal lands. If Mike proposes more, push back — fewer, clearer bets win.
- Each land must cite a perf goal. If no perf goal fits, surface it: "this land doesn't tie to an H1 goal — rename the goal OR question if the land actually matters."
- Don't let Mike leave this command without the file having real content. Stubs defeat the purpose.

## Output

- `landings/weekly/<YYYY-Wxx>.md` filled in
- `#cos` summary post
- Next trigger: Friday's `/hex-post-week` for the close

## See also

- `/hex-post-week` — Friday close + post draft
- `me/perf/` — semester goals this ties up to
- `projects/cos/charter.md` — CoS monitors the gap between declared lands and daily work
