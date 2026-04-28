# Hermes Agent — Extensibility Research

**Source:** NousResearch/hermes-agent (GitHub, ~25K stars)  
**Researched:** 2026-04-27

---

## Overview

Hermes Agent (by NousResearch) is an open-source self-improving AI agent that runs continuously on user-owned infrastructure. It supports multiple LLM backends, messaging platforms (Telegram, Discord, Slack, WhatsApp), and has a sophisticated extensibility model.

---

## Extension Model: Three Tiers

Hermes uses three distinct extension mechanisms with clear boundary separation:

### Tier 1: Skills (No-code, Markdown)

Skills are on-demand knowledge documents the agent loads when relevant. They are **Markdown files** (SKILL.md) with YAML frontmatter — no Python required.

**Manifest format:**
```yaml
---
name: my-skill
description: Brief description
version: 1.0.0
author: Your Name
license: MIT
platforms: [macos, linux]        # Optional; omit = all platforms
tags: [Category, Sub-category]
requires_skills: [other-skill]   # Optional dependencies
requires_toolsets: [web]         # Only show when these toolsets active
requires_tools: [web_search]     # Only show when these tools exist
fallback_for_toolsets: [browser] # Hide when these toolsets active
config:                          # Optional — settings the skill needs
  - key: my.setting
    description: What this setting does
    default: sensible-default
    prompt: Display prompt for setup
---

# Skill Title

Step-by-step instructions the agent follows.

## Pitfalls
Known failure modes and how to handle them.

## Verification
How the agent confirms it worked.
```

**Discovery locations (priority order):**
1. `~/.hermes/skills/` — primary, agent-writable, source of truth
2. External dirs configured in `~/.hermes/config.yaml` — read-only
3. Plugin-bundled skills — namespaced as `plugin:skill-name`, read-only

**Discovery mechanics:**
- Bundled skills copied from repo to `~/.hermes/skills/` on install
- Hub-installed skills downloaded to `~/.hermes/skills/`
- The agent can create/modify skills in `~/.hermes/skills/`
- External dirs: if same skill name exists in both, local version wins

**Upgrade safety:** Skills in `~/.hermes/skills/` are user-owned and never overwritten by `hermes update`. The agent distinguishes "bundled" vs "user-modified" state via a manifest at `~/.hermes/skills/.hub/`.

**Hub/Registry:** Skills can be installed from:
- `hermes skills install <name>` — from hub registry
- Direct URL: `hermes skills install https://example.com/my-skill.md`
- Custom taps: `hermes skills tap add myorg/skills-repo`

---

### Tier 2: Tools (Python, code-required)

Tools are Python modules that register with a central `ToolRegistry`. Required when the capability needs API keys, custom auth, binary data, or streaming.

**File structure:**
```
tools/
  your_tool.py     # handler, schema, check_fn, registry.register() call
toolsets.py        # add tool name to _HERMES_CORE_TOOLS or a toolset
```

**Auto-discovery:** Any `tools/*.py` file with a top-level `registry.register()` call is auto-discovered at startup — no manual import list.

**Registration pattern:**
```python
from tools.registry import registry

def handler(name, args, **kwargs) -> str:
    # Must return JSON strings, never raw dicts
    # Errors: return {"error": "message"}, never raise
    result = do_work(args)
    return json.dumps(result)

def check_fn() -> bool:
    # Return False to exclude tool (e.g., missing API key)
    return bool(os.getenv("MY_API_KEY"))

SCHEMA = {"type": "function", "function": {"name": "my_tool", ...}}

registry.register(
    name="my_tool",
    schema=SCHEMA,
    handler=handler,
    check_fn=check_fn,
)
```

**Toolsets:** Tools are grouped into toolsets (sets of related tools). `_HERMES_CORE_TOOLS` is always loaded; other toolsets are opt-in.

**State-aware tools:** Tools needing per-session agent state (todo, memory) are intercepted by `run_agent.py` before reaching the registry. Schema still registered so the model sees it; dispatch is handled by the agent.

---

### Tier 3: Plugins (Python, full integration)

Plugins are the most powerful extension type — full Python packages with lifecycle hooks, tool registration, skill bundling, and CLI command registration.

**Directory layout:**
```
plugins/
  my-plugin/
    __init__.py      # register() entry point
    plugin.yaml      # metadata
    tools.py         # tool implementations
    schemas.py       # tool schemas
    skills/          # bundled skills
      my-skill/
        SKILL.md
    README.md
```

**Plugin manifest (`plugin.yaml`):**
```yaml
name: my-plugin
version: 1.0.0
description: "Short description"
hooks:
  - on_session_end
  - post_tool_call
```

