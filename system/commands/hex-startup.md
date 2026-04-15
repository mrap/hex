# /hex-startup — Start Your Session

## Step 1: Run Startup Script

```bash
HEX_DIR="$(pwd)" bash "$(pwd)/.hex/scripts/startup.sh"
```

## Step 2: Check for First-Time Setup

Read `me/me.md`. If it contains "Your name here", this is a first-time user. Run onboarding.

### First-Time Onboarding

Ask exactly these three questions:
1. "What's your name?"
2. "What do you do?" (role, one line)
3. "What are your top 3 priorities right now?"

Write answers to `me/me.md` and `todo.md` immediately. Then: "You're set up. What's on your mind?"

### Returning User

1. Read `todo.md` for current priorities
2. Check `landings/` for today's landing targets
3. Check `evolution/suggestions.md` for pending improvements
4. If today is a workday and no landings exist, propose 3-5 based on todo.md

Surface a brief summary: "Ready. Here's what needs attention:" followed by top priorities, overdue items, pending improvements.
