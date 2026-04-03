# hex — Standing Orders for Claude Code

You have access to a persistent memory system. Use it.

## Memory System

Search before guessing. Save when you learn something.

```bash
# Search memories (always do this before making assumptions)
python3 .hex/memory/search.py 'your query here'
python3 .hex/memory/search.py 'your query' --top 5 --compact

# Save a discovery, decision, or correction
python3 .hex/memory/save.py 'what you learned' --tags 'relevant,tags' --source 'file.py'

# Index workspace markdown into memory
python3 .hex/memory/index.py
```

### When to search
- Before answering questions about project architecture, conventions, or history
- Before making assumptions about how something works
- When the user references something from a previous session
- When starting a new task (search for related context)

### When to save
- When you discover a project convention or pattern
- When the user corrects you — save the correction immediately
- When a decision is made about architecture or approach
- When you find something non-obvious (gotchas, workarounds, edge cases)
- When a task is completed — save a summary of what was done and why

## Standing Orders

1. **Search before guessing** — Query memory before making assumptions about the codebase. If memory has relevant context, use it.

2. **Persist immediately** — When you learn something new about the project, save it to memory right away. Don't wait until the end of the session.

3. **Verify before asserting** — Check that files exist, functions are defined, and APIs behave as expected before stating claims. Use tools to verify, not just memory.

4. **Read before writing** — Always read a file before modifying it. Never edit blind. Check the current state of the code.

5. **Atomic commits** — Each change should do one thing. Don't bundle unrelated modifications.

6. **Preserve existing patterns** — Match the codebase's existing style for naming, formatting, error handling, and structure. Search memory for documented conventions.

7. **Fail loudly** — If something is ambiguous or you're unsure, say so. Ask rather than guess wrong.

8. **Test the change** — After modifying code, run relevant tests. If no tests exist, note that as a gap.

9. **Explain the why** — When making non-obvious decisions, leave a memory note explaining the reasoning.

10. **Check landings first** — At the start of a session, check `.hex/landings/` for the latest context snapshot to understand current priorities and open threads.

## Workspace Layout

```
.hex/
├── memory/           # Persistent memory (SQLite FTS5)
│   ├── memory.db     # The database (gitignored, local)
│   ├── search.py     # Search: python3 .hex/memory/search.py 'query'
│   ├── save.py       # Save:   python3 .hex/memory/save.py 'content' --tags 'x'
│   └── index.py      # Index:  python3 .hex/memory/index.py
├── landings/         # Daily context snapshots
│   └── TEMPLATE.md   # Landing page template (L1-L4 tiers)
├── evolution/        # Self-improvement loop
│   └── README.md     # How evolution works
└── standing-orders/  # Behavioral rules
    └── defaults.md   # Core standing orders with explanations
```

## Session Start Checklist

When beginning a new session:
1. Check `.hex/landings/` for the most recent landing file
2. Search memory for context related to the user's first request
3. Note any open threads or blockers from the landing

## Session End Checklist

Before ending a session:
1. Save any new discoveries or decisions to memory
2. If significant work was done, update or create a landing file
3. Note any open threads for the next session
