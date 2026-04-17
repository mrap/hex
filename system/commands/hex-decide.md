---
name: hex-decide
description: >
  Structure a decision when you're stuck. Turns "I need to think about X" into
  a clear options table with trade-offs and a recommendation. Saves the decision
  record so future sessions have the reasoning.
---
<!-- # sync-safe -->

# /hex-decide — Structure a Decision

## Step 1: Identify the Decision

If the user provided a topic, use it. Otherwise ask: "What do you need to decide?"

Keep the scope tight. If the topic is broad ("what to do about the project"), ask: "What specific question needs an answer?"

## Step 2: Gather Context

Search memory and project files for relevant context:

```bash
python3 $HEX_DIR/.hex/skills/memory/scripts/memory_search.py "decision topic keywords"
```

Read any relevant project context files, people profiles, or past decisions.

## Step 3: Build the Decision Document

Create a structured decision document with this format:

```markdown
# Decision: {topic}
**Date:** {today}
**Status:** Draft

## Context
{Why this decision needs to be made now. 2-3 sentences max.}

## Options

| Option | Pros | Cons | Risk |
|--------|------|------|------|
| A: {name} | {benefits} | {drawbacks} | {what could go wrong} |
| B: {name} | {benefits} | {drawbacks} | {what could go wrong} |
| C: {name} | {benefits} | {drawbacks} | {what could go wrong} |

## Recommendation
{Which option and why. Be direct.}

## What Changes
{If we go with the recommendation, what happens next? Who needs to know?}
```

Rules:
- Always present 2-3 options. Never just one.
- One option can be "do nothing" if that's a valid choice.
- Pros and cons must be specific, not generic. "Faster" is bad. "Saves 2 hours per week" is good.
- The recommendation must pick a side. No "it depends."

## Step 4: Present and Discuss

Show the decision document to the user. Ask: "Does this capture the decision correctly? Want to adjust any options?"

If the user makes a choice, update Status to "Decided" and record which option was chosen.

## Step 5: Save the Decision Record

Determine where the decision belongs:
- If it relates to a known project in `$HEX_DIR/projects/`, save to `$HEX_DIR/projects/{project}/decisions/{date}_{slug}.md`
- Otherwise, save to `$HEX_DIR/me/decisions/{date}_{slug}.md`

Use today's date (YYYY-MM-DD) and a URL-safe slug of the topic.

Create the directory if it doesn't exist.

Print: "Decision recorded at {path}. Future sessions will have this context."
