# hex-ops

Operational guide for hex's runtime glue: session management, dashboards,
LaunchAgents, and **telemetry**.

---

## LaunchAgents (launchd)

> **Note:** This section documents hex's **sanctioned** supervised services (the harness
> bootstrap and the personal BOI daemon). It is **not** a pattern to copy for new scheduled
> jobs — new recurring/scheduled work is a **hex worker**, and new persistent processes ride
> the engine via **iii-exec** (see the AGENTS.md "Automation" rule and `docs/iii-hex.md`). Do
> not add new per-job LaunchAgents (decision:
> `persistent-processes-via-iii-exec-not-launchagents-2026-06-11`).

hex's supervised long-running services run as **per-user gui LaunchAgents** in
`~/Library/LaunchAgents/`, bootstrapped into the **`gui/<uid>`** domain, with
**`SessionCreate=true`** and **no `UserName`**. Examples: `com.hex.harness` (the core
harness); `com.mrap.boi-daemon` (the personal BOI daemon — same pattern). The code already
implements this: `hex harness start|stop|status` targets `gui/$(id -u)/com.hex.harness` and
`upgrade.rs` kickstarts the same target after a binary swap.

### Why gui/ + SessionCreate (rationale)

The harness runs per-task reasoning *inside* `claude`, and BOI workers spawn `claude`;
Claude Code auth lives in the macOS **login keychain**. `SessionCreate=true` bridges the
launchd job into the user's Aqua login (security) session so keychain lookups succeed. The
alternatives cannot reach the login keychain:

| Option | Login keychain | Notes |
|---|---|---|
| **gui/ LaunchAgent + SessionCreate** (chosen) | yes — via the Aqua session | must be bootstrapped from a real GUI login session |
| user/ LaunchAgent (no SessionCreate) | no — no Aqua session | `SessionCreate` + `user/` also fails to bootstrap (EIO) |
| system LaunchDaemon (`UserName=mrap`) | no — runs outside any login session | starts at boot but can't read the login keychain |

FileVault forces a GUI login at every boot on this box, so there is effectively always a
login session — the gui LaunchAgent's only downside ("dies on logout") is moot.

**macOS 26 caveat:** the SecurityAgent session is NOT inherited by child processes — spawn
`claude` as a DIRECT program, never `bash -> claude`, or it loses keychain access.

### Operational gotchas (learned 2026-06-05)

- **Bootstrap only from a real GUI login session.** `launchctl bootstrap` returns
  `Input/output error` (errno 5) when run from a *detached* session — inside **tmux** or
  over plain **SSH** — because those carry their own audit session (`asid`), not the
  Aqua login session. The sandboxed agent shell cannot bootstrap either. Run it from
  **Terminal.app at the Mac console or via Screen Sharing**.
- **Reload = `bootout` THEN `bootstrap`.** `bootstrap` alone fails on an already-loaded
  service. After editing a plist:
  ```
  launchctl bootout   gui/$(id -u)/com.hex.harness
  launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.hex.harness.plist
  ```
- **Diagnose which session you're in:** `launchctl print pid/$$ | grep -E 'asid|coalition'`.
  If the coalition is `com.mrap.tmux-boot` (or the `asid` is not your Aqua login session),
  `launchctl bootstrap` will EIO from there — switch to a GUI terminal.
- **Status / health:** `launchctl print gui/$(id -u)/com.hex.harness | grep -E 'state =|pid ='`.

---

## Telemetry

hex telemetry is a **native, local SQLite event store** owned by the Rust
harness. Every iii worker job is auto-traced via the worker host
(`iii_worker::run_command`), and any other code path or shell script can emit
into the same store via `hex telemetry record`. There is no Prometheus,
Grafana, or OTLP collector — a single-user local system gets a single-user
local store.

### Store

- **Path:** `$HEX_DIR/.hex/telemetry/events.db` (HEX_DIR falls back to `.`).
- **Engine:** SQLite (rusqlite, bundled) with `PRAGMA journal_mode=WAL`.
- **Readers:** consumers (failures detector, probe) open plain read-only via
  `telemetry::open_ro`. NEVER open this WAL db with `immutable=1` — immutable
  readers silently skip un-checkpointed WAL frames, i.e. the freshest rows.
- **Schema:**

