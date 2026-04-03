<p align="center">
  <h1 align="center">hex 🧠</h1>
  <p align="center"><b>Persistent memory for AI coding agents</b></p>
  <p align="center"><i>Zero dependencies. Zero API keys. Zero cloud. Just SQLite.</i></p>
  <p align="center">
    <a href="#quick-start">Quick Start</a> ·
    <a href="#how-it-works">How It Works</a> ·
    <a href="#why-hex">Why hex?</a>
  </p>
  <p align="center">
    <a href="https://github.com/mrap/hex-hermes/stargazers"><img src="https://img.shields.io/github/stars/mrap/hex-hermes?style=social" alt="GitHub Stars"></a>
    <a href="https://github.com/mrap/hex-hermes/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-lightgrey" alt="MIT License"></a>
    <img src="https://img.shields.io/badge/dependencies-zero-brightgreen" alt="Zero Dependencies">
    <img src="https://img.shields.io/badge/python-3.8+-blue" alt="Python 3.8+">
    <img src="https://img.shields.io/badge/storage-SQLite_FTS5-orange" alt="SQLite FTS5">
  </p>
</p>

---

**Your AI agent has amnesia.** Every time you start a new session with Claude Code, Codex, Cursor, or Aider, it starts from zero. Re-discovers your conventions. Asks questions you already answered. Makes mistakes you already corrected.

**hex gives your agent a memory.** A persistent, searchable memory backed by SQLite FTS5 — in ~200 lines of Python standard library code with zero external dependencies.

<p align="center">
  <img src="assets/demo.gif" alt="hex demo — setup, save, search" width="700">
</p>

### The difference

**Without hex** — every session starts cold:
```
You: Fix the auth middleware
Agent: What framework are you using?
You: Express, like I told you yesterday
Agent: What's your auth strategy?
You: JWT with refresh tokens, same as the last 3 sessions...
      (10 minutes of re-discovery)
```

**With hex** — your agent remembers:
```
You: Fix the auth middleware
Agent: [searches memory → finds: Express + JWT + refresh tokens,
        auth middleware is in src/middleware/auth.ts,
        last issue was token expiry edge case]
Agent: Found relevant context in memory. Your auth middleware is in
       src/middleware/auth.ts using JWT with refresh tokens. Looking
       at the recent token expiry issue...
       (starts working immediately)
```

## Quick start

**30 seconds. Zero dependencies. Python 3.8+ only.**

### Option A: Add to an existing project (recommended)

```bash
# From your project directory:
git clone https://github.com/mrap/hex-hermes.git /tmp/hex-setup
cp -r /tmp/hex-setup/{CLAUDE.md,AGENTS.md,setup.sh,.hex} .
bash setup.sh
rm -rf /tmp/hex-setup
```

### Option B: Use as a GitHub template

