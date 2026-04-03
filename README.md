<p align="center">
  <h1 align="center">hex</h1>
  <p align="center"><b>persistent memory for AI coding agents</b></p>
  <p align="center">
    <a href="#quick-start">Quick Start</a> ·
    <a href="#how-it-works">How It Works</a> ·
    <a href="#philosophy">Philosophy</a>
  </p>
  <p align="center">
    <img src="https://img.shields.io/badge/dependencies-zero-brightgreen" alt="Zero Dependencies">
    <img src="https://img.shields.io/badge/python-3.8+-blue" alt="Python 3.8+">
    <img src="https://img.shields.io/badge/storage-SQLite_FTS5-orange" alt="SQLite FTS5">
    <img src="https://img.shields.io/badge/license-MIT-lightgrey" alt="MIT License">
  </p>
</p>

---

Your AI agent forgets everything between sessions. hex fixes that.

Every time you start a new session with Claude Code, Codex, Cursor, Aider, or Gemini CLI, your agent starts from zero. It re-discovers your conventions. It asks questions you already answered. It makes mistakes you already corrected. You lose 10–15 minutes per session to context rebuilding.

hex gives your agent a persistent memory backed by SQLite FTS5. No API keys. No cloud services. No dependencies beyond Python's standard library. Just files in your repo that your agent reads automatically.

## The difference

**Without hex** — every session starts from scratch:
```
You: Fix the auth middleware
Agent: What framework are you using?
You: Express, like I told you last time
Agent: What's your auth strategy?
You: JWT with refresh tokens, same as the last 3 sessions
Agent: Let me look at the codebase...
(10 minutes of re-discovery)
```

**With hex** — your agent remembers:
```
You: Fix the auth middleware
Agent: [searches memory → finds: Express + JWT + refresh tokens, auth middleware
        is in src/middleware/auth.ts, last issue was token expiry edge case]
Agent: I see the auth middleware in src/middleware/auth.ts. Based on previous context,
       you're using JWT with refresh tokens. Looking at the recent issue with token
       expiry handling...
(starts working immediately)
```

## Quick start

**30 seconds. Zero dependencies.**

```bash
# 1. Use this template (click "Use this template" on GitHub) or clone it
git clone https://github.com/mrap/hex-hermes.git .hex-bootstrap
cp -r .hex-bootstrap/{CLAUDE.md,AGENTS.md,setup.sh,.hex} your-project/
cd your-project

# 2. Run setup
bash setup.sh

# 3. Done. Your agent now has persistent memory.
```

That's it. Next time Claude Code, Codex, or Cursor opens your project, it reads `CLAUDE.md` / `AGENTS.md` and knows how to use the memory system.

## How it works

hex is just files and SQLite. No magic.

```
your-project/
├── CLAUDE.md              # Agent instructions (Claude Code reads this automatically)
├── AGENTS.md              # Agent instructions (Codex, Cursor, Gemini CLI)
└── .hex/
    ├── memory/
    │   ├── memory.db      # SQLite FTS5 database (gitignored)
    │   ├── search.py      # Search memories: python3 .hex/memory/search.py 'query'
    │   ├── save.py        # Save a memory:   python3 .hex/memory/save.py 'content'
    │   └── index.py       # Index workspace:  python3 .hex/memory/index.py
    ├── landings/           # Daily context snapshots (what your agent reads first)
    ├── evolution/          # Self-improvement logs and retrospectives
    └── standing-orders/    # Behavioral rules your agent follows
```

**Memory operations** are plain Python scripts your agent calls via shell:

```bash
# Search (FTS5 full-text search)
python3 .hex/memory/search.py 'authentication middleware'

# Save a discovery
python3 .hex/memory/save.py 'JWT refresh tokens stored in httpOnly cookies' \
  --tags 'auth,security' --source 'src/middleware/auth.ts'

# Index all markdown files into memory
python3 .hex/memory/index.py
```

## Features

- **Zero dependencies** — Python 3.8+ standard library only. SQLite FTS5 ships with Python.
- **Works with any agent** — Claude Code (`CLAUDE.md`), Codex/Cursor/Gemini CLI (`AGENTS.md`), or anything that can read files and run shell commands.
- **Full-text search** — SQLite FTS5 with BM25 ranking. Sub-millisecond queries on thousands of memories.
- **Incremental indexing** — `index.py` hashes content chunks and skips unchanged files. Rebuilds in seconds.
- **Git-friendly** — Memory DB is gitignored. Config and instructions are committed. Team members get the structure; memories are local.
- **Idempotent setup** — Run `setup.sh` as many times as you want. It won't clobber existing data.
- **Template repo** — Click "Use this template" to bootstrap any new project.

## Compatible agents

| Agent | Reads config from | Status |
|-------|-------------------|--------|
| Claude Code | `CLAUDE.md` | ✅ Works now |
| OpenAI Codex | `AGENTS.md` | ✅ Works now |
| Cursor | `AGENTS.md` / `.cursorrules` | ✅ Works now |
| Aider | `AGENTS.md` | ✅ Works now |
| Gemini CLI | `AGENTS.md` | ✅ Works now |

Any agent that can read workspace files and execute `python3` commands will work.

## Philosophy

hex is built on three ideas:

**1. Landings** — Every session starts with a context snapshot. What's in progress, what's blocked, what was decided. Your agent reads this first and hits the ground running instead of re-discovering your project from scratch. Landings have tiers: L1 (critical blockers) through L4 (background context).

**2. Standing orders** — Persistent behavioral rules your agent follows across sessions. "Search memory before guessing." "Persist discoveries immediately." "Verify before asserting." These compound over time — your agent gets better the longer you use it.

**3. Evolution** — Your agent observes its own friction patterns, logs what slows it down, and proposes improvements. A retrospective loop that turns repeated mistakes into standing orders. The system improves itself.

These three layers work together: landings provide context, standing orders provide discipline, and evolution provides growth. The result is an agent that doesn't just remember — it gets better.

## Memory system details

The memory database uses SQLite FTS5 for full-text search with BM25 ranking. Each memory record stores:

| Field | Description |
|-------|-------------|
| `content` | The memory text (searchable via FTS5) |
| `tags` | Comma-separated tags for filtering |
| `source` | Origin file or context |
| `timestamp` | ISO 8601 creation time |

Search returns results ranked by relevance. The `--compact` flag gives one-line summaries suitable for agent consumption. The `--context` flag controls how much surrounding text to show.

## Contributing

hex is intentionally simple. PRs that add external dependencies will be closed. PRs that make the memory system smarter, the standing orders better, or the evolution engine more useful are welcome.

## License

MIT — see [LICENSE](LICENSE).
