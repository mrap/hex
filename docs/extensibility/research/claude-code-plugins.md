# Claude Code Plugins + NanoClaw / OpenClaw — Extensibility Research

**Researched:** 2026-04-27
**Sources:** Context7 (Claude Code docs), Exa web search, NanoClaw docs, OpenClaw docs

---

## Part 1: Claude Code Official Plugin System

### Overview

Claude Code's plugin system is a **file-system-based, manifest-driven extension model**. Plugins are directories with a `plugin.json` manifest (at `.claude-plugin/plugin.json`) that declare paths to skills, commands, agents, hooks, MCP servers, and more.

### Plugin Manifest: `plugin.json`

```json
{
  "name": "plugin-name",
  "version": "1.2.0",
  "description": "Brief plugin description",
  "author": {
    "name": "Author Name",
    "email": "author@example.com",
    "url": "https://github.com/author"
  },
  "homepage": "https://docs.example.com/plugin",
  "repository": "https://github.com/author/plugin",
  "license": "MIT",
  "keywords": ["keyword1", "keyword2"],
  "skills": "./custom/skills/",
  "commands": ["./custom/commands/special.md"],
  "agents": "./custom/agents/",
  "hooks": "./config/hooks.json",
  "mcpServers": "./mcp-config.json",
  "outputStyles": "./styles/",
  "themes": "./themes/",
  "lspServers": "./.lsp.json",
  "monitors": "./monitors.json",
  "dependencies": [
    "helper-lib",
    { "name": "secrets-vault", "version": "~2.1.0" }
  ]
}
```

**Key fields:**
- `skills` — directory of `SKILL.md` files (loaded contextually when relevant)
- `commands` — flat `.md` files or directories loaded as slash commands
- `agents` — sub-agent definitions (markdown with YAML frontmatter)
- `hooks` — path to `hooks.json` specifying event handlers
- `mcpServers` — path to `.mcp.json` for MCP server definitions
- `lspServers` — path to `.lsp.json` for language server definitions
- `monitors` — background monitors that run during sessions
- `bin/` — executables added to PATH (callable from Bash tool)
- `dependencies` — other plugins this plugin requires

### Standard Plugin Directory Structure

```
my-plugin/
├── .claude-plugin/           # Metadata (optional — can also be at root)
│   └── plugin.json           # plugin manifest
├── skills/                   # Skill directories
│   ├── code-reviewer/
│   │   └── SKILL.md
│   └── pdf-processor/
│       ├── SKILL.md
│       └── scripts/
├── commands/                 # Slash commands as flat .md files
│   ├── status.md
│   └── logs.md
├── agents/                   # Subagent definitions
│   ├── security-reviewer.md
│   └── performance-tester.md
├── output-styles/
│   └── terse.md
├── themes/
│   └── dracula.json
├── monitors/
│   └── monitors.json
├── hooks/
│   └── hooks.json            # Event hook configuration
├── bin/                      # Executables on PATH
│   └── my-tool
├── settings.json             # Default plugin settings
├── .mcp.json                 # MCP server definitions
├── .lsp.json                 # LSP server configs
└── scripts/                  # Hook and utility scripts
```

### Skill Discovery and Loading

Skills are **Markdown files** (`SKILL.md`) with YAML frontmatter. Claude Code loads them contextually — a skill is shown to the model when its `description` and `triggers` match what the user is doing. Skills are NOT always loaded; they're surfaced on-demand.

Typical `SKILL.md` frontmatter:
```yaml
---
name: my-skill
description: Brief description (used for relevance matching)
version: 1.0.0
triggers:
  - keyword or phrase that activates this skill
requires:
  tools: [web_search]
  config:
    - key: my.setting
      description: What this does
      default: value
---

# Skill Title
Step-by-step instructions Claude follows.
```

**Discovery path precedence** (high to low):
1. Agent-level skills (defined per agent)
2. Workspace skills (`.claude/skills/`)
3. Plugin skills (from plugin manifest)
4. Bundled / system skills

### Hook System

Hooks intercept Claude Code events and run shell commands, LLM prompts, or MCP tool calls. Configured in `hooks.json`.

