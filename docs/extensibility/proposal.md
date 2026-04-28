# Hex Extensibility Architecture — Proposal

> Derived from competitive analysis in `docs/extensibility/competitive-analysis.md` (2026-04-27).
> This is the concrete design for how users extend hex without touching core.

---

## Executive Summary

Hex needs a two-zone file system, a declarative extension manifest (`extension.yaml`), seven
extension types, a subprocess-isolated extension host, and three concrete upgrade safety rules.
The design borrows the manifest discipline from VS Code, the user-space boundary from Hermes,
the hook system from Claude Code, and the layered config from Codex.

---

## 1. Core vs. User Space — File Tree

### Principle

`hex upgrade` is safe to run at any time. It must never overwrite user work. The boundary is
enforced by convention (documented below) and, eventually, by a checksum manifest (Phase 5).

### Zone Map

```
~/.hex/                          ← USER SPACE ROOT — never touched by hex upgrade
  extensions/                    ← installed extensions (hex extension install writes here)
    my-dashboard/
      extension.yaml
      views/
      skills/
  skills/                        ← user's freestanding skills (no extension bundle)
  policies/                      ← user's freestanding event policies
  audit/                         ← runtime logs (coordinator, errors, events)
  config.yaml                    ← global user config (hex upgrade never writes this)

<repo>/.hex/                     ← PROJECT USER SPACE — checked into git, never overwritten
  extensions/                    ← project-scoped extensions
  skills/                        ← project-scoped skills (override global)
  policies/                      ← project-scoped event policies
  config.yaml                    ← project-level config (layered over ~/.hex/config.yaml)

<repo>/system/                   ← CORE MANAGED — overwritten on hex upgrade
  policies/                      ← system event policies (shipped with hex)
  skills/                        ← system skills (shipped with hex)
  commands/                      ← system CLI commands (shipped with hex)
  hooks/                         ← system hook scripts (shipped with hex)
  harness/                       ← agent harness binary + config
  scripts/                       ← system scripts (shipped with hex)
  sse/                           ← SSE server config (shipped with hex)
  templates/                     ← core templates (shipped with hex)
  version.txt                    ← hex version stamp (used by upgrade safety check)

<repo>/projects/                 ← PROJECT AGENTS — user-owned, never overwritten
  <project-name>/
    charter.yaml                 ← agent charter (user-authored)
    ...

<repo>/templates/                ← USER TEMPLATES — never overwritten
  integrations/                  ← integration bundle templates (user can add)
  ...
```

### Upgrade Safety Rules (enforced)

1. **Never write to `~/.hex/`** — `hex upgrade` only writes to `<repo>/system/`. The user home
   directory is always user space.
2. **Never write to `<repo>/projects/` or `<repo>/templates/`** — these are user-authored and
   version-controlled alongside project code.
3. **Never write to `<repo>/.hex/`** — this directory does not exist in the shipped binary; it
   is created by users and is always user-managed.

### Discovery Priority (highest wins)

```
<repo>/.hex/extensions/  >  ~/.hex/extensions/  >  <repo>/system/
<repo>/.hex/skills/      >  ~/.hex/skills/       >  <repo>/system/skills/
<repo>/.hex/policies/    >  ~/.hex/policies/     >  <repo>/system/policies/
```

Project config overrides user config overrides system defaults — identical to Codex's layered
config model.

---

## 2. Extension Types

Seven extension types cover everything users need without touching the binary.

### 2.1 Skill Extension (`type: skill`)

**What it is:** An instruction file that changes how the hex agent behaves. Zero code.

**Format:** SKILL.md (Markdown + YAML frontmatter)

**Lives in:** `~/.hex/skills/<name>/SKILL.md` or `<repo>/.hex/skills/<name>/SKILL.md`

