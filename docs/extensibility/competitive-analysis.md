# Hex Extensibility — Competitive Analysis

> Synthesized from research in `docs/extensibility/research/` (2026-04-27).
> Covers: Hermes, Claude Code plugins, NanoClaw, Codex, Cursor, Aider, VS Code, MCP.

---

## 1. Comparative Matrix

| System | Extension types | Discovery | Manifest format | Sandboxing | Upgrade safety | UI extensibility | Event system |
|--------|----------------|-----------|-----------------|------------|----------------|------------------|--------------|
| **Hermes** | Skills (Markdown), Tools (Python), Plugins (Python packages), Shell hooks | Dir scan of `~/.hermes/skills/`, `~/.hermes/plugins/`; Hub registry (`hermes skills install`) | `plugin.yaml` + SKILL.md frontmatter | None — runs as user process | `~/.hermes/` never overwritten; bundled content tracked separately | None (terminal/messaging only) | 6 lifecycle hooks (session, LLM, tool); shell hooks via config |
| **Claude Code Plugins** | Skills, Commands, Agents, Hooks, MCP servers, LSP servers, Monitors, Themes | `--plugin-dir` flag / config; workspace `.claude/skills/`; plugin `plugin.json` | `plugin.json` (JSON); SKILL.md (Markdown + YAML) | None — shell hooks run as user | Plugins live outside core binary; versioned `dependencies` field | None | Pre/PostToolUse, Stop, SubagentStop, Notification |
| **NanoClaw** | Skills (4 types: Feature/Utility/Operational/Container), no runtime hooks | Git branch merge (`skill/<name>`); upstream marketplace (PR) | SKILL.md frontmatter; git branch as implicit manifest | N/A — instruction-only | Git merge explicit; user fork never touched by upstream | None | Layer 1 only (skill next-steps instructions) |
| **OpenClaw** | Tools, Providers, Channels, Hooks, Commands, Speech/Voice/Fetch providers; Bundle compat for Claude/Codex/Cursor | ClawHub registry; npm install; bundle auto-detect | `openclaw.plugin.json` (JSON) + npm; bundle compat layer | In-process Node.js (no OS sandbox) | Config schema validation before load; versioned manifests | Channel plugins (messaging UIs only) | Gateway hooks + Claude CC hooks + webhook triggers |
| **Codex** | Hooks, AGENTS.md instructions, custom subagent configs, MCP servers | Config file scan (`~/.codex/`, `.codex/`); MCP server list | `config.toml` (TOML); agent `.toml` files; AGENTS.md | Sandbox modes per session (read-only, none) | `~/.codex/` and `.codex/` never overwritten; binary versioned separately | None | PreToolUse, PostToolUse, SessionStart, SessionStop, UserPromptSubmit (mergeable across layers) |
| **Cursor** | Rules (.mdc), Custom instructions (global), MCP servers, VS Code extensions (subset) | `.cursor/rules/*.mdc` dir scan; Cursor extension marketplace; MCP config | `.mdc` (Markdown + YAML frontmatter); `.cursorrules` (legacy) | None for rules; extension host for VS Code extensions | `.cursor/rules/` is user-owned, version-controlled with repo | Sidebar panels, custom views via VS Code extension API | None (rules inject context; no runtime hooks) |
| **Aider** | Config files, model aliases, custom system prompts, repo-specific settings, watch scripts | `.aider.conf.yml` in home/repo root; `CONVENTIONS.md`; `watch_files` globs | `.aider.conf.yml` (YAML); `aider.model.metadata.json` | None | Config in user home/repo; binary separate; no extension model | None | `--watch-files` (poll); pre/post script hooks (planned) |
| **VS Code** | 60+ contribution points: commands, views, menus, languages, grammars, themes, debuggers, auth providers, chat participants | marketplace.visualstudio.com; `extensionDependencies`; Extension Packs | `package.json` (npm manifest + `contributes` key) | Extension Host process isolation (separate Node.js process) | `engines.vscode` semver range enforced at load; deprecated APIs kept for multiple releases | Full: sidebar views, panels, webviews, custom editors, menus | Activation events (lazy load); no runtime event bus |
| **MCP** | Tools (actions), Resources (data reads), Prompts (templates) | Host-configured server list; dynamic `tools/list_changed` notifications | JSON-RPC 2.0 capability negotiation; per-tool JSON Schema | Process isolation (stdio subprocess or HTTP server) | Capability negotiation; forward-compat by design; version field in initialize | None (no UI primitives) | `notifications/tools/list_changed`; no event bus |