1. Click **[Use this template](https://github.com/mrap/hex-hermes/generate)** on GitHub
2. Clone your new repo
3. Run `bash setup.sh`

### Verify it works

```bash
# Save your first memory
python3 .hex/memory/save.py 'Project uses Express with JWT auth, refresh tokens in httpOnly cookies' \
  --tags 'auth,architecture' --source 'initial-setup'

# Search it back
python3 .hex/memory/search.py 'authentication'
# → #1  2025-04-03T...  initial-setup
#   tags: auth,architecture
#   Project uses Express with JWT auth, refresh tokens in httpOnly cookies

# ✅ Now start Claude Code or Codex — they'll read CLAUDE.md/AGENTS.md
#    and automatically search memory before making assumptions.
```

## How it works

hex is files + SQLite. No magic, no servers, no config.

```
your-project/
├── CLAUDE.md              # Instructions for Claude Code (auto-read)
├── AGENTS.md              # Instructions for Codex, Cursor, Gemini CLI, Aider
└── .hex/
    ├── memory/
    │   ├── memory.db      # SQLite FTS5 database (gitignored — stays local)
    │   ├── search.py      # python3 .hex/memory/search.py 'query'
    │   ├── save.py        # python3 .hex/memory/save.py 'content' --tags 'x'
    │   └── index.py       # python3 .hex/memory/index.py  (bulk index markdown)
    ├── landings/           # Daily context snapshots (what your agent reads first)
    ├── evolution/          # Self-improvement logs
    └── standing-orders/    # Behavioral rules across sessions
```

**How your agent uses it:**
1. Agent starts a session → reads `CLAUDE.md` or `AGENTS.md`
2. These files instruct it to **search memory before guessing**
3. Agent runs `python3 .hex/memory/search.py 'topic'` → gets ranked results via FTS5
4. Agent saves new discoveries with `python3 .hex/memory/save.py 'what it learned'`
5. Next session, those memories are there. Your agent gets smarter over time.

## Why hex?

### vs. just using CLAUDE.md / AGENTS.md alone

Those files give your agent instructions but not *memory*. They're static. hex adds a searchable, growing database that your agent reads and writes to. Instructions + memory > instructions alone.

### vs. mem0

mem0 is a full memory platform — API keys, cloud service, pip/npm dependencies, LLM calls to extract memories. hex is the opposite: zero dependencies, runs locally, no API keys, no cloud, no LLM needed. If you want a managed service with entity extraction and multi-user support, use mem0. If you want something you can `cp` into any project in 10 seconds and never think about dependencies, use hex.

### vs. rolling your own

You could build this in an afternoon. But you'd also need to wire up FTS5, write the save/search/index scripts, create the agent instruction files, handle idempotent setup, design the landing/standing-order system, and keep it all working across agents. hex is that afternoon, packaged.

## Features

- **Zero dependencies** — Python 3.8+ standard library only. No pip install, no npm, no API keys.
- **Works with any agent** — Claude Code (`CLAUDE.md`), Codex/Cursor/Gemini CLI/Aider (`AGENTS.md`), or anything that reads files and runs shell commands.
- **Full-text search** — SQLite FTS5 with BM25 ranking. Sub-millisecond queries over thousands of memories.
- **Incremental indexing** — `index.py` hashes content chunks and only re-indexes changed files.
- **Git-friendly** — Memory DB is gitignored (local). Config and instructions are committed. Team members get the structure; memories stay personal.
- **Idempotent** — Run `setup.sh` as many times as you want. It never clobbers existing data.
- **Self-improving** — Landings give context, standing orders enforce discipline, evolution tracks what works. Your agent gets better the longer you use it.

## Compatible agents

| Agent | Config file | Status |
|-------|------------|--------|
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | `CLAUDE.md` | ✅ Works |
| [OpenAI Codex](https://openai.com/index/codex/) | `AGENTS.md` | ✅ Works |
| [Cursor](https://cursor.sh/) | `AGENTS.md` | ✅ Works |
| [Aider](https://aider.chat/) | `AGENTS.md` | ✅ Works |
| [Gemini CLI](https://github.com/google-gemini/gemini-cli) | `AGENTS.md` | ✅ Works |

Any agent that can read files and run `python3` commands will work.

## Memory commands

```bash
# Search memories (FTS5 full-text search with BM25 ranking)
python3 .hex/memory/search.py 'authentication middleware'
python3 .hex/memory/search.py 'auth' --top 5 --compact

# Save a memory
python3 .hex/memory/save.py 'JWT refresh tokens stored in httpOnly cookies' \
  --tags 'auth,security' --source 'src/middleware/auth.ts'

# Bulk index all markdown files into memory
python3 .hex/memory/index.py
```

### Memory schema

| Field | Description |
|-------|-------------|
| `content` | The memory text (searchable via FTS5) |
| `tags` | Comma-separated tags for filtering |
| `source` | Origin file or context |
| `timestamp` | ISO 8601 creation time |

## The three layers

hex is built on three ideas that compound over time:

**🎯 Landings** — Every session starts with a context snapshot. What's in progress, what's blocked, what was decided. Your agent reads this first and skips the re-discovery phase. Landings have priority tiers: L1 (critical) → L4 (background).

**📏 Standing orders** — Persistent rules your agent follows: "Search memory before guessing." "Save discoveries immediately." "Verify before asserting." These build discipline that persists across sessions.

**🔄 Evolution** — Your agent logs friction patterns and proposes improvements. Repeated mistakes become standing orders. The system literally improves itself.

## Troubleshooting

**"no such module: fts5"** — Your Python was compiled without FTS5 support. This is rare but happens on minimal Linux installations (Alpine, some Docker images). Fix: install `python3` from your distro's main repo (not the minimal package), or build Python with `--enable-loadable-sqlite-extensions`.

**setup.sh creates directories but no DB** — Make sure Python 3.8+ is available as `python3`. Run `python3 --version` to check.

**Agent doesn't search memory** — Verify `CLAUDE.md` (for Claude Code) or `AGENTS.md` (for Codex/Cursor) is in your project root. These files contain the instructions that teach your agent to use the memory system.

## Contributing

hex is intentionally simple. PRs that add external dependencies will be closed. PRs that make the memory smarter, the standing orders better, or the evolution loop more useful are welcome.

## License

MIT — see [LICENSE](LICENSE).

---

<p align="center">
  <sub>Built by <a href="https://github.com/mrap">@mrap</a> · If hex saves you time, <a href="https://github.com/mrap/hex-hermes">⭐ star the repo</a></sub>
</p>