```sql
CREATE TABLE events (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  ts          TEXT    NOT NULL,   -- RFC3339 UTC, event start time
  source      TEXT    NOT NULL,   -- worker_name, or arbitrary source
  event       TEXT    NOT NULL,   -- function/job id, e.g. hex::memory::index
  status      TEXT    NOT NULL,   -- 'ok' | 'error' | 'spawn_error'
  duration_ms INTEGER,            -- nullable
  exit_code   INTEGER,            -- nullable
  detail      TEXT                -- stdout/stderr tail or free-form/JSON meta
);
CREATE INDEX idx_events_ts    ON events(ts);
CREATE INDEX idx_events_event ON events(event);
```

### Auto-tracing of iii jobs

Every iii job flows through one chokepoint — `iii_worker::run_command` — and
that function writes a telemetry row on every outcome:

- `status = ok` on a successful exit.
- `status = error` on a non-zero exit (records the `exit_code`).
- `status = spawn_error` if the process failed to launch.

Each row carries the worker name as `source`, the job id as `event`, the
measured `duration_ms`, and a stdout/stderr tail in `detail`. Zero per-worker
opt-in is required — wiring a new iii worker automatically gets telemetry.

Telemetry writes from inside the worker are **loud-but-not-fatal**: a write
failure logs `telemetry: failed to record ...` to stderr but never fails the
observed job. Telemetry is observational; it must not break the thing it
observes.

### `hex telemetry` commands

```bash
hex telemetry recent   [--limit N] [--json]      # newest events first
hex telemetry failures [--since 24h|7d] [--json] # only status != 'ok'
hex telemetry status   [--json]                  # per-event aggregates
hex telemetry record   --source S --event E --status ok|error|spawn_error \
                       [--duration-ms N] [--exit-code N] [--detail TEXT]
hex telemetry prune    [--keep-days 30]
```

- **recent / failures / status** print a compact aligned text table by default
  (`ts source event status dur`) or JSON with `--json`. `--since` accepts
  `Nh`/`Nd`; default `24h`.
- **record** is the manual emit seam: any shell script or external tool can
  push an event into the same store. Unlike the in-worker path, write failures
  surface as a non-zero exit.
- **prune** deletes rows older than `keep-days` (default 30) and prints how
  many it removed.

### `hex failures` — unexpected-failure digest

`hex failures [--window N] [--alert]` evaluates the worker registry's cron
expectations against the store: MISSED runs (duration-aware slack, downtime
subtraction), NEVER-RAN fids, modules on disk but not compiled into the
binary, failure signatures (new vs chronic), and engine double-fires. Exit 1
when anything is bad; `--alert` routes each condition through
`hex::alert::notify` (6h dedupe per condition key). `hex failures probe` is
the out-of-process liveness probe (events.db staleness + harness launchd
state; template: `system/templates/launchd/com.hex.failures-probe.plist`).
Detection only — it never remediates. The daily in-harness digest is the
`hex-failures` cron worker (13:30 UTC ≈ 06:30 PT).

### Doctor check

