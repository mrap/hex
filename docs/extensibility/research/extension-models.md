# Extension Models Research: VS Code + MCP

> **Task:** t-4 — Gold standard extensibility models for hex to learn from.
> **Focus:** What makes these models successful? What should hex adopt? What should it avoid?

---

## 1. VS Code Extension Model

### Overview

VS Code has 50,000+ extensions and ~70% developer tool market share. The extension ecosystem is arguably VS Code's most important competitive moat — the editor itself is secondary to the network of extensions users accumulate.

### The Manifest: `package.json`

Every VS Code extension is declared via a standard `package.json`. There is no separate manifest format — VS Code reuses the npm manifest with additional fields. Key extension-specific fields:

```json
{
  "name": "my-extension",
  "displayName": "My Extension",
  "publisher": "my-publisher",
  "version": "1.0.0",
  "engines": { "vscode": "^1.105.0" },
  "categories": ["Programming Languages", "Snippets"],
  "keywords": ["productivity", "tools"],
  "activationEvents": ["onLanguage:python", "onCommand:myExt.doThing"],
  "contributes": {
    "commands": [...],
    "views": [...],
    "menus": [...],
    "languages": [...],
    "themes": [...]
  },
  "main": "./out/extension.js"
}
```

Key design decisions:
- **`engines.vscode`**: Semver range declaring compatibility. VS Code refuses to load extensions that don't match. This is how upgrade safety is enforced.
- **`publisher`**: Namespace for unique extension identity (`publisher.name`). Publishers are verified via domain DNS TXT record.
- **`categories`**: Used for Marketplace grouping and discovery.

### Contribution Points

Contribution points are **declarative JSON registrations** inside `contributes`. They are the primary extension mechanism — extensions don't patch core code, they declare what they add:

| Contribution Point | What it registers |
|---|---|
| `contributes.commands` | New commands in Command Palette |
| `contributes.menus` | Menu items (right-click, toolbar, title bar) |
| `contributes.views` | Panels in sidebar/activity bar |
| `contributes.viewsContainers` | New sidebar sections |
| `contributes.languages` | New language identifiers |
| `contributes.grammars` | Syntax highlighting |
| `contributes.themes` | Color themes |
| `contributes.snippets` | Code snippets |
| `contributes.configuration` | Settings schema (user-configurable) |
| `contributes.keybindings` | Keyboard shortcuts |
| `contributes.debuggers` | Debug adapters |
| `contributes.taskDefinitions` | Custom task types |
| `contributes.authentication` | Auth providers |
| `contributes.customEditors` | Custom file editors |
| `contributes.chatParticipants` | Copilot Chat participants |

**Key insight**: Contribution points are purely declarative. VS Code reads them at load time without executing any extension code. This enables fast startup, discovery without activation, and static analysis of what an extension does before the user installs it.

### Activation Events

Extensions are loaded **lazily** — VS Code does not load all extensions at startup. Instead, extensions declare when they should be activated:

```json
"activationEvents": [
  "onLanguage:python",           // When a Python file opens
  "onCommand:myExt.doThing",     // When user invokes the command
  "workspaceContains:.myconfig", // When workspace has this file
  "onView:myView",               // When user opens this panel
  "onStartupFinished",           // After all * extensions finish
  "*"                            // Always (bad practice — use sparingly)
]
```

**Key insight**: Lazy activation is critical for performance at scale. With 50,000 extensions, loading them all at startup would be catastrophic. The activation event pattern means only relevant extensions run.

For hex: extensions that are never triggered by the user's workflow consume zero resources.

### Extension Host: Process Isolation

The most important architectural decision in VS Code's extension model: **all extensions run in a separate Node.js process** called the Extension Host.

```
┌─────────────────────────────────────┐
│  VS Code Main Process (Renderer)    │
│  - Electron / Chromium              │
│  - DOM, UI, window management       │
│  - No extension code runs here      │
└──────────────┬──────────────────────┘
               │ RPC (structured protocol)
               │ No shared memory
               │ Serialized messages
┌──────────────▼──────────────────────┐
│  Extension Host Process             │
│  - Separate Node.js process         │
│  - All extensions run here          │
│  - Own V8 engine, own memory heap   │
│  - Can crash without affecting UI   │
└─────────────────────────────────────┘
```

Properties of extension host isolation:
- Extensions **cannot access the DOM** directly
- Extensions **cannot import Electron APIs**
- All UI interactions go through the `vscode` API (proxy objects)
- When the Extension Host crashes, VS Code restarts it — the editor UI is unaffected
- Extensions are "prisoners" of the API surface — powerful but bounded

The API surface (`extHost.protocol.ts`) is a single authoritative contract defining `MainThread*Shape` (extension calls into UI) and `ExtHost*Shape` (UI calls into extensions). 60+ service pairs cover every API surface.

**What hex should steal**: The idea that extensions can't destabilize the host. For hex, this means extension code runs in isolated processes or sandboxed contexts, with a defined API for accessing core primitives.