---

## 2. Strengths and Weaknesses

### Hermes
**Strengths:**
- Three-tier model (skills → tools → plugins) is the clearest "what tier do I need?" guidance of any system.
- `~/.hermes/` / repo split is clean, binary, enforced.
- Plugin `ctx` API is narrow: five methods cover 90% of use cases.
- Bundled plugins ship disabled — no surprise activation on upgrade.
- Hub registry with taps (custom registries) is the right architecture for enterprises.

**Weaknesses:**
- No UI extensibility. Built for terminal/messaging; web views are out of scope.
- No SSE or event bus — hooks are sync lifecycle events only.
- No sandboxing — plugins run as the user.
- Python-only tool tier locks out other languages.

### Claude Code Plugins
**Strengths:**
- `plugin.json` manifest is comprehensive but not overwhelming.
- Skill = Markdown is the right zero-code escape hatch.
- Hook system covers the right events at the right granularity.
- MCP integration means plugins can add new tools without code in the plugin itself.
- `bin/` on PATH is elegant for CLI command extensions.

**Weaknesses:**
- No sandboxing — shell hooks run arbitrary commands as user.
- No UI extensibility.
- No official marketplace — discovery is ad hoc.
- Skill relevance matching is opaque (when does a skill load?).

### NanoClaw
**Strengths:**
- Branch-per-feature is elegant: git is the package manager, merge is install, conflict is the upgrade UX.
- Zero runtime complexity — no daemon, no registry, no hooks to fire.
- Instruction-only skills are maximally portable across LLM runtimes.

**Weaknesses:**
- Doesn't scale past ~20 installed features (merge conflicts compound).
- No hooks, no events, no programmatic capability.
- Not suitable for system-level extensions (daemons, background workers).
- Each installation is a manual fork — no centralized update path.

### OpenClaw
**Strengths:**
- Multi-ecosystem bundle compatibility is unique — reads Claude Code, Codex, Cursor formats.
- ClawHub registry with semantic search and moderation is the most mature discovery solution.
- Gateway vs. agent scoping distinction is architecturally correct.
- Three hook layers degrade gracefully across runtimes.

**Weaknesses:**
- In-process Node.js is not sandboxed — a malicious plugin has full system access.
- Complexity: three manifest formats, three hook layers, multiple registration surfaces.
- No UI views beyond messaging channels.

### Codex
**Strengths:**
- Layered config (global → project → agent) with merge semantics for hooks is the best config inheritance model.
- Hooks that never crash the session (errors are non-blocking) is the right default.
- Codex-as-MCP-server is a powerful orchestration primitive.
- Per-agent sandbox modes are ahead of every other system.