`hex doctor` runs a `telemetry-health` check. If the store is missing it
skips. Otherwise it queries the last 24h: any non-`ok` rows produce a warn
with a count and the most recent failing event id ("Run `hex failures`
(digest) or `hex telemetry failures` (raw rows) to inspect"); a clean window
passes.

### History

This replaces the old in-memory iii observability (ephemeral, 1000-span cap,
not queryable) and the previous `.hex/telemetry/events.db` that was removed
when `hex-events` was deleted on 2026-06-02. The store is now rebuilt
natively in the Rust harness.

### Resources

`hex resources sample|status` — hourly disk sampler (tier 0) + deterministic floor/trend pressure rules (tier 1) over the same telemetry store; on breach it alerts (6h dedupe) and emits `resource.pressure` level-triggered. Detection only — never cleans anything up.

---

## LLM configuration (`llm.toml`)

Every LLM-backed feature in hex — memory distill (extract + judge), memory
consolidate's operating-model audit, and the doctor provider health check —
resolves its provider endpoint, model, max_tokens, transport, and API key
environment variable through a single registry. Defaults are baked in, so a
fresh install with no config behaves exactly as today.

To customize: copy `system/templates/llm.toml.example` to
`$HEX_DIR/.hex/config/llm.toml` and edit. The example file documents the full
schema with commented-out defaults for each known use case.

### Use cases

| Use case            | What it backs                                 | Built-in default                  |
|---------------------|-----------------------------------------------|-----------------------------------|
| `memory_extract`    | `hex memory distill` — structured extraction  | `anthropic/claude-sonnet-4.5`     |
| `memory_judge`      | `hex memory distill` — retention judge        | `anthropic/claude-sonnet-4.5`     |
| `consolidate_audit` | `hex memory consolidate full` — audit pass    | `anthropic/claude-sonnet-4.5`     |
| `health_check`      | `hex doctor` — cheap provider probe           | `anthropic/claude-haiku-4.5`      |

### Resolution order (highest wins)

1. **Env var** `HEX_LLM_MODEL_<USE_CASE_UPPER>` — e.g.
   `HEX_LLM_MODEL_MEMORY_EXTRACT=anthropic/claude-opus-4.5` — and
   `HEX_LLM_TRANSPORT_<USE_CASE_UPPER>` for the transport.
   `HEX_CONSOLIDATE_MODEL` is still honored as a back-compat alias for
   `consolidate_audit`.
2. **`[use_cases.<name>]`** table in `llm.toml`.
3. **`[defaults]`** table in `llm.toml`.
4. **Built-in registry defaults** (the values above).

### Transports

Each use case resolves a `transport` (spec Sbe8m4886):

- **`http`** (default for every use case) — POST to an OpenAI-compatible
  chat/completions endpoint using `base_url` + `api_key_env`. Pre-existing
  behavior; nothing changes unless you opt in below.
- **`claude-cli`** — shell out to a headless `claude -p`, authenticated via
  the **macOS login keychain** (explicitly NOT the daemon setup-token — see
  decision `memory-cli-transport-no-setup-token-2026-06-10`). Useful for
  shifting heavy use cases (`memory_extract`, `consolidate_audit`) onto a
  Claude subscription instead of metered HTTP.

How the `claude-cli` spawn works (`system/harness/src/memory/claude_cli.rs`,
verified recipe 2026-06-10):

- Runs `claude -p <prompt> --strict-mcp-config --mcp-config '{"mcpServers":{}}'
  --no-session-persistence --setting-sources '' --disable-slash-commands
  --settings <…> --model <…> --output-format json` from a **fresh tempdir**
  (CLAUDE.md auto-discovery is cwd-based; a workspace cwd would slurp it).
- Strips `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and
  `ANTHROPIC_AUTH_TOKEN` from the child env so claude falls through to the
  login keychain (any of them would shadow it).
- `--settings` gets the optional `claude_settings_file` path, else a hardened
  inline JSON (hooks/auto-memory/skills/plugins/telemetry all disabled).
- Caveats: requires an unlocked login keychain in the `gui/<uid>` session
  (see LaunchAgents above); **`max_tokens` is NOT enforceable** in this
  transport (the CLI has no flag for it) — the cap is ignored; model ids are
  mapped from registry form to CLI form (`anthropic/claude-sonnet-4.5` →
  `claude-sonnet-4-5`), non-`anthropic/` ids pass through verbatim, so
  CLI aliases like `"sonnet"` work directly in `llm.toml`.
- Unknown transport value (file or env override) is a hard `resolve()` error;
  a configured-but-missing `claude_settings_file` is a loud failure, never a
  silent fallback (S6).

### Schema (excerpt)

```toml
[defaults]
model       = "anthropic/claude-sonnet-4.5"
base_url    = "https://openrouter.ai/api/v1/chat/completions"
api_key_env = "OPENROUTER_API_KEY"
# transport = "http"                       # or "claude-cli"

[use_cases.memory_extract]
model      = "..."
max_tokens = 16384
# transport            = "claude-cli"      # keychain-authed headless claude -p
# claude_settings_file = "/path/to/settings.json"  # optional --settings file
```

`base_url` lets you point any use case at an OpenAI-compatible alternative
(Ollama, vLLM, a self-hosted gateway). `api_key_env` names the environment
variable to read the key from; the OpenRouter file fallback
(`$HEX_DIR/.hex/secrets/openrouter.env`) only applies when it's left at the
default `OPENROUTER_API_KEY`.

### Failure modes

- **No `llm.toml`** — built-ins are used, no warning.
- **Malformed TOML or invalid field** — hex fails loudly to stderr and the
  operation aborts (per S6, no quiet failures).
- **Unknown `[use_cases.*]` table** — warning to stderr, otherwise tolerated.
- `hex doctor` runs an `llm-config` check that validates the file when
  present and prints the resolved model per use case.

---

## Lean Claude runs (`claude-runs.toml`)

**Policy (spec Sf5bj7y1d):** every headless `claude -p` invocation hex makes
runs as lean as possible — no plugins, no skills, no MCP servers, no hooks,
no CLAUDE.md — unless a per-run profile explicitly re-enables specific
functionality.

This is enforced by a central profile resolver
(`system/harness/src/claude_runs.rs`) and a tiny CLI surface:

```
hex claude-flags <profile>     # prints eval-safe shell flags for that profile
```

Built-in profiles (apply with or without a config file):

| profile           | used by                                                | re-enabled |
|-------------------|--------------------------------------------------------|-----------|
| `default`         | fallback                                               | —         |
| `harness_worker`  | `system/harness/src/worker/run.rs`                     | —         |
| `eval`            | `tests/eval/run_eval.py`                               | —         |

Lean default = `--bare --strict-mcp-config --mcp-config '{}'
--disable-slash-commands`. `--bare` skips auto-discovery of hooks, LSP,
plugin sync, auto-memory, CLAUDE.md, and plugin/MCP/skill auto-discovery.
The explicit empty strict mcp config ensures no MCP server loads even on a
future Claude Code version where `--bare` covers less.

### Bare-run auth injection

`--bare` also skips **keychain reads** and ignores `CLAUDE_CODE_OAUTH_TOKEN`
by design (upstream anthropics/claude-code#51047, closed not-planned) — so a
bare run has no auth path on its own. The harness compensates at spawn time
(`system/harness/src/worker/run.rs`, spec Sbe8m4886): when the resolved
profile is `bare = true` and the harness has a non-empty
`CLAUDE_CODE_OAUTH_TOKEN` (the daemon setup-token), it injects that value as
`ANTHROPIC_AUTH_TOKEN` into **that child's env only** — `--bare` honors the
bearer var, and the setup-token works as a bearer (verified 2026-06-10).

Rules (decision `daemon-token-scoped-not-session-wide-2026-06-10`):

- **Child-scoped only.** Never `launchctl setenv`, never process-wide
  `std::env::set_var` — `ANTHROPIC_AUTH_TOKEN` sits at precedence level 2
  and would shadow every other auth path if leaked session-wide.
- **Bare + no token** → loud stderr warning ("bare claude run has no auth
  path"), then spawn anyway (S6 — fail loud, not silent).
- **Non-bare profiles never get the injection** — they must keep falling
  through to the login keychain.

### Profile schema

Drop a `claude-runs.toml` at `$HEX_DIR/.hex/config/claude-runs.toml` to
override the built-ins. See `system/templates/claude-runs.toml.example` for
a fully commented reference. Minimum:

```toml
[defaults]
bare = true
# disable_slash_commands = true
# mcp_servers     = []     # names looked up in workspace .mcp.json
# plugin_dirs     = []
# setting_sources = []     # subset of ["user", "project", "local"]
# allowed_tools   = []
# extra_flags     = []     # appended verbatim

[runs.harness_worker]
# Lean — no overrides needed.

[runs.eval]
```

### Re-enable knobs (flag emission)

| TOML field               | Emits                                              |
|--------------------------|----------------------------------------------------|
| `bare = true`            | `--bare`                                           |
| `mcp_servers = [..]`     | `--strict-mcp-config --mcp-config '<inline json>'` containing ONLY the named servers, looked up from `.mcp.json`. Empty/absent → `'{}'`. |
| `disable_slash_commands` | `--disable-slash-commands`                         |
| `plugin_dirs = [..]`     | repeated `--plugin-dir <dir>`                      |
| `setting_sources = [..]` | `--setting-sources a,b,c`                          |
| `allowed_tools = [..]`   | `--allowedTools "..."`                             |
| `extra_flags = [..]`     | appended verbatim                                  |

Unknown profile name, malformed TOML, or `mcp_servers` naming a server
absent from the workspace MCP config → **hard error** (Standing Order S6:
no quiet failures). `hex doctor` runs the `claude-runs-config` check which
absent-passes when no `claude-runs.toml` is present, and validates the file
when one is — including resolving every named MCP server.

### Using the flags

Shell call sites use `hex claude-flags` with eval-style substitution:

```bash
claude $(hex claude-flags harness_worker) -p "$(cat prompt.txt)"
```

The Rust harness call sites build the arg vector via
`claude_runs::resolve(<profile>, Some(&hex_dir))?.to_cli_flags(&mcp)?`.

**Behavior change at install time:** a machine with no
`claude-runs.toml` will still work — the built-in profiles apply and runs
become lean. That IS the intended default; only opt in to re-enabling
specific functionality, per profile.