**Entry point (`register()`):**
```python
def register(ctx):
    # Register tools
    ctx.register_tool(
        name="calculate",
        toolset="calculator",
        schema=schemas.CALCULATE,
        handler=tools.calculate
    )
    # Register lifecycle hooks
    ctx.register_hook("post_tool_call", on_tool_called)
    # Register CLI subcommands
    ctx.register_cli_command("my-plugin", cmd_handler)
    # Register bundled skills (namespaced, read-only)
    for child in (Path(__file__).parent / "skills").iterdir():
        skill_md = child / "SKILL.md"
        if child.is_dir() and skill_md.exists():
            ctx.register_skill(child.name, skill_md)
```

**Discovery locations (priority order):**
1. `plugins/<name>/` — bundled (in-repo)
2. `~/.hermes/plugins/<name>/` — user-installed
3. `./hermes/plugins/<name>/` — project-local

**Lifecycle hooks available:**
- `on_session_start` / `on_session_end`
- `pre_llm_call` / `post_llm_call`
- `pre_tool_call` / `post_tool_call`

**Bundled vs user plugins:**
- Bundled plugins ship disabled; user must explicitly enable in `~/.hermes/config.yaml`
- User-installed plugins with the same name override bundled versions
- Bundled plugins never auto-enable on upgrade

---

## Hook System (Three Variants)

| System | Registered via | Scope | Use case |
|--------|---------------|-------|----------|
| Gateway hooks | `HOOK.yaml` + `handler.py` in `~/.hermes/hooks/` | Gateway only | Logging, alerts, webhooks |
| Plugin hooks | `ctx.register_hook()` in plugin `register()` | CLI + Gateway | Tool interception, metrics, guardrails |
| Shell hooks | `hooks:` block in `~/.hermes/config.yaml` | CLI + Gateway | Drop-in scripts, blocking, context injection |

**Shell hook config schema:**
```yaml
hooks:
  post_tool_call:
    - matcher: "<regex>"          # Optional; filters by tool name
      command: "<shell command>"  # Runs via shlex.split, shell=False
      timeout: 60                 # Default 60s, capped at 300s
```

**JSON wire protocol:** Shell hooks receive a JSON payload on stdin with `event_name`, `tool_name`, `tool_input`, and `extra` dict.

---

## Configuration vs Code

| Capability | Mechanism | Code required? |
|-----------|-----------|---------------|
| Instruction-based behaviors | Skills (SKILL.md) | No |
| New tools (API, binary, streaming) | Python tool module | Yes |
| Full integration (hooks + tools + CLI) | Plugin package | Yes |
| Platform-specific behavior | Shell hooks in config.yaml | Shell script only |
| Memory providers | Plugin with MemoryProvider interface | Yes |

**Key design principle:** "Make it a Skill when the capability can be expressed as instructions + shell commands + existing tools. Make it a Tool when it requires end-to-end Python integration."

---

## Upgrade Safety

- `hermes update` updates the source tree (core)
- `~/.hermes/` is user space — never overwritten
  - `~/.hermes/skills/` — user skills, agent-created skills
  - `~/.hermes/plugins/` — user-installed plugins
  - `~/.hermes/config.yaml` — user configuration
  - `~/.hermes/hooks/` — user gateway hooks
- Bundled content (in repo) is separate from user content
- User-modified bundled skills tracked via audit log; can be reset to bundled version with `hermes skills reset <name> --restore`

---

## MCP Integration

Hermes supports MCP (Model Context Protocol) for connecting to 6,000+ external apps. MCP servers are configured in `~/.hermes/config.yaml` and appear as tools in the agent's tool list.

---

## Key Takeaways for Hex

1. **Three-tier extension model** (skills → tools → plugins) with clear guidance on which tier to use — hex could adopt the same progression
2. **Markdown-first skills** with YAML frontmatter: zero code, highly portable, compatible with agentskills.io open standard
3. **`~/.hermes/` is user space, repo is core** — clean upgrade-safe separation
4. **Plugin context object (`ctx`)** as the single extension surface: register_tool, register_hook, register_cli_command, register_skill
5. **Plugin discovery by convention**: scan known directories for `__init__.py` + `plugin.yaml`, no manual registration
6. **Bundled plugins ship disabled** — user must opt-in, upgrades never auto-enable
7. **User override wins**: user-installed plugin of same name always overrides bundled version
8. **Three hook types** for different integration depths (gateway-only, cross-platform Python, shell scripts)

---

## Gaps vs Hex Needs

- Hermes has no native **UI extensibility** model (it's terminal/messaging focused)
- Hermes has no **SSE topic** extension concept
- Hermes has no **event policy** system beyond hooks
- Hermes's skill system is Claude/LLM-centric; hex needs skills for both agent behaviors AND automation policies