**What VS Code got wrong / should avoid**:
- No true permission system. Extensions run as full Node.js processes — they can read files, make network requests, etc. This is the primary security criticism.
- Marketplace security is weak: publisher verification via DNS, virus scan on upload, but no runtime permission enforcement. Researchers found malicious extensions.
- The `*` activation event is abused — many extensions activate on every startup.

### Marketplace & Discovery

- Central registry at marketplace.visualstudio.com
- Publisher verification via domain DNS TXT record
- Extensions tagged with categories (20 categories) + keywords (30 max)
- Extensions can depend on other extensions (`extensionDependencies`)
- **Extension Packs**: Bundle multiple extensions together
- Rating system, download counts for social proof

**Key insight**: Discovery infrastructure matters as much as the extension API itself. Nobody installs an extension they can't find.

### Upgrade Safety

VS Code's approach:
1. `"engines": {"vscode": "^1.x"}` — extensions declare compatible VS Code versions
2. VS Code silently refuses to load incompatible extensions
3. Core VS Code APIs are versioned and stable for long periods
4. Deprecated APIs are announced early and kept for multiple releases
5. VS Code never ships user configuration files inside extension directories

**Key insight**: The "engines" field is the entire upgrade safety mechanism. It's simple, well-understood, and works at scale.

---

## 2. MCP (Model Context Protocol)

### Overview

MCP is a vendor-neutral, open protocol (released by Anthropic, Nov 2024) that standardizes how LLM applications connect to external tools and data. As of early 2026: 5,800+ MCP servers, 300+ clients. It has become the de facto standard for AI tool extensibility.

The design was inspired by LSP (Language Server Protocol), which unified how editors add language support. MCP applies the same pattern to AI tool use.

### Architecture

```
┌─────────────────────┐
│  Host                │  (LLM application: Claude Desktop, VS Code, hex)
│  ┌───────────────┐  │
│  │  MCP Client   │  │  (manages connections to one or more servers)
│  └───────┬───────┘  │
└──────────┼──────────┘
           │ JSON-RPC 2.0
           │ (Stdio or Streamable HTTP)
┌──────────▼──────────┐
│  MCP Server          │  (exposes tools, resources, prompts)
│  - file system       │
│  - database          │
│  - API wrapper       │
│  - custom logic      │
└─────────────────────┘
```

### Three Primitives

MCP defines exactly three things a server can expose:

| Primitive | Direction | Purpose | Who decides when to use |
|---|---|---|---|
| **Tools** | Client → Server (call) | Actions the model can execute | The model (AI agent) |
| **Resources** | Server → Client (read) | Data/context the model can read | The user or model |
| **Prompts** | Server → Client (template) | Reusable prompt templates | The user |

**Tools** are the primary primitive for hex's use case. A tool has:
```json
{
  "name": "hex_event_emit",
  "description": "Emit a hex event to the SSE bus",
  "inputSchema": {
    "type": "object",
    "properties": {
      "topic": {"type": "string"},
      "payload": {"type": "object"}
    },
    "required": ["topic"]
  }
}
```

**Resources** are addressable by URI, support MIME types, and can be subscribed to for live updates:
```
hex://events/recent          → Last 50 events (JSON)
hex://assets/myfile.png      → Binary asset
hex://messages/inbox         → Message list
```

**Prompts** are server-side template definitions that generate context-rich prompts.

### Lifecycle

1. **Initialize**: Client sends `initialize` with protocol version + capabilities
2. **Negotiate**: Server responds with its capabilities (what primitives it supports)
3. **Discover**: Client lists available tools/resources/prompts
4. **Use**: Client calls tools, reads resources, gets prompts
5. **Update**: Server sends `notifications/tools/list_changed` when tools change dynamically
6. **Disconnect**: Either side can terminate

Capability negotiation at connect time means clients handle missing capabilities gracefully.

### Transport Options

- **Stdio**: Server is a subprocess, communication via stdin/stdout. Simple, no port conflicts, used for local tools.
- **Streamable HTTP**: HTTP + streaming. Better for remote servers, multiple concurrent clients, production deployments. Replaced SSE-only transport.

### Dynamic Tool Discovery

