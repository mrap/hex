---
name: hex-startup
description: >
  Full session initialization. First-time users get onboarding.
  Returning users get context loading, action items, and daily landings.
---
<!-- # sync-safe -->

# /hex-startup — Start Your Session

## Step 0: Check for Pending Migration

Before anything else, check if a migration breadcrumb exists:

```bash
test -f "$(pwd)/.hex/migrate-from"
```

If `.hex/migrate-from` exists, a bootstrap migration completed on a previous run and user data needs to be migrated from the backup directory. Invoke **hex-doctor in migration mode** before proceeding:

1. Read `.hex/migrate-from` to find the backup path
2. Follow the hex-doctor Migration procedure exactly (copy user data, verify, re-index, remove breadcrumb)
3. Report migration results to the user
4. Only continue to Step 1 after migration is complete

If `.hex/migrate-from` does not exist, skip to Step 1.

## Step 1: Run Startup Script

```bash
HEX_DIR="$(pwd)" bash "$(pwd)/.hex/scripts/startup.sh"
```

This handles: environment detection, session registration, transcript parsing, memory index rebuild, health check, integration check, and pending improvement suggestions.

## Step 2: Check for First-Time Setup

Read `me/me.md`. If it still contains the placeholder text "Your name here", this is a first-time user. Run **Onboarding** (Step 2a). Otherwise, skip to **Returning User** (Step 2b).

### Step 2a: First-Time Onboarding (Phase 1)

Ask exactly these three questions. Nothing more.

1. "What's your name?"
2. "What do you do?" (role, one line)
3. "What are your top 3 priorities right now?"

Write answers to `$HEX_DIR/me/me.md` immediately. Replace the placeholder text.

Then say: "You're set up. I'll learn more about how you work over the next few sessions. For now, let's get to work. What's on your mind?"

### Step 2b: Returning User

1. Read `todo.md` for current priorities
2. Read `me/learnings.md` for recent observations
3. Check `landings/` for today's landing targets (if any)
4. Check `evolution/suggestions.md` for pending improvement proposals
5. Check `evolution/reflection-log.md` for entries added since last session. If new entries exist, surface a summary: "Since last session: N new standing orders applied, M learnings added." List each fix briefly (one line per fix). This gives visibility into what background reflection changed.
6. If today is a workday and no landings exist for today, propose 3-5 landing targets based on todo.md

Surface a brief summary: "Ready. Here's what needs attention today:" followed by top priorities, meetings to prep, overdue items, any pending improvement suggestions, and any reflection fixes applied since last session.

## Step 3: Team Sync (if configured)

Check `teams.json`. If teams are configured, mention any unsynced updates. Don't auto-sync. Just surface: "Team updates available. Run /hex-sync-base when ready."