**Activation modes** (borrowed from Cursor's four modes):
- `always` — always injected into the agent's context
- `auto` — LLM decides based on relevance
- `glob` — only when working on matching file patterns (e.g., `src/**/*.rs`)
- `manual` — only when the user explicitly invokes `hex skill <name>`

**Example:**
```markdown
---
name: hex-debug-mode
version: "1.0.0"
type: skill
activation: glob
glob: "**/*.rs"
engines:
  hex: ">=0.8.0"
description: "Extra Rust debugging guidance"
---

When debugging Rust code, always check for borrow checker violations first...
```

---

### 2.2 Event Policy Extension (`type: policy`)

**What it is:** A YAML rule file that reacts to hex events (identical format to existing
`system/policies/*.yaml`, but placed in user space).

**Format:** Policy YAML (already established in `system/policies/`)

**Lives in:** `~/.hex/policies/<name>.yaml` or `<repo>/.hex/policies/<name>.yaml`

**Compatible with existing:** The existing `system/policies/` format is already the extension
format. This formalizes user-space policy placement.

**Additional fields for user policies:**
```yaml
name: my-custom-alert
version: "1.0.0"
type: policy
engines:
  hex: ">=0.8.0"
author: "user@example.com"
# ... rest of existing policy YAML format ...
```

---

### 2.3 CLI Command Extension (`type: command`)

**What it is:** An executable that becomes a `hex <subcommand>`. No binary modification needed.

**Format:** Executable file + `command.yaml` manifest in a bundle directory

**Lives in:** `~/.hex/extensions/<bundle>/commands/<name>` or `<repo>/.hex/extensions/<bundle>/commands/<name>`

**Discovery:** At startup, hex scans extension directories for `commands/` subdirs and adds
them to the PATH as `hex-<name>`. When the user runs `hex my-cmd`, hex dispatches to the
executable via the same mechanism as git's external commands.

**Example bundle layout:**
```
~/.hex/extensions/my-tools/
  extension.yaml
  commands/
    hex-report          ← executable (any language)
    hex-report.yaml     ← optional: description, flags, usage
```

**`hex-report.yaml`:**
```yaml
name: report
description: "Generate a weekly activity report"
usage: "hex report [--since DATE]"
```

---

### 2.4 Agent Behavior Extension (`type: agent`)

**What it is:** A custom agent charter that defines a new hex agent (identical to `projects/*/charter.yaml` format).

**Format:** `charter.yaml` (already established)

**Lives in:** `~/.hex/extensions/<bundle>/agents/<name>/charter.yaml` or `<repo>/.hex/extensions/<bundle>/agents/<name>/charter.yaml`

**How it differs from `projects/`:** Project agents in `projects/` are first-party, tracked in the repo.
Extension agents are installed via `hex extension install` and live in user space. Both use the same
`charter.yaml` schema — agents from extensions are registered into the harness exactly like project agents.

**`extension.yaml` for an agent bundle:**
```yaml
name: my-monitor-agent
version: "1.0.0"
type: agent
engines:
  hex: ">=0.8.0"
agents:
  - agents/my-monitor/charter.yaml
```

---

### 2.5 SSE Topic Extension (`type: sse_topic`)

**What it is:** Declares a new real-time event stream that the hex SSE server will publish.

**Format:** YAML descriptor + optional producer script

**Lives in:** `~/.hex/extensions/<bundle>/sse/<topic-name>.yaml`

**Contract:** The extension declares the topic name, schema, and a producer script. Hex registers
the topic in the SSE server at startup and starts the producer. Consumers subscribe via
`GET /sse?topic=<topic-name>` — identical to existing SSE topics.

**Example:**
```yaml
name: sse-github-activity
version: "1.0.0"
type: sse_topic
engines:
  hex: ">=0.8.0"
topics:
  - name: github.pr.opened
    schema:
      type: object
      properties:
        repo: { type: string }
        pr_number: { type: integer }
        title: { type: string }
    producer: scripts/github-pr-watcher.sh
    interval: 60s
```

---

### 2.6 UI View Extension (`type: ui_view`)

**What it is:** A custom web page served by the hex HTTP server. Covered in detail in
`hex-ui-extensions.md`; summarized here for completeness.

**Format:** Static files in `views/` dir + manifest entry

**Lives in:** `~/.hex/extensions/<bundle>/views/<view-name>/`

**Entry in `extension.yaml`:**
```yaml
views:
  - name: my-dashboard
    path: /my-dashboard           ← served at http://hex.local/my-dashboard
    index: views/my-dashboard/index.html
    title: "My Dashboard"
    icon: chart-bar
    nav: true                     ← show in hex landing page nav
```

---

### 2.7 Integration Bundle (`type: integration`)

**What it is:** The existing integration format (`templates/integrations/_template/`), formally
declared as an extension type. Backward-compatible — existing integrations continue to work.

**Format:** `integration.yaml` + supporting files (already established)

**Lives in:** `<repo>/integrations/` (existing) or `~/.hex/extensions/<bundle>/` (new: installable)

**Validation:** `hex extension validate` checks that the integration's declared `requires.events`
are available in the running hex instance.

---

## 3. Extension Manifest — `extension.yaml`

Every extension bundle (except freestanding single-file skills/policies) declares itself with
an `extension.yaml` at the bundle root.

### Format: YAML

**Rationale:** YAML is already the hex config language (policies, charters, integrations). JSON
would be out of place. TOML was considered but offers no advantage for this schema.

### Full Schema

```yaml
# extension.yaml
name: my-extension              # machine name, used as install ID
version: "1.2.0"                # semver
title: "My Extension"           # human display name
description: "What it does"
author: "user@example.com"
license: "MIT"                  # optional

# Compatibility
engines:
  hex: ">=0.8.0 <2.0.0"         # semver range; hex refuses to load if out of range

# What this extension provides
provides:
  skills:
    - skills/my-skill/SKILL.md
  policies:
    - policies/my-policy.yaml
  commands:
    - commands/hex-report
  agents:
    - agents/my-monitor/charter.yaml
  sse_topics:
    - sse/github-activity.yaml
  views:
    - name: my-dashboard
      path: /my-dashboard
      index: views/my-dashboard/index.html
      title: "My Dashboard"
      nav: true

# What this extension needs from hex
requires:
  capabilities:
    - events              # access to hex event bus
    - assets              # access to hex asset store
    - messaging           # access to hex messaging
    - sse                 # access to SSE server
    - agent_harness       # can register agents
  events:
    - timer.tick.hourly   # events this extension listens for
  min_hex_version: "0.8.0"

# Runtime isolation
sandbox:
  mode: subprocess        # subprocess (isolated) | inline (trusted, no isolation)
  # subprocess: extension commands run in a child process; extension agents run in the harness
  # inline: only for system/trusted extensions that need direct harness access

# Upgrade behavior
upgrade:
  strategy: preserve_user_data   # preserve_user_data | replace | merge
  # preserve_user_data: never overwrite files in user-space overlay
  # replace: overwrite everything (only valid for core extensions)
  # merge: attempt merge, prompt on conflict
```

### Minimal manifest (single-type extension)

```yaml
name: rust-debugging-skill
version: "1.0.0"
type: skill
engines:
  hex: ">=0.8.0"
provides:
  skills:
    - SKILL.md
```

---

## 4. Extension Lifecycle

### 4.1 Install

```
hex extension install <source>
```

Sources:
- **Local path:** `hex extension install ./my-extension/`
- **Git URL:** `hex extension install github.com/user/hex-my-extension`
- **Registry (future):** `hex extension install @registry/my-extension`

Install steps:
1. Download/copy bundle to `~/.hex/extensions/<name>/`
2. Parse `extension.yaml` — reject if schema invalid
3. Check `engines.hex` semver range — reject if incompatible
4. Check `requires.capabilities` — warn if a required capability is unavailable
5. Validate all declared files exist (skills, policies, commands, views)
6. Register with extension index at `~/.hex/extensions/index.yaml`
7. Restart affected subsystems (hot-reload where supported)

### 4.2 List

```
hex extension list
```

Output: table of installed extensions with name, version, type(s), status (active/disabled/error).

Reads from `~/.hex/extensions/index.yaml` (written by `hex extension install`).

### 4.3 Validate

```
hex extension validate [<path>]
```

Runs static validation without installing:
- Schema check on `extension.yaml`
- Verifies all declared files exist
- Checks `engines.hex` compatibility
- Checks `requires.capabilities` against current hex instance
- Dry-run policy syntax check for any declared policies

Returns exit 0 if valid, exit 1 with errors if not.

### 4.4 Enable / Disable

```
hex extension enable <name>
hex extension disable <name>
```

Sets `status: disabled` in index. Disabled extensions are discovered but not loaded.
Useful for troubleshooting without uninstalling.

### 4.5 Remove

```
hex extension remove <name>
```

1. Read `~/.hex/extensions/index.yaml`
2. Confirm with user if extension has user-modified files
3. Delete `~/.hex/extensions/<name>/`
4. Remove entry from index
5. Restart affected subsystems

### 4.6 Upgrade

```
hex extension upgrade <name>
hex extension upgrade --all
```

1. Fetch new version from original source
2. Compare with installed version
3. Apply `upgrade.strategy` from manifest:
   - `preserve_user_data`: copy new files, skip any file that user has modified (detected by checksum comparison against install-time snapshot)
   - `replace`: overwrite all files
4. Update index entry
5. Restart affected subsystems

### 4.7 How Extensions Access Core Primitives

Extensions access hex primitives through **environment variables + REST + SSE** — not through
direct Rust API calls. This is the subprocess boundary.

| Primitive | How extensions access it |
|-----------|-------------------------|
| **Events** | `POST $HEX_API/events` to emit; `GET $HEX_API/events?since=<ts>` to poll; `$HEX_EVENTS_SOCKET` for socket-based listen |
| **Assets** | `GET/POST/DELETE $HEX_API/assets/<key>` |
| **Messaging** | `POST $HEX_API/messages` (emit to channel); `GET $HEX_API/messages?channel=<ch>` |
| **SSE** | Subscribe: `GET $HEX_SSE_URL?topic=<topic>`; Publish (from producer scripts): `POST $HEX_API/sse/publish` |
| **Agent harness** | Extension agents declared in `extension.yaml` are registered by hex at load time — no direct API needed |

All primitives are available via HTTP to the `$HEX_API` base URL (default: `http://localhost:$HEX_PORT`).
Extension processes receive `HEX_API`, `HEX_PORT`, `HEX_SSE_URL`, and `HEX_WORKSPACE` as environment
variables at launch.

---

## 5. Upgrade Safety — Concrete Rules

### Rule 1: Two-Zone File Tree (enforced, not advisory)

`hex upgrade` operates with an explicit allowlist of directories it may write:

```
WRITABLE BY hex upgrade:
  <repo>/system/**
  <repo>/system/version.txt

NEVER WRITTEN BY hex upgrade:
  ~/**                        ← entire user home
  <repo>/projects/**
  <repo>/templates/**
  <repo>/.hex/**
  <repo>/integrations/**      ← user-authored integrations
  <any path not in allowlist>
```

The upgrade script validates its own write targets before executing. If a computed write path
falls outside the allowlist, it aborts with an error rather than proceeding.

### Rule 2: engines.hex Semver Guard

Every extension declares:
```yaml
engines:
  hex: ">=0.8.0 <2.0.0"
```

At load time, hex checks the installed version against this range. If incompatible:
- The extension is **skipped** (not loaded), not crashed
- A warning is emitted: `extension <name>@<version> requires hex >=0.8.0, found 0.7.2 — skipped`
- `hex extension list` shows status `incompatible`

Hex never removes extensions on upgrade — it skips them and tells the user.

### Rule 3: Two-Zone CLAUDE.md Pattern Extended to All Managed Files

The existing two-zone CLAUDE.md pattern (core-managed zone / user zone within a single file)
extends to any file that `hex upgrade` produces but users may also edit:

```
# system/CLAUDE.md — two zones
# ╔══════════════════════════════════╗
# ║  HEX CORE ZONE — DO NOT EDIT    ║
# ║  Overwritten by hex upgrade      ║
# ╚══════════════════════════════════╝

... core content ...

# ╔══════════════════════════════════╗
# ║  USER ZONE — SAFE TO EDIT       ║
# ║  Never touched by hex upgrade    ║
# ╚══════════════════════════════════╝

... user additions ...
```

Files using this pattern: `system/CLAUDE.md`, `system/AGENTS.md`, `README.md` (if ship-managed).

Files that don't need the pattern (because they're entirely user-owned): everything in `~/.hex/`,
`<repo>/.hex/`, `<repo>/projects/`, `<repo>/integrations/`.

