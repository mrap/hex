# Default Standing Orders

These 10 rules work for any project. They're the foundation hex agents follow.

---

### 1. Search memory before guessing

Before making assumptions about architecture, conventions, or history, query the memory system. Five seconds of searching saves five minutes of wrong assumptions.

```bash
python3 .hex/memory/search.py 'relevant topic'
```

### 2. Persist discoveries immediately

When you learn something about the project — a convention, a gotcha, a decision — save it to memory right now. Not later. Not at the end of the session. Now. Memories that aren't saved are lost.

```bash
python3 .hex/memory/save.py 'what you learned' --tags 'topic' --source 'where'
```

### 3. Verify before asserting

Don't state that a file exists without checking. Don't claim a function takes certain parameters without reading it. Don't say a test passes without running it. Use tools to verify, then speak.

### 4. Read before writing

Always read the current state of a file before modifying it. Code changes since your last read. Blind edits cause conflicts and bugs.

### 5. Preserve existing patterns

The codebase has conventions — naming, formatting, error handling, file organization. Match them. Search memory for documented patterns. When in doubt, look at adjacent code and do what it does.

### 6. One change, one commit

Each commit should do one thing. Don't bundle a bug fix with a refactor with a new feature. Atomic commits are reviewable, revertable, and understandable.

### 7. Fail loudly

If you're uncertain, say so. If the request is ambiguous, ask for clarification. A confident wrong answer costs more than an honest "I'm not sure — let me check."

### 8. Test your changes

After modifying code, run the relevant tests. If there are no tests for what you changed, flag that as a gap. Untested changes are liabilities.

### 9. Check the landing

At the start of each session, check `.hex/landings/` for the latest context snapshot. It tells you what's in progress, what's blocked, and what matters right now. Don't skip this.

### 10. Leave context for next time

Before a session ends, save what you worked on, what's still open, and any decisions made. The next session (maybe you, maybe another agent) should be able to pick up without re-discovery.
