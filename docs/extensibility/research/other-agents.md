# Codex, Cursor, Aider — Extensibility Research

**Researched:** 2026-04-27

---

## 1. OpenAI Codex CLI

**Sources:** developers.openai.com/codex, openai/codex GitHub

### Overview

Codex CLI is OpenAI's agentic coding assistant. Its extensibility model is TOML-first with layered configuration, a full hook system, custom agent definitions, and deep MCP integration.

---

### Configuration Layers

Codex uses a hierarchical config system — each layer overrides the one below:

| Layer | Path | Scope |
|-------|------|-------|
| Enterprise (managed) | `requirements.toml` (MDM-deployed) | Global, enforced |
| Global user | `~/.codex/config.toml` | All projects |
| Project | `.codex/config.toml` | Per-repo |
| Custom agent | `.codex/agents/<name>.toml` | Per-agent override |

Higher-precedence layers override individual keys; **hooks are merged** (not replaced) across layers. This matters: an enterprise hook and a project hook both fire.

**Key config.toml sections:**

```toml
# Project documentation (AGENTS.md equivalent)
project_doc_max_bytes = 32768
project_doc_fallback_filenames = ["TEAM_GUIDE.md"]

# Feature flags
[features]
codex_hooks = true
multi_agent = true
shell_tool = true
apps = false

# Agent definitions (multi-agent)
[agents]
max_threads = 4
max_depth = 2

[agents.reviewer]
description = "Find correctness, security, and test risks in code."
config_file = "./agents/reviewer.toml"

# Inline hook configuration
[hooks]
[[hooks.PreToolUse]]
matcher = "^Bash$"
[[hooks.PreToolUse.hooks]]
type = "command"
command = 'python3 "/absolute/path/to/pre_tool_use_policy.py"'
timeout = 30
statusMessage = "Checking Bash command"
```

---

### AGENTS.md — Instruction Chain

Codex reads instruction files before starting work. Discovery order (per-directory, walking down from repo root to CWD):

1. `AGENTS.override.md` — takes precedence if present
2. `AGENTS.md` — standard instructions
3. Fallback filenames (configured in `project_doc_fallback_filenames`)

Files are merged into a chain up to `project_doc_max_bytes` (32 KiB default). The global `~/.codex/AGENTS.md` provides user-level defaults; project files layer on top.

**What AGENTS.md controls:** build commands, code conventions, workflow rules, agent behavior, what tools to avoid — anything you'd tell a new teammate.

---

### Hook System

Hooks are the primary programmatic extensibility mechanism. Enabled via feature flag:

```toml
[features]
codex_hooks = true
```

**Hook sources** (all merge, none replace):
- `~/.codex/hooks.json` or inline in `~/.codex/config.toml`
- `.codex/hooks.json` or inline in `.codex/config.toml`
- Enterprise `requirements.toml` (MDM-managed)

**Supported events:**

| Event | What `matcher` filters | Notes |
|-------|------------------------|-------|
| `PreToolUse` | tool name (regex) | Can block or modify tool calls |
| `PostToolUse` | tool name (regex) | Observe results, run follow-up |
| `SessionStart` | start source | Setup, auth checks |
| `SessionStop` | — | Cleanup, notifications |
| `UserPromptSubmit` | — | Intercept/augment user input |

**Hook execution protocol:**
- Codex spawns the hook command as a subprocess
- Passes a JSON payload on stdin
- Reads a structured JSON response from stdout
- Hook outcomes: `proceed`, `block` (with message), `modify` (substitute input)
- Failures are non-blocking — hook errors never crash the session
- `Modify` is blocked for `LocalShell` (security guard)

**Example hook response:**
```json
{"outcome": "block", "message": "This command touches production files."}
{"outcome": "modify", "content": "git status"}
{"outcome": "proceed"}
```

**Matcher regex examples:**
- `"^Bash$"` — only the Bash tool
- `"^mcp:.*"` — any MCP tool
- `""` or `"*"` — every occurrence

---

### Custom Agents (Subagents)

Codex supports multi-agent hierarchies. Custom agents are TOML files that override session config:

**File locations:**
- `~/.codex/agents/<name>.toml` — personal/global agents
- `.codex/agents/<name>.toml` — project-scoped agents

**Agent manifest fields:**

```toml
# .codex/agents/reviewer.toml
name = "reviewer"
description = "Find correctness, security, and test risks in code."
developer_instructions = "You review code for bugs, security issues, and missing tests..."
nickname_candidates = ["Athena", "Ada"]

# Inherits from parent session when omitted:
model = "o4-mini"
model_reasoning_effort = "high"
sandbox_mode = "read-only"
mcp_servers = [...]
```