### Rule 4: Install-Time Checksum Snapshot

When `hex extension install` runs, it writes a checksum manifest:
```
~/.hex/extensions/<name>/.install-manifest.yaml
```

This records the SHA256 of every installed file at install time. On upgrade:
- Files that match their install-time checksum are replaced (user hasn't touched them)
- Files that differ are preserved (user has modified them) and the new version is written to `<file>.new`
- The user is prompted: `<file> has local changes. New version saved to <file>.new`

This is the same conflict resolution UX as NanoClaw's git merge, but without requiring git.

---

## 6. Extension Index

`~/.hex/extensions/index.yaml` is the source of truth for installed extensions:

```yaml
schema_version: "1"
extensions:
  - name: my-dashboard
    version: "1.2.0"
    source: "github.com/user/hex-my-dashboard"
    installed_at: "2026-04-27T21:00:00Z"
    path: "~/.hex/extensions/my-dashboard"
    status: active          # active | disabled | incompatible | error
    provides:
      - skill
      - ui_view
    engines:
      hex: ">=0.8.0"
```

The index is written by `hex extension install/remove/upgrade` and read by `hex extension list`
and by the hex server at startup.

---

## 7. What Ships in hex v0.8.0 vs. Later

### Phase 1 — Foundation (v0.8.x)

These are the minimum viable extensibility features. Nothing else works without them.

- [ ] Define and document the two-zone file tree (this document)
- [ ] `<repo>/.hex/skills/` auto-discovery (scanned at startup, no manifest required)
- [ ] `<repo>/.hex/policies/` auto-discovery (extends existing `system/policies/` scanning)
- [ ] `extension.yaml` schema + parser
- [ ] `hex extension validate` command
- [ ] `engines.hex` semver guard at load time

### Phase 2 — Install + CLI (v0.9.x)

- [ ] `hex extension install/list/remove/upgrade`
- [ ] `~/.hex/extensions/index.yaml` management
- [ ] CLI command extensions (`commands/` subdir on PATH)
- [ ] Install-time checksum snapshot + upgrade conflict detection

### Phase 3 — Hooks + SSE (v0.10.x)

- [ ] Extension-declared SSE topics (`sse/` subdir)
- [ ] Extension hook system: `OnHexEvent`, `OnAssetChange` hooks in policies
- [ ] Non-blocking hook failure (extension error → warning log, not server crash)
- [ ] `HEX_API` environment variable injection for subprocess extensions

### Phase 4 — Agents + UI (v1.0.x)

- [ ] Agent behavior extensions (charter.yaml in extension bundles)
- [ ] UI view extensions (static files served at declared paths)
- [ ] `hex extension enable/disable`
- [ ] Extension Host subprocess isolation

### Phase 5 — Ecosystem (v1.x+)

- [ ] `engines.hex` enforced via checksum manifest (not just advisory)
- [ ] Extension registry / discovery
- [ ] Hex as MCP host
- [ ] Hex as MCP server (events, assets, messages as MCP resources/tools)

---

## 8. Worked Examples

### Example A: Install a skill extension

```bash
# User creates a skill bundle
mkdir -p ~/.hex/extensions/rust-debugging/skills

cat > ~/.hex/extensions/rust-debugging/extension.yaml << 'EOF'
name: rust-debugging
version: "1.0.0"
title: "Rust Debugging Skill"
engines:
  hex: ">=0.8.0"
provides:
  skills:
    - skills/SKILL.md
EOF

cat > ~/.hex/extensions/rust-debugging/skills/SKILL.md << 'EOF'
---
activation: glob
glob: "**/*.rs"
---
When debugging Rust, check borrow checker errors first. Use `cargo check` before `cargo build`.
EOF

hex extension validate ~/.hex/extensions/rust-debugging/
# ✓ extension.yaml valid
# ✓ skills/SKILL.md exists
# ✓ engines.hex ">=0.8.0" satisfied by hex 0.8.2
# Extension is valid.
```

### Example B: Add a new CLI command

```bash
mkdir -p ~/.hex/extensions/my-tools/commands

cat > ~/.hex/extensions/my-tools/extension.yaml << 'EOF'
name: my-tools
version: "1.0.0"
title: "My Custom Tools"
engines:
  hex: ">=0.8.0"
provides:
  commands:
    - commands/hex-report
EOF

cat > ~/.hex/extensions/my-tools/commands/hex-report << 'EOF'
#!/bin/bash
# Generates a weekly activity report
curl -s "$HEX_API/events?since=7d" | python3 -c "
import sys, json
events = json.load(sys.stdin)
print(f'Events this week: {len(events)}')
"
EOF
chmod +x ~/.hex/extensions/my-tools/commands/hex-report

# After hex restart:
hex report
# Events this week: 142
```

### Example C: Project-scoped policy (no manifest needed for single-file extensions)

```bash
# In the project repo:
mkdir -p .hex/policies

cat > .hex/policies/alert-on-error.yaml << 'EOF'
name: alert-on-error
version: "1.0.0"
description: "Alert when hex.error events fire"
provides:
  events:
    - hex.alert.sent
requires:
  events:
    - hex.error

rules:
  - name: alert
    trigger:
      event: hex.error
    actions:
      - type: shell
        command: |
          echo "ERROR: $HEX_EVENT_DATA" | mail -s "hex error" ops@example.com
EOF

# hex auto-discovers .hex/policies/ at startup — no install step needed
hex events tail
# [info] Loaded project policy: alert-on-error
```

---

## 9. Design Decisions Summary

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Manifest format | YAML | Already the hex config language; consistent with policies + charters |
| Extension root | `~/.hex/extensions/` + `<repo>/.hex/extensions/` | Matches Hermes + Codex user-space conventions; project-local overrides global |
| Code execution boundary | subprocess (HTTP/env vars) | Extension crashes don't take down hex server; aligns with MCP + VS Code Extension Host patterns |
| Upgrade conflict resolution | Checksum snapshot + `.new` files | Avoids silent overwrites; explicit user decision; no git required |
| Discovery mechanism | Dir scan + `index.yaml` | Startup scan for freestanding files; index for managed bundles — both work without a daemon |
| CLI dispatch | `hex-<name>` on PATH | Identical to git external commands; no binary modification; language-agnostic |
| Capability access | REST API via `$HEX_API` | Language-agnostic; works for shell, Python, Node; aligns with hex's existing HTTP server |

---

*Next: `hex-ui-extensions.md` covers the UI view extension system in detail.*
