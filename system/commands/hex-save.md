---
name: hex-save
description: >
  Save current session. Parses transcripts into readable daily files
  and rebuilds the memory search index.
---

# /hex-save — Save Session

## Steps

1. **Parse transcripts**: Convert raw .jsonl session data into readable daily markdown.

```bash
python3 $AGENT_DIR/.hex/scripts/parse_transcripts.py
```

2. **Rebuild memory index**: Update the search index with any new or changed files.

```bash
python3 $AGENT_DIR/.hex/skills/memory/scripts/memory_index.py
```

3. **Report**: Tell the user what was saved.

Format: "Saved. [N] transcript(s) parsed, [M] files indexed."
