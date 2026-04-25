---
name: hex-triage
description: >
  Triage untriaged captures from raw/captures/. Routes content to the right
  files (todo.md, people profiles, project context, decisions, learnings).
  Runs automatically during /hex-startup or manually via /hex-triage.
---

# /hex-triage — Triage Captured Context

## Step 1: Find Untriaged Captures

Read all `.md` files in `$AGENT_DIR/raw/captures/`. For each file, parse the YAML frontmatter. Skip files where `triaged: true` is already set.

If no untriaged captures exist, say: "No untriaged captures." and stop.

## Step 2: Classify and Route Each Capture

For each untriaged capture, read the content below the frontmatter and determine:

### Classification

Assign one or more types:

| Type | Signal | Destination |
|------|--------|-------------|
| **action_item** | "remind me to", "need to", "follow up", "TODO", deadlines, asks | `$AGENT_DIR/todo.md` |
| **reminder** | "don't forget", dates, time-sensitive notes | `$AGENT_DIR/todo.md` (with date if mentioned) |
| **person_info** | Names + roles, preferences, org info, relationship notes | `$AGENT_DIR/people/{name}/profile.md` |
| **meeting_note** | "meeting with", "talked to", "key takeaways", attendee names | `$AGENT_DIR/projects/{project}/context.md` or `$AGENT_DIR/me/learnings.md` |
| **decision** | "decided", "approved", "agreed", "we're going with" | `$AGENT_DIR/projects/{project}/decisions/{date}_{slug}.md` or `$AGENT_DIR/me/decisions/{date}_{slug}.md` |
| **project_context** | Updates about a known project, status changes, blockers | `$AGENT_DIR/projects/{project}/context.md` |
| **idea** | "what if", "we could", brainstorms, feature ideas | `$AGENT_DIR/me/ideas.md` (create if needed) |
| **general** | Anything that doesn't fit above | `$AGENT_DIR/me/learnings.md` |

### Routing Rules

1. **Match to existing projects first.** Check `$AGENT_DIR/projects/` for known project directories. If the capture mentions a known project, route there.
2. **Match to existing people.** Check `$AGENT_DIR/people/` for known people directories. If the capture is about a known person, route there. If the person directory doesn't exist, create it with a `profile.md`.
3. **Action items always go to todo.md** in addition to any other destination. Format as a checkbox line: `- [ ] {action} (captured {date})`.
4. **Be conservative.** If you're unsure where something belongs, ask the user: "I captured this but I'm not sure where to route it: '{first 80 chars}...'. Where should it go?"
5. **Preserve the original text.** When appending to a destination file, include the full capture content, not a summary.

## Step 3: Write Routed Content

For each capture, append the content to the destination file(s):

- Use **Edit** (not Write) for existing files to avoid overwriting.
- For new files (new person profile, new decision record), use **Write**.
- When appending to `todo.md`, add items under the appropriate section or at the bottom.
- When appending to `learnings.md` or `context.md`, add a dated entry:

```markdown
### {date} — From capture
{content}
```

## Step 4: Mark Captures as Triaged

For each processed capture, update its frontmatter to add:

```yaml
triaged: true
routed_to:
  - todo.md
  - people/sarah/profile.md
```

Use **Edit** to update the frontmatter in the original capture file. Replace the closing `---` of the frontmatter block with the new fields before `---`.

## Step 5: Print Summary

Print a single summary line:

```
Triaged N captures: X action items added to todo, Y person profiles updated, Z project contexts updated.
```

Be specific about what was routed where. Example:

```
Triaged 3 captures:
- 1 action item added to todo.md (follow up with Sarah)
- 1 person profile updated (people/jake/profile.md)
- 1 project context updated (projects/roadmap/context.md)
```
