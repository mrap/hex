---
name: memory
description: >
  Hex memory system — save, search, and retrieve persistent memories across
  sessions. Provides FTS5 + optional vector hybrid search over indexed files.
tags: memory, search, recall, persistence, knowledge
trigger: >
  Agent needs to save or recall information across sessions, or search
  existing memories.
version: 1
---

# Memory System

## Overview

The hex memory system stores persistent facts across sessions using SQLite FTS5 (full-text search) with optional vector embedding for semantic recall. Memories are markdown files with YAML frontmatter stored in `.hex/memory/`.

## Scripts

- **memory_search.py** — Search memories by keyword or semantic similarity
- **memory_save.py** — Save a new memory with optional tags
- **memory_index.py** — Index filesystem content into the SQLite database

## Usage

### Search
```bash
python3 .hex/skills/memory/scripts/memory_search.py "query terms"
python3 .hex/skills/memory/scripts/memory_search.py --top 5 "phrase"
python3 .hex/skills/memory/scripts/memory_search.py --compact "keyword"
```

### Save
```bash
python3 .hex/skills/memory/scripts/memory_save.py "memory content" --tags "tag1,tag2"
```

### Index
```bash
python3 .hex/skills/memory/scripts/memory_index.py --path .hex/memory/
```

## Memory Format

Memories are markdown files with YAML frontmatter:

```markdown
---
name: memory-name
description: One-line description for index lookup
type: user | feedback | project | reference
---

Memory content here.
```

## Hybrid Search

If `sqlite_vec` and `fastembed` are installed, search uses vector embeddings for semantic recall. Falls back to FTS5-only when deps are absent.