**Hook event types:**
| Event | When it fires |
|-------|--------------|
| `PreToolUse` | Before a tool executes — can allow, deny, or modify |
| `PostToolUse` | After a tool completes — can add context, trigger side effects |
| `Stop` | When Claude wants to finish — can force continuation |
| `SubagentStop` | When a sub-agent finishes |
| `Notification` | When Claude sends a status notification |

**Hook matcher:** regex pattern matching tool names (e.g. `"Write|Edit"`, `"Bash"`, `""` = all tools).

**Hook types:**
- `command` — run a shell script; input arrives on stdin as JSON
- `prompt` — call Claude to evaluate a condition; returns `{ok: true}` or `{ok: false, reason: "..."}`
- `mcp_tool` — invoke an MCP server tool directly

**Example hooks.json:**
```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          { "type": "command", "command": "jq -r '.tool_input.file_path' | xargs npx prettier --write" }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "prompt",
            "prompt": "Evaluate whether all user tasks are complete. Return {\"ok\": true} or {\"ok\": false, \"reason\": \"...\"}",
            "timeout": 30
          }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "osascript -e 'display notification \"Claude needs attention\"'" }
        ]
      }
    ]
  }
}
```

**Hook input (stdin):** JSON with `hook_event_name`, `tool_name`, `tool_input`, `tool_response` (for PostToolUse).
**Hook output (stdout):** JSON with `permissionDecision` (PreToolUse) or `additionalContext` (PostToolUse).

### Agent Definitions

Agents are markdown files in the `agents/` directory with YAML frontmatter describing:
- Name, description, triggers
- Allowed tools, forbidden tools
- System prompt / persona
- When to be invoked (auto-dispatch rules)

Agents are sub-agents that Claude Code can spawn for specialized tasks.

### MCP Server Integration

Plugins declare MCP servers in `.mcp.json` or via the manifest's `mcpServers` field:

```json
{
  "mcpServers": {
    "github": {
      "type": "http",
      "url": "https://api.githubcopilot.com/mcp/"
    },
    "database": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@bytebase/dbhub"],
      "env": { "DSN": "${DATABASE_URL}" }
    }
  }
}
```

MCP servers provide tools, resources, and prompts to Claude. They are a first-class extension point.

### What Plugins Can Access

Claude Code plugins operate at the **prompt/context injection level**, not at the binary level:
- They inject skills (instructions) into Claude's context
- They register hooks that intercept tool calls (shell-level interception)
- They provide MCP servers that expose new tools
- They add CLI commands (via commands/ .md files)
- They add agents (sub-agents Claude can delegate to)
- They cannot modify the core binary behavior
- They cannot directly access Claude Code's internal state — only via hooks and MCP

**Upgrade safety:** Plugins live outside Claude Code's core. `claude` binary upgrades don't touch plugin directories. Plugin compatibility is managed via `dependencies` versioning.

---

## Part 2: NanoClaw

**Repository:** `qwibitai/nanoclaw` (~27K stars)
**Philosophy:** "Skills over features" — users fork and customize, core stays minimal (~43.7k tokens)

### Core Philosophy

NanoClaw is explicitly NOT a monolithic framework. Users fork the repo and apply skills to customize their fork. Every installation is bespoke — no unused features, no config sprawl. Contributions to core are limited to bug fixes and simplifications; new features must be skills.

### Four Skill Types

| Type | Location | How it works |
|------|----------|-------------|
| **Feature** | `.claude/skills/` + `skill/*` branch | SKILL.md in marketplace; code on git branch; applying = `git merge` |
| **Utility** | `.claude/skills/<name>/` with code files | Self-contained; code lives alongside SKILL.md; no branch needed |
| **Operational** | `.claude/skills/<name>/SKILL.md` on main | Instruction-only workflows (setup, debug, update); no code changes |
| **Container** | `container/skills/` | Loaded in agent containers at runtime; teach container agents |

### Feature Skill Distribution (Branch-per-Skill)

This is NanoClaw's most distinctive pattern. Feature skills live on named branches:

```
main              — core NanoClaw (no skill code)
skill/discord     — main + Discord integration
skill/telegram    — main + Telegram integration
skill/slack       — main + Slack integration
skill/gmail       — main + Gmail integration
```

Applying a skill = `git merge skill/telegram`. Updating core = `git merge` from upstream `main`. Everything uses standard git operations. Claude Code resolves conflicts during merge.

