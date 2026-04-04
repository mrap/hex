<h1 align="center">hex 🧠</h1>
<p align="center"><b>Your agent forgets everything. Every session. hex fixes that.</b></p>

<p align="center">
  <a href="https://github.com/mrap/hex/stargazers"><img src="https://img.shields.io/github/stars/mrap/hex?style=social" alt="GitHub Stars"></a>
  <a href="https://github.com/mrap/hex/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-lightgrey" alt="MIT License"></a>
  <img src="https://img.shields.io/badge/dependencies-zero-brightgreen" alt="Zero Dependencies">
  <img src="https://img.shields.io/badge/python-3.8+-blue" alt="Python 3.8+">
</p>

<p align="center">
  <img src="assets/demo.gif" alt="hex demo — setup in 60 seconds" width="700">
</p>

## The problem

Every new session with Claude Code, Codex, or Cursor is **Groundhog Day**. Your agent starts from zero. Re-asks your stack. Re-discovers your conventions. Makes the same mistakes you corrected yesterday. You spend the first 10 minutes re-explaining context instead of shipping code.

`CLAUDE.md` helps — until compaction kicks in and your rules get silently dropped.

## The fix

**hex gives your agent persistent memory, daily plans, behavioral rules, and a feedback loop — in one `cp` command, zero dependencies.**

```
Week 1 with hex:  47 friction events (re-asks, wrong assumptions, forgotten conventions)
Week 6 with hex:  12 friction events
```

It feels like someone who has been on your team for months.

## Quick start

**60 seconds. Zero dependencies. Python 3.8+ only.**

```bash
# From your project directory:
git clone https://github.com/mrap/hex.git /tmp/hex-setup
cp -r /tmp/hex-setup/{CLAUDE.md,AGENTS.md,setup.sh,.hex} .
bash setup.sh
rm -rf /tmp/hex-setup
```

That's it. Start your agent. It reads `CLAUDE.md` / `AGENTS.md` and knows how to use hex automatically.

### Verify it works

```bash
# Save your first memory
python3 .hex/memory/save.py 'Project uses Express with JWT auth, refresh tokens in httpOnly cookies' \
  --tags 'auth,architecture' --source 'initial-setup'

# Search it back (FTS5 full-text search with prefix matching)
python3 .hex/memory/search.py 'auth'
# → Project uses Express with JWT auth, refresh tokens in httpOnly cookies
```

## What you actually get

**Without hex** — every session starts cold:
```
You: Fix the auth middleware
Agent: What framework are you using?
You: Express, like I told you yesterday...
      (10 minutes of re-discovery before any real work)
```

**With hex** — your agent remembers:
```
You: Fix the auth middleware
Agent: [searches memory → finds Express + JWT + refresh tokens,
        reads today's landing → knows auth refactor is in progress]
Agent: Your auth middleware is in src/middleware/auth.ts using JWT with
       refresh tokens. Picking up where we left off...
       (starts working immediately)
```

## How it works

hex is plain files + SQLite. No servers, no API keys, no config.

```
your-project/
├── CLAUDE.md                    # Agent instructions (Claude Code reads this automatically)
├── AGENTS.md                    # Agent instructions (Codex, Cursor, Gemini CLI, Aider)
└── .hex/
    ├── memory/
    │   ├── memory.db            # SQLite FTS5 search — sub-ms queries, grows over time
    │   ├── search.py            # python3 .hex/memory/search.py 'query'
    │   ├── save.py              # python3 .hex/memory/save.py 'what you learned'
    │   └── index.py             # Bulk-index markdown files into memory
    ├── landings/                # Daily context snapshots ("here's what's in progress")
    │   └── TEMPLATE.md          #   Your agent reads this first — skips re-discovery
    ├── standing-orders/         # Persistent behavioral rules ("search before guessing")
    │   └── defaults.md          #   Survives compaction — enforced every session
    └── evolution/               # Friction log + feedback loop
        └── README.md            #   Repeated mistakes become permanent rules
```

**The four parts:**

| Part | What it does | Why it matters |
|------|-------------|----------------|
| **Memory** | Searchable database of everything your agent learns | No more re-asking your stack, conventions, or past decisions |
| **Landings** | Daily context snapshot (blockers, active work, open threads) | Agent picks up exactly where you left off — zero warm-up |
| **Standing orders** | Behavioral rules enforced every session | "Search before guessing" survives compaction, unlike CLAUDE.md rules |
| **Evolution** | Logs friction, turns repeated mistakes into new rules | The system gets better the longer you use it |

## hex vs. alternatives

| | **hex** | **claude-mem** | **CLAUDE.md alone** | **Roll your own** |
|---|---------|---------------|--------------------|--------------------|
| Setup time | 60 seconds | 5–15 min | 30 seconds | 4+ hours |
| Dependencies | **Zero** (stdlib only) | pip + API keys | None | Varies |
| Memory (searchable, persistent) | ✅ FTS5 | ✅ LLM-based | ❌ | Maybe |
| Daily planning | ✅ Landings | ❌ | ❌ | DIY |
| Behavioral rules | ✅ Standing orders | ❌ | ⚠️ Lost on compaction | DIY |
| Feedback loop | ✅ Evolution engine | ❌ | ❌ | DIY |
| Works offline | ✅ | ❌ Needs API | ✅ | Varies |
| Multi-agent support | ✅ CLAUDE.md + AGENTS.md | Claude only | Claude only | DIY |
| Cloud / API keys | **None** | Required | None | Varies |

**tl;dr** — claude-mem gives you memory. CLAUDE.md gives you rules. hex gives you memory + rules + planning + a feedback loop, in zero dependencies. It's the integrated system that makes the difference.

## Compatible agents

| Agent | Config file | Status |
|-------|------------|--------|
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | `CLAUDE.md` | ✅ Works |
| [OpenAI Codex](https://openai.com/index/codex/) | `AGENTS.md` | ✅ Works |
| [Cursor](https://cursor.sh/) | `AGENTS.md` | ✅ Works |
| [Aider](https://aider.chat/) | `AGENTS.md` | ✅ Works |
| [Gemini CLI](https://github.com/google-gemini/gemini-cli) | `AGENTS.md` | ✅ Works |

Any agent that can read markdown files and run `python3` commands will work.

## Commands

```bash
# Search memories (FTS5 full-text search with BM25 ranking + prefix matching)
python3 .hex/memory/search.py 'auth middleware'
python3 .hex/memory/search.py 'auth' --top 5 --compact

# Save a memory
python3 .hex/memory/save.py 'JWT refresh tokens stored in httpOnly cookies' \
  --tags 'auth,security' --source 'src/middleware/auth.ts'

# Bulk index all markdown files into memory
python3 .hex/memory/index.py
```

## Troubleshooting

**"no such module: fts5"** — Your Python was compiled without FTS5. Fix: install `python3` from your distro's main repo (not the minimal package).

**Agent doesn't search memory** — Make sure `CLAUDE.md` or `AGENTS.md` is in your project root. These files teach your agent to use hex.

## Contributing

hex is intentionally simple. PRs that add external dependencies will be closed. PRs that make memory smarter, planning better, or the feedback loop more useful are welcome.

```bash
# Use as a GitHub template
# Click "Use this template" on GitHub → clone → bash setup.sh
```

## License

MIT — see [LICENSE](LICENSE).

---

<p align="center">
  <sub>Built by <a href="https://github.com/mrap">@mrap</a> · If hex saves you from Groundhog Day, <a href="https://github.com/mrap/hex">⭐ star the repo</a></sub>
</p>
