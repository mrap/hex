---
name: hex-consolidate
description: Consolidate and sharpen hex's operating model. Removes contradictions, deduplicates, updates outdated entries, and ensures nothing important is lost. Run periodically or before promoting changes to the hex template repo.
---
<!-- # sync-safe -->

# /hex-consolidate — Operating Model Cleanup

## When to Run

- Before promoting changes to the hex template repo
- When CLAUDE.md exceeds 700 lines
- When standing orders accumulate faster than they're tested
- When learnings.md has entries older than 30 days without validation
- On explicit request

## Procedure

### Phase 1: Audit (read-only)

Dispatch a subagent to scan all operating model files. Do NOT modify anything yet.

**Files to audit:**
1. `CLAUDE.md` — standing orders, layer 2 mechanisms, protocols
2. `me/learnings.md` — observed patterns and preferences
3. `evolution/observations.md` — friction patterns
4. `evolution/suggestions.md` — proposed improvements
5. `evolution/changelog.md` — implemented improvements
6. `todo.md` — completed items still listed, stale entries
7. `.hex/skills/*/SKILL.md` — all skills
8. `.hex/commands/*.md` — all commands

**For each file, check:**

| Check | What to look for |
|-------|-----------------|
| **Aspirational instructions** | Rules that say "always do X" but nothing enforces it. #1 antipattern. If it can't be enforced, rewrite to match reality. |
| **Contradictions** | Two rules that conflict (e.g., SO says "always X" but another says "never X") |
| **Duplicates** | Same concept stated in different places with different wording |
| **Stale entries** | Rules referencing systems that no longer exist, or undated entries with no validation |
| **Superseded entries** | Earlier rules effectively replaced by later ones or by new systems |
| **Orphaned references** | References to files, skills, or systems that don't exist |
| **Derived files in git** | Binary files (DBs, caches, indexes) that should be in .gitignore |
| **Completed items in active sections** | `[x]` items still in "Now" sections of todo.md |

**Output:** Write audit report to `evolution/consolidation-audit-YYYY-MM-DD.md` with:
- Total items audited per file
- Issues found, categorized: REMOVE, MERGE, UPDATE, REVIEW
- For each issue: file, line/section reference, reason

### Phase 2: Start consolidation log

Create `evolution/consolidation-log-YYYY-MM-DD.md` to track every change made during this run. This log serves two purposes:
1. Accountability — what changed, why, who decided
2. Skill refinement — meta-insights get baked back into this skill

Format each change as:
```markdown
### Change N: [short name]
- **File:** which file changed
- **What:** what was done
- **Why:** reasoning (include user's input if they weighed in)
- **Risk:** what could go wrong
- **Insight:** (optional) meta-learning about the consolidation process itself
```

### Phase 3: Apply safe changes

Apply changes that need no user input. These are:
- Typo fixes (date formats, spelling)
- Removing obvious test/placeholder data
- Adding context notes to undated entries (prefer file-level notes over per-item — scales better)
- Removing derived files from git tracking (`git rm --cached`)

Log each change to the consolidation log as you go.

### Phase 4: Review items with user — one at a time

For each REVIEW item, present:

1. **What it's supposed to give us** — the intent behind the rule/entry
2. **What's working** — what's functioning as designed
3. **What's missing** — gaps, enforcement failures, scattered data
4. **Recommendation** — what to do, with reasoning

Wait for the user's response before proceeding. The user may:
- Approve the recommendation
- Redirect (change the approach)
- Expand (the review item surfaces a bigger design question — bookmark it)
- Defer (skip for now)

**Key principle:** Don't rush through review items. Each one is an opportunity for the user to teach the system something. Some items will be quick fixes. Others will pull threads that lead to foundational design insights. Let the conversation breathe.

Log each decision to the consolidation log, including the user's reasoning.

### Phase 5: Verify and summarize

After all changes:
1. Check CLAUDE.md line count (target: stay under 600 lines)
2. Verify no broken references (search for file paths that don't exist)
3. Run memory index rebuild
4. Write summary at the bottom of the consolidation log:
   - Total changes applied
   - Breakdown by type (safe fixes, decisions with user, no-changes, deferred)
   - Ideas bookmarked for future work
   - Meta-insights for baking back into this skill

### Phase 6: Refine this skill

Read the consolidation log's meta-insights section. If any insights change how future consolidations should work, update this SKILL.md now. The skill should get better each time it runs.

## Antipatterns to Watch For

These patterns recur across consolidation runs. Check explicitly:

| Antipattern | What it looks like | Fix |
|-------------|-------------------|-----|
| **Aspirational but unwired** | "Always check X on startup" but nothing calls it | Rewrite to match reality, or wire it |
| **Scattered data sources** | Same type of data in 3+ locations | Pick one canonical source, redirect writers |
| **Wrong surfacing moment** | Important info shown at startup (when user wants to work) | Move to planning (landings) or reflection (session end) |
| **Throwaway infrastructure** | Building temp fix when permanent solution is deployed but unverified | Track the dependency, wait for stabilization |
| **Over-generalized learnings** | Rule extracted from one context applied universally | Check if the user is consistently direct — sometimes the general rule IS correct |
| **Undated imports** | Bulk-imported data with no timestamps | Add a global date note at file level, not per-item |
| **Derived files in git** | Indexes, caches, DBs committed to repo | `git rm --cached`, verify in .gitignore |

## Standing Order Cross-Reference

When auditing standing orders, cross-reference against:
- `me/learnings.md` — do learnings align with or contradict SOs?
- `evolution/observations.md` — are there friction patterns caused by SOs?
- Layer 2 mechanisms — do they duplicate or conflict with SOs?
- Recent session behavior — are SOs actually being followed?
- R-codes (recurrence trackers) — are the escalation paths defined and enacted?

## Promotion Checklist

Before promoting to the hex template repo:
- [ ] All personal references removed (user name → generic)
- [ ] File paths use `$HEX_DIR` not absolute paths
- [ ] No project-specific content in CLAUDE.md
- [ ] Skills are generic enough for other users
- [ ] Standing orders are universally applicable (mark personal ones)