**Advantages of branch-per-skill:**
- Zero risk of skill code bleeding into core
- Upgrading core preserves user customizations (merge conflicts are explicit)
- Clear audit trail of what's installed
- Skills can depend on other skills (branch ordering)
- No custom manifest format — git is the manifest

### Extension Contract

NanoClaw skills use the same SKILL.md format as Claude Code skills (YAML frontmatter + markdown instructions). The "extension contract" is:
1. Create a SKILL.md with instructions
2. Put code changes on a `skill/*` branch
3. Publish to the NanoClaw marketplace (PR with SKILL.md)

No runtime hooks, no binary API, no TypeScript SDK. Instructions + git.

---

## Part 3: OpenClaw

**Repository:** `openclaw/openclaw`
**Character:** Full-featured agent platform with runtime plugin SDK, compatible with Claude/Codex/Cursor plugin formats

### Plugin Architecture: Two Formats

| Format | Markers | How it works |
|--------|---------|-------------|
| **Native** | `openclaw.plugin.json` + runtime module | In-process Node.js; full SDK access |
| **Bundle** | `.claude-plugin/`, `.codex-plugin/`, default Claude/Cursor layout | Read-only mapping of supported content to native surfaces |

OpenClaw can install plugins from Claude Code, Codex, and Cursor ecosystems — treating them as "bundles" and mapping their skills, hooks, and MCP config into native OpenClaw features.

### Native Plugin Manifest (`openclaw.plugin.json`)

```json
{
  "id": "my-plugin",
  "configSchema": { ... },
  "skills": ["./skills/"],
  "cliBackends": ["my-backend"],
  "contracts": {
    "webSearch": true,
    "imageGeneration": false
  }
}
```

### Plugin SDK Registration

Native plugins export a `definePluginEntry` function that receives an `api` object:

```typescript
definePluginEntry({
  register(api) {
    api.registerTool(...);        // New agent tools
    api.registerProvider(...);    // Model/media providers (LLMs, image gen, TTS)
    api.registerChannel(...);     // Messaging channels (Slack, Telegram, Discord)
    api.registerHook(...);        // Event hooks (gateway-level)
    api.registerCommand(...);     // Custom slash commands
    api.registerSpeechProvider(...);
    api.registerRealtimeVoiceProvider(...);
    api.registerWebFetchProvider(...);
    api.registerContextEngine(...);
  }
});
```

**Key distinction from Claude Code plugins:** OpenClaw plugins operate at the **gateway/runtime level** — they affect all agents on that gateway, not per-session. Per-agent scoping uses skill allowlists.

### Hook System (Three Layers)

OpenClaw skills support a layered hook approach for maximum portability:

| Layer | Mechanism | Where it works |
|-------|-----------|----------------|
| Layer 1 | SKILL.md Next Steps instructions | Every agent (Claude Code, Codex, OpenClaw, any) |
| Layer 2 | Claude Code hooks in SKILL.md frontmatter or scripts/ | Claude Code + Codex |
| Layer 3 | OpenClaw Gateway hooks (agent:bootstrap, command:new) | OpenClaw only |

**Three hook systems in native plugins:**
1. **Claude Code/Codex hooks** — PreToolUse, PostToolUse, Stop (same format)
2. **OpenClaw Gateway hooks** — native runtime events
3. **Webhook hooks** — HTTP triggers for external integrations (e.g., linear-webhook, fieldy-ai-webhook)

### ClawHub Registry

ClawHub (`clawhub.ai`) is OpenClaw's public skills/plugin registry:
- 5,705 total skills (Feb 2026), 3,002 curated after moderation
- Vector search (semantic, not just keyword)
- Semantic versioning with changelogs
- Moderation hooks for approvals/audits
- CLI-friendly API for automation

```bash
openclaw skills install my-skill   # Install a skill
openclaw plugins install my-plugin # Install a plugin
openclaw skills list               # List installed skills
```

### Bundle Compatibility Matrix

OpenClaw maps bundle content from other ecosystems:

| Feature | How it maps | Applies to |
|---------|-------------|------------|
| Skill content | Bundle skill roots → OpenClaw skills | All formats |
| Commands | `commands/`, `.cursor/commands/` → skill roots | Claude, Cursor |
| Hook packs | OpenClaw-style HOOK.md + handler.ts | Codex |
| MCP tools | Bundle MCP config merged into embedded settings | All formats |
| LSP servers | Claude `.lsp.json` → embedded LSP defaults | Claude |
| Settings | Claude `settings.json` → embedded defaults | Claude |