**Weaknesses:**
- No plugin packaging — hooks are just scripts, no bundling/versioning.
- Hooks are behind a feature flag (not on by default).
- No UI extensibility.
- No discovery/listing of installed "extensions" (they're just config).

### Cursor
**Strengths:**
- Four rule activation modes (always, AI-decided, glob-scoped, manual) cover every use case elegantly.
- `.cursor/rules/` is version-controlled alongside the repo — the right default.
- Rules can stack in subdirectories — project-level overrides scoped to a subtree.

**Weaknesses:**
- Closed-source IDE — extensibility is limited by Cursor's product decisions.
- Rules inject context but cannot intercept or modify behavior.
- No event system.
- VS Code extension compatibility is partial and shrinking as Cursor diverges.

### Aider
**Strengths:**
- Single YAML config with sensible defaults is easy to adopt.
- `CONVENTIONS.md` / `AGENTS.md` pattern for per-repo instructions is portable.
- Model metadata is user-overridable — good for private/custom models.

**Weaknesses:**
- Aider has essentially no extension model. Config file customization is the ceiling.
- No hooks, no plugins, no events.
- Upgrade safety is "don't check in `.aider/`" — informal, not enforced.

### VS Code
**Strengths:**
- Extension Host process isolation is the gold standard for stability — extensions crash, the editor doesn't.
- Contribution points are declarative and statically analyzable — no code runs at discovery time.
- Lazy activation events mean 50,000 extensions coexist without startup cost.
- `engines.vscode` semver range is the simplest possible upgrade safety mechanism.
- 60+ contribution point types cover every UI surface systematically.

**Weaknesses:**
- No permission model — extensions run as full Node.js processes, can read/write anything.
- Marketplace security relies on publisher trust (DNS verification) not capability enforcement.
- Web views are sandboxed iframes but require more code than simpler alternatives.
- `*` activation event is widely abused.

### MCP
**Strengths:**
- Protocol-based: any language/runtime can implement a server. No lock-in.
- Three primitives (tools, resources, prompts) are simple, composable, and sufficient.
- Capability negotiation at connect time — forward and backward compatible by design.
- Process isolation via stdio transport.
- Dynamic tool discovery via `tools/list_changed` notification.
- 5,800+ servers — the ecosystem is real and growing.

**Weaknesses:**
- No UI primitives. Pure programmatic extension.
- Schema versioning has been unstable — breaking changes in the registry schema.
- Stateless bias — persistent/background extensions require extra engineering.
- No concept of "extension manifest" beyond the server's capability response.

---

## 3. What Hex Should Steal from Each

### From Hermes
- **Three-tier extension model** (skills → integrations → extensions/plugins) with explicit "which tier do I need?" guidance.
- **`~/.hex/` as absolute user space** — anything in `~/.hex/` is never touched by `hex upgrade`.
- **`ctx` pattern for plugin registration** — a single narrow API surface: `ctx.register_tool()`, `ctx.register_hook()`, `ctx.register_sse_topic()`, `ctx.register_view()`.
- **Bundled content ships disabled** — users opt-in, upgrades don't auto-enable.

### From Claude Code Plugins
- **`extension.yaml` manifest** — a single declarative file listing all extension surfaces before any code runs.
- **SKILL.md format** — Markdown + YAML frontmatter for zero-code behavioral extensions.
- **Hook system** — `PreToolUse`, `PostToolUse`, `Stop` pattern plus hex-specific hooks (`OnHexEvent`, `OnAssetChange`, `OnSSETopic`).
- **`bin/` on PATH** — extensions ship CLI tools without modifying the hex binary.
- **MCP as the code execution boundary** — when an extension needs real capability, it's an MCP server.

### From NanoClaw
- **Branch-per-feature for upgrade safety** — consider shipping hex's own optional features on `feature/*` branches, documented as "merge to enable."
- **Skills-over-features discipline** — the extension model should bias toward instruction/policy extensions over binary modifications.
- **Four skill weight tiers** — operational (instructions), utility (self-contained scripts), integration (config + service), full extension (code + manifest).

### From OpenClaw
- **Bundle compatibility** — hex's manifest format should be parseable by other tools; consider explicitly reading Claude Code plugin manifests.
- **Three hook layers** — design hooks so skills that add `Next Steps` instructions degrade gracefully when the hex runtime doesn't support the full hook system.
- **Registry with semantic versioning + moderation** — if hex gets a public extension ecosystem, build ClawHub-like infrastructure from day one, not after.

### From Codex
- **Layered config inheritance** (global → project → agent) with **merge semantics for hooks** — enterprise + project hooks should coexist, not override.
- **Non-blocking hook failures** — extension errors should never crash the hex server.
- **Per-agent sandbox modes** — extensions should declare their required permission tier.
- **Expose hex itself as an MCP server** — `hex mcp-server` would allow orchestration from Claude Code, VS Code, and other hosts.

### From Cursor
- **Four rule activation modes** (always, AI-decided, file-scoped, manual) — apply the same pattern to hex skills.
- **Subdirectory-scoped rules** — skills in `subdir/.hex/skills/` apply only to that subtree.
- **Version-controlled extension config** — hex extension config should live in `.hex/` alongside project files, checked into git.

### From VS Code
- **Declarative contribution points** — extensions declare what they add (SSE topics, views, CLI commands, event policies) in the manifest; hex validates without executing code.
- **Lazy activation events** — extensions declare when they should be loaded (e.g., `onHexEvent:asset.uploaded`); never loaded until triggered.
- **`engines.hex` semver range** — extensions declare compatible hex versions; hex refuses to load incompatible extensions.
- **Extension Host isolation** — extension code runs in a subprocess; the hex server stays stable if an extension crashes.

### From MCP
- **Make hex an MCP host** — hex should connect to MCP servers as a first-class integration mechanism.
- **Make hex expose an MCP server** — hex primitives (events, assets, messages, SSE) exposed as MCP tools/resources.
- **Dynamic capability discovery** — hex should support `hex://` URIs for resources and `notifications/events/list_changed` for dynamic event topics.
- **Capability negotiation** — hex extensions should declare which hex capabilities they need; hex refuses to load extensions that require unavailable capabilities.

---

## 4. Gap Analysis — Hex vs. Best-in-Class

| Capability | Best-in-class example | Hex today | Priority |
|------------|----------------------|-----------|----------|
| **Extension manifest format** | VS Code `package.json` / Claude Code `plugin.json` | None | 🔴 Critical |
| **Zero-code skill system** | Hermes SKILL.md / Claude Code skills | `CLAUDE.md` sections (no discovery) | 🔴 Critical |
| **Hook system (tool interception)** | Claude Code hooks / Codex hooks | None | 🔴 Critical |
| **Core vs. user space boundary** | Hermes `~/.hermes/` / Codex `~/.codex/` | None — everything mixed | 🔴 Critical |
| **MCP host (consume MCP servers)** | Claude Code, Codex, Cursor | None | 🔴 Critical |
| **Extension discovery** | VS Code marketplace / ClawHub / Hermes Hub | None | 🟠 High |
| **CLI command extensions** | Claude Code `bin/` / Codex agent TOML | None | 🟠 High |
| **Event policy extensions** | Hermes plugin hooks / Codex hooks | `~/.hex-events/policies/` (undocumented) | 🟠 High |
| **SSE topic extensions** | None (gap everywhere) | None | 🟠 High |
| **Upgrade safety rules** | NanoClaw git branches / VS Code `engines` | None formal | 🟠 High |
| **UI view extensions** | VS Code contribution points (views) | None | 🟡 Medium |
| **Extension sandboxing** | VS Code Extension Host / MCP stdio | None | 🟡 Medium |
| **Layered config inheritance** | Codex global/project/agent TOML | None | 🟡 Medium |
| **Hex as MCP server** | Codex `codex mcp-server` | None | 🟡 Medium |
| **Extension registry** | VS Code Marketplace / ClawHub | None | 🟢 Later |
| **Cross-ecosystem bundle compat** | OpenClaw bundle formats | None | 🟢 Later |
| **Per-agent capability scopes** | Codex sandbox modes | None | 🟢 Later |

**Summary:** Hex has zero formal extensibility infrastructure today. The four critical gaps (manifest, skills, hooks, core/user boundary) block everything else. MCP host support is critical because it's the ecosystem standard; building it now avoids lock-in.

---

## 5. Recommended Adoption Sequence

```
Phase 1 — Foundation (blocks everything)
  1a. Define core vs. user space file tree (where is user zone?)
  1b. Introduce extension.yaml manifest format
  1c. Formalize SKILL.md system in .hex/skills/

Phase 2 — Hooks + Events
  2a. Implement hook system (OnHexEvent, OnAssetChange, OnSSETopic + lifecycle)
  2b. Implement event policy extensions (replace ~/.hex-events/policies/ with manifested policies)
  2c. Non-blocking hook failure model

Phase 3 — MCP Integration
  3a. Hex as MCP host (consume external MCP servers)
  3b. Hex as MCP server (expose events, assets, messages as tools/resources)

Phase 4 — UI + CLI
  4a. SSE topic extensions
  4b. Custom view extensions (hex server proxies/serves user-defined views)
  4c. CLI command extensions (bin/ on PATH, `hex <cmd>` dispatch)

Phase 5 — Ecosystem
  5a. Upgrade safety enforcement (engines.hex semver, protected file tree)
  5b. Extension registry / discovery
  5c. Extension Host sandboxing
```

---

*Next: `proposal.md` will specify the concrete hex extensibility architecture derived from this analysis.*