MCP supports dynamic toolset management — servers can register/unregister tools at runtime and notify clients via `notifications/tools/list_changed`. This enables:
- Context-aware tools (different tools available in different workspaces)
- Lazy tool loading (don't expose tools until relevant)
- Feature flags on tool availability

### What Makes MCP Successful

1. **Protocol-based, not API-based**: Any language/runtime can implement MCP. Python, TypeScript, Go, Rust — all work identically. No language lock-in.
2. **Three clean primitives**: The tool/resource/prompt trichotomy is simple enough to explain in 30 seconds and powerful enough to cover most use cases.
3. **Capability negotiation**: Clients don't crash when servers lack a feature. This is critical for forward compatibility.
4. **Transport flexibility**: Local (stdio) and remote (HTTP) use the same protocol.
5. **Composability**: Multiple MCP servers can be connected simultaneously. A host can connect to a filesystem server, a database server, and a custom business logic server at once.
6. **LSP heritage**: MCP borrowed LSP's battle-tested lifecycle and registration patterns.

### What to Avoid / Limitations

1. **Schema versioning instability**: MCP's registry schema changed from v0 to v1 in ways that broke clients (the `version` field location moved). This caused real pain. Lesson: version your schemas explicitly and maintain compatibility windows.
2. **No UI primitives**: MCP has no way for a server to register a UI view, a menu item, or a settings panel. It's purely programmatic. For UI extensibility, you need something more like VS Code's contribution points.
3. **Stateless bias**: Tools are designed as stateless function calls. Stateful extensions (persistent connections, background workers) need extra engineering.
4. **No discovery infrastructure**: There's no MCP equivalent of the VS Code Marketplace. Discovery relies on GitHub search and community lists.
5. **Security model immature**: Tool execution happens with the permissions of the host process. There's no sandboxing, no capability declarations, no permission prompts (though some clients add these themselves).

---

## 3. Synthesis: What Hex Should Adopt

### From VS Code

| Pattern | Hex application |
|---|---|
| `package.json`-style manifest | `extension.yaml` with `name`, `version`, `hex` compat range, `contributes`, `activationConditions` |
| Contribution points (declarative) | Declare what an extension adds without loading code: `contributes.commands`, `contributes.views`, `contributes.eventPolicies`, `contributes.sseTopics` |
| Activation events (lazy loading) | Only load extension code when relevant: `onEvent:topic`, `onCommand:name`, `onStartup` |
| Engine version pinning | `hex: ">=0.8.0 <1.0.0"` in extension manifest; hex refuses incompatible extensions |
| Process isolation | Extension code runs in subprocess or WASM sandbox; can crash without affecting hex core |
| Extension packs | Bundle related extensions (e.g., "my-workspace-bundle" includes event policies + UI views + agent behaviors) |

### From MCP

| Pattern | Hex application |
|---|---|
| Three clean primitives | Map to hex: Tools (event emitters/actions), Resources (events/assets/messages as URIs), Prompts (agent charters) |
| Capability negotiation | Extensions declare what they need (`needs: [events, messaging, assets]`); hex grants or denies at load time |
| Transport flexibility | Extensions can be local (subprocess) or remote (HTTP proxy) — same manifest, different transport |
| Dynamic discovery | Extensions can register new SSE topics or CLI commands at runtime via notification protocol |
| Single-responsibility | Each extension does one thing; compose via multiple extensions, not mega-extensions |
| Stdio for local | Agent behaviors / event policy scripts run as subprocesses communicating via stdio |

### Patterns to Avoid

| Anti-pattern | Why |
|---|---|
| Full OS access for extensions | Malicious or buggy extensions can destroy user data; use capability declarations |
| `*` activation (always-on) | Kills performance at scale; require explicit activation conditions |
| No discovery infrastructure | Extensions nobody finds are useless; build discovery into `hex extension search` from day one |
| Unstable schema changes | Version schemas explicitly; maintain compatibility windows (e.g., 2 release deprecation period) |
| UI primitives via MCP | MCP has no UI model; hex's UI extensibility needs contribution-point style declarations separate from tool calls |
| Extensions modifying core files | The entire point of extensibility is to keep user customizations out of core-managed files |

---

## 4. Key Design Insights for Hex

### The Two-Manifest Pattern

VS Code uses `package.json` for static declarations + `activate()` for runtime behavior. MCP uses capability negotiation + tool handler registration. Hex should combine both:

1. **Static manifest** (`extension.yaml`): declarative, read without loading code, used for discovery and compatibility checking
2. **Runtime registration** (subprocess stdio or HTTP): dynamic capability announcement after process start

### The Activation Condition Pattern

Don't load extensions unless they're needed. For hex:
- `onEvent:hex.agent.woke` → load agent behavior extensions
- `onRequest:GET /myview` → load UI view extension
- `onCommand:hex mycommand` → load CLI command extension
- `onStartup` → load extensions that need background workers

### The Capability Declaration Pattern

Extensions should declare what core primitives they need:
```yaml
needs:
  - events.read
  - events.write
  - messaging.send
  - assets.read
```

Hex grants only declared capabilities. This enables:
- Security auditing ("what can this extension do?")
- Sandboxing (only pass relevant APIs to the process)
- Future permission prompts if needed

### The Upgrade Contract

The cleanest pattern from VS Code: the `engines` field. Hex should require:
```yaml
hex: ">=0.8.0 <1.0.0"
```

On `hex upgrade`, the hex binary checks all installed extensions for compatibility. If an extension declares `hex: "<0.8.0"`, hex warns before upgrade and either disables the extension or runs in compatibility mode.

---

## References

- VS Code Extension API: https://code.visualstudio.com/api/references/contribution-points
- VS Code Activation Events: https://code.visualstudio.com/api/references/activation-events
- VS Code Extension Host: https://code.visualstudio.com/api/advanced-topics/extension-host
- VS Code Extension Host Architecture (DeepWiki): https://deepwiki.com/microsoft/vscode/3.1-multi-process-architecture
- MCP Specification (2025-11-25): https://modelcontextprotocol.io/specification/2025-11-25
- MCP Server Development Guide: https://github.com/cyanheads/model-context-protocol-resources
- VS Code Extension Ecosystem Analysis (arxiv 2024): https://arxiv.org/html/2411.07479v1