Agents can define their own sub-agents in their config file, enabling a tree hierarchy. Per-agent tool access control is a planned feature (whitelist/blacklist per agent).

---

### MCP Integration

MCP servers are first-class citizens in Codex config. Defined per-session or per-agent in `config.toml`. The `skill_mcp_dependency_install` feature flag enables auto-installing MCP dependencies.

Codex itself can be exposed **as an MCP server** via `codex mcp-server`, exposing `codex()` (start session) and `codex-reply()` (continue session) tools — enabling orchestration from the OpenAI Agents SDK.

---

### Upgrade Safety

- User config lives in `~/.codex/` and `.codex/` — never overwritten by Codex CLI upgrades
- `AGENTS.md` is user-owned, version-controlled with the repo
- The Codex binary is upgraded separately; config format is versioned
- No explicit migration tooling documented

---

### What Codex Does Well

- **Layered config** — global → project → agent, with clear override semantics
- **Mergeable hooks** — enterprise + project hooks coexist without conflict
- **Multi-agent hierarchy** — agents can delegate to sub-agents
- **MCP as both consumer and provider** — Codex integrates MCP tools AND exposes itself as MCP
- **Sandbox modes** — clear security boundaries per session/agent

### What Codex Lacks

- No UI extensibility
- No plugin marketplace (hooks are scripts, not packaged extensions)
- Limited discovery — no `codex extension list` command
- Hooks require a feature flag (not on by default)

---

## 2. Cursor IDE

**Sources:** cursor.com/learn, cursor.com/docs, cursor-alternatives.com

### Overview

Cursor is a closed-source AI IDE built on VS Code. Its extensibility model is rules-first (persistent context injection), augmented by a plugin system and deep MCP integration. Users can customize behavior without touching Cursor's internals.

---

### Rules System

Rules are the core extensibility primitive — Markdown files that inject persistent context into every AI request.

**File format:** `.mdc` (Markdown + YAML frontmatter)

```markdown
---
description: Python coding guidelines for this project
globs: **/*.py
alwaysApply: false
---

# Python Coding Guidelines

- Use type hints for all function parameters and returns
- Follow PEP 8 style guide
- Prefer dataclasses over plain dicts for structured data
```

**Directory structure:**

```
PROJECT_ROOT/
├── .cursor/
│   └── rules/
│       ├── code-style.mdc          # alwaysApply: true
│       ├── testing.mdc             # globs: **/*.test.ts
│       ├── api-guidelines.mdc      # glob-scoped
│       └── deploy-workflow.mdc     # manual @mention
├── .cursorrules                    # Legacy (deprecated, still works)
└── src/
    └── .cursor/
        └── rules/
            └── component.mdc       # Scoped to src/ subtree
```

**Four activation modes:**

| Mode | Frontmatter | When applied |
|------|-------------|--------------|
| Always Apply | `alwaysApply: true` | Every session |
| Agent Decides | `alwaysApply: false` + `description` | AI picks based on relevance |
| Glob-Scoped | `globs: "src/api/**/*.ts"` | When matching files are in context |
| Manual | No frontmatter / `alwaysApply: false` without description | `@rule-name` mention only |

**Hierarchy (highest to lowest priority):**
1. Global rules — Cursor Settings → Rules for AI (personal, not version-controlled)
2. Project workspace rules — `.cursor/rules/*.mdc`
3. Subdirectory rules — `subdir/.cursor/rules/*.mdc`

---

### Skills

Skills are on-demand specialized knowledge documents the AI can invoke when relevant. Distinct from rules (rules are always-included; skills are pulled in as needed). Skills use the same `.mdc` format but are listed in the "Agent Decides" section of settings. Users invoke them manually with `/skill-name` in chat.

---

### Plugin System

Cursor 0.43+ introduced a full plugin system:

**Plugin manifest:** `.cursor-plugin/plugin.json`

**Plugin components:**

| Component | Description |
|-----------|-------------|
| **Rules** | Persistent AI guidance — `.mdc` files |
| **Skills** | Specialized agent capabilities, on-demand |
| **Agents** | Custom agent configurations and prompts |
| **Commands** | Agent-executable command files |
| **MCP Servers** | Model Context Protocol integrations |
| **Hooks** | Automation scripts triggered by events |

**Plugin distribution:**
- Packaged as Git repositories
- Official Marketplace: `cursor.com/marketplace` (manually reviewed by Cursor team)
- Community directory: `cursor.directory`
- Install by pointing to a Git repo URL

