---
description: Friday weekly close — review lands, draft shareable post, log outcomes to posts/.
---

# /hex-post-week

Runs the Friday close cadence. Reviews the week's lands, produces a draft post for LinkedIn/X/personal site, and logs the outcome.

## When to run

- Friday evening (or Saturday morning if Friday slipped)
- When Mike wants to publish a weekly recap before the next week starts

## What this command does

1. **Load this week's weekly file:** `landings/weekly/<YYYY-Wxx>.md`.
2. **Review each land:** for every M# and P#, look at (a) its sub-items + Status, (b) the daily landings files from Mon-Fri of this week that tagged that land (`grep -l 'M1\|P1' landings/YYYY-MM-DD.md`), (c) any shipped artifacts (git commits, BOI q-IDs, decision records).
3. **Generate per-land outcome:**
   - *Landed:* one-line outcome + pointer to artifact
   - *Partial:* what moved, what's blocked, carry-forward to next week
   - *Didn't ship:* honest reason, plus: does it still matter for H1?
4. **Draft the post.** Use Mike's voice (from `me/learnings.md` Communication + Presentation sections):
   - Direct, no hedging
   - Tables where structure helps
   - Lead with the ask or the win
   - No em dashes
   - First-person, short sentences
   - Include the biggest learning (often the non-obvious insight, not the shipped thing)
5. **Write to 3 places:**
   - Appended to the weekly file's `## Friday post draft` section
   - Standalone at `landings/weekly/posts/<YYYY-Wxx>.md` for copy-paste
   - Summary card to `#cos` with the post preview + a link to the full draft
6. **Set up next week:** pre-generate `landings/weekly/<next-week>.md` from the template with carry-forward threads already populated. Monday's `/hex-plan-week` lands on a warm start.

## Output format

The draft post has 3 parts:

```
This week:
— <M1 outcome one-liner>
— <P1 outcome one-liner>
— <anything else notable>

Biggest learning: <one line, non-obvious>

Next week:
— <M1' look-ahead>
— <P1' look-ahead>
```

Mike edits to voice, then ships via his normal publishing path. Don't auto-publish — explicit review + ship.

## Constraints

- Be honest about what didn't land. Hiding misses erodes the whole system.
- Don't invent learnings. If the week was execution-only with no insight, say "no new learning — execution week."
- The post is for Mike's brand. Keep it tight — target ≤ 150 words.
- If the weekly file was never actually filled in on Monday, the Friday close is mostly retrospective; name the planning miss explicitly.

## See also

- `/hex-plan-week` — the Monday pair
- `landings/weekly/posts/` — archive of shipped weekly recaps