---

## Part 4: Extensibility Model Comparison

| Dimension | Claude Code Plugins | NanoClaw | OpenClaw |
|-----------|--------------------:|--------:|--------:|
| **Manifest format** | `plugin.json` (JSON) | Git branch + SKILL.md | `openclaw.plugin.json` (JSON) + npm |
| **Code execution** | Hooks (shell/LLM/MCP) | None (git + instructions) | In-process Node.js SDK |
| **Discovery** | `--plugin-dir` flag / config | Git branch merge | ClawHub registry + npm |
| **Skill format** | SKILL.md + YAML frontmatter | SKILL.md + YAML frontmatter | SKILL.md + YAML frontmatter |
| **Hook surface** | Pre/PostToolUse, Stop, Notification | None (Layer 1 only) | Gateway hooks + Claude hooks |
| **Sandboxing** | None — shell hooks run as user | N/A | Plugin runs in-process as Node.js |
| **Upgrade safety** | Plugins outside core; versioned deps | Git merge resolves conflicts | Config schema validation before load |
| **UI extensibility** | None | None | Channel plugins (messaging UIs) |
| **Marketplace** | No official marketplace | NanoClaw upstream repo | ClawHub registry |
| **Cross-ecosystem** | No | No | Yes — reads Claude/Codex/Cursor bundles |

---

## Part 5: Key Patterns for Hex

### What to steal from Claude Code plugins

1. **Manifest-first discovery** — `plugin.json` declares all extension points before code runs. Hex should validate extension manifests without executing extension code.
2. **Skill = Markdown + frontmatter** — zero-code extensibility for knowledge/workflow injections. Most users want to add instructions, not write Rust.
3. **Hook matchers** — regex-filtered hooks are expressive and simple. `"Write|Edit"` is clearer than complex event routing.
4. **`bin/` directory on PATH** — plugins can ship executables without modifying the host binary. Hex could analogize this for CLI command extensions.
5. **MCP as the code boundary** — when plugins need real capability (not just instructions), MCP servers are the clean interface. They're sandboxed, protocol-defined, and replaceable.

### What to steal from NanoClaw

1. **Branch-per-feature for upgrade safety** — the cleanest solution to "how do I upgrade without clobbering user customizations." Git is the package manager.
2. **Skills over features** — keeping core minimal and pushing complexity to user-owned extensions is a forcing function for good architecture.
3. **Four skill types by code-weight** — operational (instructions only), utility (self-contained scripts), feature (git branch), container (runtime injection). Hex's extension types should have similar weight tiers.

### What to steal from OpenClaw

1. **Bundle compatibility** — reading Claude Code and Codex plugin formats. Hex doesn't need to invent its own format if it can consume existing ecosystems.
2. **Three-layer hook portability** — design hooks so skills degrade gracefully on platforms without full hook support.
3. **Gateway vs. agent scoping** — some extensions (channels, providers) affect the whole system; others (skills, hooks) scope per-agent. Hex should make this distinction explicit.
4. **Registry with semantic versioning** — ClawHub's moderation + vector search is the right model for a mature extension ecosystem.

### Gap analysis for Hex

| Capability | Claude Code | NanoClaw | OpenClaw | Hex today |
|------------|:-----------:|:--------:|:--------:|:---------:|
| Plugin manifest format | ✅ | Partial (SKILL.md) | ✅ | ❌ |
| Skill system | ✅ | ✅ | ✅ | Partial (CLAUDE.md) |
| Hook system | ✅ | ❌ | ✅ | ❌ |
| MCP integration | ✅ | ❌ | ✅ | ❌ |
| CLI command extensions | ✅ | ❌ | ✅ | ❌ |
| UI extensibility | ❌ | ❌ | Partial (channels) | ❌ |
| Upgrade-safe user zone | Partial | ✅ (git) | Partial | ❌ |
| Extension registry | ❌ | Partial | ✅ (ClawHub) | ❌ |
| Cross-ecosystem bundles | ❌ | ❌ | ✅ | ❌ |

**Hex's biggest gaps:** no manifest format, no hook system, no clear core/user boundary, no extension discovery.