**No scripting required** — plugins are declarative (rules, skills, configs) or use external processes (hooks, MCP servers). The plugin itself is not compiled code.

---

### MCP Integration

MCP is the primary mechanism for adding external tools. Any MCP server can be added to Cursor — standard protocol, works with any compliant server. The agent can also run any CLI tool directly without MCP (e.g., `gh`, `kubectl`, `docker`).

---

### Upgrade Safety

- `.cursor/rules/` and `.cursorrules` are user-owned, version-controlled
- Cursor (closed-source binary) upgrades independently of user config
- Rules survive upgrades because they're just files in the repo
- No upgrade migration needed — plain Markdown never breaks
- Global settings (Settings → Rules for AI) are stored in Cursor's app data, not the repo

---

### What Cursor Does Well

- **Dead simple rules format** — plain Markdown with YAML frontmatter; anyone can write one
- **Granular activation** — always, glob-matched, AI-decides, or manual
- **Plugin system with marketplace** — discoverable, shareable, version-controlled
- **MCP-first for tool extensibility** — clean separation of concerns
- **Subdirectory scoping** — rules can apply only to a subtree

### What Cursor Lacks

- No server-side extensibility (closed-source binary; users can't add CLI subcommands)
- No sandboxing for plugins/hooks
- No version compatibility declarations (a plugin doesn't say "requires Cursor ≥ 0.43")
- Marketplace requires Cursor team review — community extensions live in a separate directory

---

## 3. Aider

**Sources:** aider.chat, Aider-AI/aider GitHub

### Overview

Aider is an open-source command-line AI coding assistant. Its extensibility model is minimal and config-file-first. There is no plugin system, no hook system, and no marketplace. Extensibility comes from: layered YAML config, model customization files, in-chat commands, and Python API usage.

---

### Configuration System

**File:** `.aider.conf.yml`

**Discovery order** (last loaded wins):
1. `~/.aider.conf.yml` — home directory (user global)
2. `<git-root>/.aider.conf.yml` — project root
3. `<cwd>/.aider.conf.yml` — current directory
4. `--config <filename>` — explicit override (loads only this one)

**Equivalent configuration methods:**

```yaml
# .aider.conf.yml
dark-mode: true
model: claude-sonnet-4-6
weak-model: claude-haiku-4-5
auto-commits: false
lint-cmd: make lint
test-cmd: make test
```

```bash
# Command-line flag
aider --dark-mode --model claude-sonnet-4-6

# Environment variable
export AIDER_DARK_MODE=true
export AIDER_MODEL=claude-sonnet-4-6
```

**Key configurable areas:**

| Category | Config keys |
|----------|-------------|
| Model selection | `model`, `weak-model`, `editor-model` |
| Git behavior | `auto-commits`, `commit-prompt`, `dirty-commits` |
| Lint/test | `lint-cmd`, `test-cmd`, `auto-lint`, `auto-test` |
| Context | `read`, `map-tokens`, `map-refresh` |
| Editor | `editor` (for `/editor` command) |
| API keys | `openai-api-key`, `anthropic-api-key` (or `.env`) |

---

### Model Customization

Aider supports custom model definitions for models it doesn't know about:

**`.aider.model.settings.yml`** — override model behavior settings:

```yaml
- name: my-custom-model
  edit_format: diff
  weak_model_name: claude-haiku-4-5
  use_repo_map: true
  cache_control: true
  streaming: true
  reasoning_tag: thinking
  extra_params:
    max_tokens: 8192
```

**`.aider.model.metadata.json`** — define context window and cost:

```json
{
  "my-custom-model": {
    "max_input_tokens": 128000,
    "max_output_tokens": 8192,
    "input_cost_per_token": 0.000003,
    "output_cost_per_token": 0.000015
  }
}
```

**Model aliases** — create shorthand names:

```yaml
# .aider.conf.yml
alias:
  - "fast:claude-haiku-4-5"
  - "smart:claude-opus-4-7"
```

---

### In-Chat Commands

Aider provides a rich set of `/` commands for runtime extensibility:

| Command | Purpose |
|---------|---------|
| `/add <file>` | Add file to context |
| `/model <name>` | Switch main model mid-session |
| `/architect` | Enter architect/editor mode with two models |
| `/test` | Run test command, add failures to chat |
| `/lint` | Run lint command, add issues to chat |
| `/web <url>` | Scrape URL, add as context |
| `/save <file>` | Save session reconstruction script |
| `/settings` | Print current configuration |
| `/reasoning-effort` | Adjust reasoning budget |
| `/think-tokens` | Set thinking token budget |

These are not extensible — users cannot add custom `/commands` beyond what Aider ships.

---

### Python API (Programmatic Use)

Aider can be used as a Python library for scripted workflows:

```python
from aider.coders import Coder
from aider.models import Model
from aider.io import InputOutput

io = InputOutput(yes=True)
model = Model("claude-sonnet-4-6")
coder = Coder.create(main_model=model, io=io, fnames=["src/app.py"])
coder.run("Add error handling to the main function")
```

This is the primary extension point for automation. No hooks, no callbacks — just scripted invocations.

---

### No Hook System

Aider has no hook system. There is no way to inject behavior at tool-use time, session start, or post-edit. The closest workaround is the `lint-cmd` and `test-cmd` config keys, which run external commands after edits and feed failures back into the chat.

---

### No Plugin System

Aider has no plugin system, no extension manifest format, and no marketplace. All capabilities are baked into the binary. Users who need custom behavior must:
1. Fork Aider and add their feature (Python, open source)
2. Wrap Aider in a shell script
3. Use Aider as a Python library

---

### Upgrade Safety

- `.aider.conf.yml`, `.aider.model.settings.yml`, `.aider.model.metadata.json` are user-owned and never overwritten by upgrades
- Aider is a Python package (`pip install aider-chat`) — upgrades replace the package, not the project config files
- No migration tooling — config format is stable; breaking changes are rare and documented in release notes
- The repo itself (`.git`, source files) is never touched by an upgrade

---

### What Aider Does Well

- **Zero-friction config** — YAML file in the repo root, no learning curve
- **Layered overrides** — global defaults + project overrides + per-run flags
- **Model portability** — register any model via metadata files; swap models instantly
- **Open source** — users can fork and extend at the Python level
- **Lean default** — no plugin system means no plugin security surface

### What Aider Lacks

- **No hook system** — cannot intercept or react to tool use
- **No plugin system** — cannot package and share extensions
- **No custom commands** — `/commands` are fixed; users cannot add new ones
- **No UI** — no server, no views, no SSE
- **Single-layer project config** — no per-directory overrides within a repo

---

## Cross-System Comparison

| Dimension | Codex | Cursor | Aider |
|-----------|-------|--------|-------|
| Config format | TOML (layered) | MDC/YAML + JSON | YAML (layered) |
| Instruction files | AGENTS.md chain | Rules (.mdc) | None |
| Hook system | Yes (feature flag, TOML) | Yes (via plugin) | No |
| Plugin system | No (hooks + agents) | Yes (plugin.json) | No |
| Custom agents | Yes (TOML files) | Yes (in plugin) | No |
| MCP support | Yes (consumer + provider) | Yes (consumer) | No |
| Marketplace | No | Yes (official + community) | No |
| UI extensibility | No | No | No |
| Upgrade safety | Config in `~/.codex/` | Config in `.cursor/` | Config in project root |
| Open source | Yes (Rust) | No | Yes (Python) |
| Version compat. declarations | No | No | No |

---

## Key Insights for Hex

1. **AGENTS.md / CLAUDE.md pattern is industry standard** — Codex and Claude Code both use layered instruction files. Hex should formalize this rather than fight it.

2. **Hooks are the minimal extension point** — Codex proves that a hook system (JSON stdin/stdout, regex matcher, lifecycle events) is sufficient for most "extensibility without forking" use cases. Hex already needs hooks for event routing; the same mechanism can serve as the extension primitive.

3. **TOML > JSON for config** — Both Codex and VS Code (via package.json) use structured config files. Cursor uses YAML frontmatter in Markdown. Hex's existing YAML (integrations) is fine; don't switch formats.

4. **Plugin = manifest + components** — Cursor's plugin model (plugin.json + rules/skills/agents/commands/hooks/MCP) is the right abstraction. Hex could adopt: `hex-extension.yaml` + policies/skills/agents/views/commands.

5. **Granular activation matters** — Cursor's four rule modes (always, glob, agent-decides, manual) show that not all extensions should fire all the time. Hex extensions should declare their trigger conditions.

6. **Upgrade safety is a file-tree problem** — All three tools solve it the same way: user config lives in well-known paths that the tool binary never touches. Hex needs a documented "never overwrite" zone.

7. **No one has solved UI extensibility** — None of these tools (Codex, Cursor, Aider) provide a model for custom UI views plugging into a server. This is a genuine differentiator hex can own.
