# hex-ops

Operational guide for hex's runtime glue: session management, dashboards,
LaunchAgents, and **telemetry**.

---

## LaunchAgents (launchd)

> **Note:** This section documents hex's **sanctioned** supervised services (canonical
> table below). It is **not** a pattern to copy for new scheduled
> jobs — new recurring/scheduled work is a **hex worker**, and new persistent processes ride
> the engine via **iii-exec** (see the AGENTS.md "Automation" rule and `docs/iii-hex.md`). Do
> not add new per-job LaunchAgents (decision:
> `persistent-processes-via-iii-exec-not-launchagents-2026-06-11`).

hex's supervised long-running services run as **per-user gui LaunchAgents** in
`~/Library/LaunchAgents/`, bootstrapped into the **`gui/<uid>`** domain, with **no
`SessionCreate` key** and **no `UserName`**. Examples: `com.hex.harness` (the core
harness); an instance-declared daemon such as `com.mrap.boi-daemon` — same pattern.
Canonical list: table below. The code already
implements this: `hex harness start|stop|status` targets `gui/$(id -u)/com.hex.harness` and
`upgrade.rs` kickstarts the same target after a binary swap.

### Sanctioned launchd surface (canonical list)

Foundation-sanctioned services — a loaded launchd job on neither this list nor the
instance's declared list is an anomaly to flag:

| Label | Role | Source |
|---|---|---|
| `com.hex.harness` | engine host | rendered by `hex harness start` |
| `com.hex.harness-watchdog` | restarts the harness if it dies | rendered by `hex harness start` |
| `com.hex.failures-probe` | out-of-process liveness probe — watches the harness itself, deliberately NOT a harness worker | `system/templates/launchd/com.hex.failures-probe.plist` |
| `com.hex.scipd` | code-intel daemon | `system/templates/launchd/com.hex.scipd.plist` |
| `com.hex.hitl-nudge` | HITL nudge | `system/templates/launchd/com.hex.hitl-nudge.plist` |

An instance may extend this with its own entries, **declared in the instance
CLAUDE.md/AGENTS.md Automation section** (e.g. a personal BOI daemon `com.mrap.boi-daemon`,
a tmux boot script, a session sentinel, or a `pf`-persist system LaunchDaemon).
`.disabled`/`.staged` plist suffixes are parked rollback/staging state, not violations.
History: pre-2026-08 docs disagreed on this list (AGENTS.md said harness-only; the mrap
instance's own architecture.md copy said boi-daemon+tmux-boot only) — reconciled 2026-08-19.

### Why gui/ with no SessionCreate (rationale)

The harness runs per-task reasoning *inside* `claude`, and BOI workers spawn `claude`;
Claude Code auth lives in the macOS **login keychain**. A `gui/<uid>` LaunchAgent that
is bootstrapped from a real Aqua login session **inherits that session's unlocked login
keychain automatically** — no `SessionCreate` key needed. Setting `SessionCreate` to
true would DETACH the job into a new audit session and BLOCK login-keychain access
(verified 2026-06-05: rc=36 `errSecAuthFailed` with the key set, rc=0 without), so it
is intentionally absent — enforced by `system/harness/tests/harness_cli_test.rs`
(asserts the rendered plist has no `<key>SessionCreate</key>`) and re-asserted by
`daemon_green_adoption_test.rs` against `main.rs`. The alternatives cannot reach the
login keychain:

| Option | Login keychain | Notes |
|---|---|---|
| **gui/ LaunchAgent, no SessionCreate** (chosen) | yes — inherits the Aqua session | must be bootstrapped from a real GUI login session |
| gui/ LaunchAgent with `SessionCreate` set | no — job detaches into a new audit session | verified rc=36 `errSecAuthFailed`; forbidden by test |
| user/ LaunchAgent | no — no Aqua session | `user/` bootstrap from a GUI session also fails (EIO) |
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

## Cron workers

Recurring in-harness jobs registered in the foundation worker registry
(`hex_modules::module_registry()`) and run in-process by the engine — never
LaunchAgents. Each fire is auto-traced (telemetry row per run) and its cron
expectation is checked by `hex failures` (MISSED/NEVER-RAN detection).

| Worker | Schedule | Does |
|---|---|---|
| `hex-failures` | daily 13:30 UTC (≈06:30 PT) | unexpected-failure digest (see `hex failures` above) |
| `resources` | hourly | disk sampler + pressure rules (see Resources above) |
| `boi-spec-watch` | every 5 min (`0 */5 * * * * *`) | watches BOI spec/task state above phase level |

### `boi-spec-watch`

Every 5 minutes it opens `~/.boi/v2/boi.db` **read-only** (never writes, never
holds a lock; bounded busy timeout; 14-day lookback), diffs the spec/task state
against the prior tick's persisted snapshot, and alerts on exactly two
transition classes:

1. a **spec newly reaching a terminal status** (`completed` / `failed` / `canceled`)
2. a **task newly entering `state='blocked'`** (any reason — every blocked task needs an operator)

Alerts go through the shared operator path (`hex::alert::notify`: stderr +
telemetry row + deduped macOS notification) — the same convention sibling
workers use.

**First tick baselines silently** — no alert storm on deploy; a spec first
observed already-terminal is not alerted.

**Failure stance (S6):**
- `~/.boi/v2/boi.db` **absent** → quiet no-op (debug-level at most). This worker
  ships in foundation and most instances never run BOI.
- `boi.db` **present but unreadable** → **loud** failure through the worker's
  normal failure path, so `hex failures` counts it.

This supersedes any ad-hoc BOI watching (e.g. the reverted standalone
`boi-spec-watch.py` launchd watcher) — recurring/scheduled work is a hex
worker, never a new LaunchAgent.

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

---

## `hex upgrade` — instance-repo consistency

### The deployed-but-orphaned blind spot

`hex upgrade` syncs foundation files into the instance's `.hex/` and rebuilds the
harness binary. Historically it stopped there: the synced source was left
**uncommitted** in the instance repo. Because `hex upgrade`'s own change detection
diffs the checked-out `.hex/` tree against the source, an already-synced but
uncommitted deploy reads as "nothing changed" — so a live, running deploy can sit
orphaned in git for days while every subsequent upgrade reports success and does
nothing. One instance ran a deployed-but-uncommitted sync for nine days this way.

### The fix: an automatic post-upgrade commit

After a **successful** sync AND rebuild, `hex upgrade` now commits the synced
tracked files so the repo reflects the deployed version (`commit_synced_files` in
`system/harness/src/upgrade.rs`). Properties:

- **Scoped to `.hex/`.** Only tracked changes under `.hex/` are staged
  (`git add -u -- .hex`). The operator's unrelated tracked work — `todo.md`,
  `me/`, `projects/`, `landings/` — is never swept into the upgrade commit.
- **Tracked-only.** New, untracked files are deliberately NOT added, so runtime
  state under `.hex/` (the per-run `.upgrade-backup-*` snapshot, `.hex/iii/data`,
  worker `node_modules`, `memory.db`) never lands in a bookkeeping commit.
- **Named by version.** The commit subject is
  `chore(hex): sync harness files to v<version>`, reading the version from
  `.hex/version.txt`.
- **Clean tree is a no-op.** If nothing under `.hex/` changed, the step prints
  "already consistent" and makes no commit — never an error.
- **Own repo only.** The commit is gated on the workspace being the top level of
  its OWN git work tree (`git rev-parse --show-toplevel` must equal `$HEX_DIR`), so
  a workspace nested inside some parent repo is never polluted. A workspace that is
  not a git repo at all is skipped with a visible note.
- **Fails loudly.** If the commit cannot be made in a repo where it should have
  succeeded, `hex upgrade` prints a `[FAIL]` to stderr stating the deploy is live
  but unrecorded in git, prints the exact manual `git` fix, and exits nonzero
  (Standing Order S6: no quiet failures). It is never a silent skip.
- **Cannot hang.** The commit runs with `commit.gpgsign=false` and `--no-verify`
  so an unattended upgrade can never block on a GPG passphrase or a pre-commit
  hook prompt.
- **Mid-merge/rebase caveat.** The commit is pathspec-scoped (`--only -- .hex`),
  which git refuses during an in-progress merge or rebase ("cannot do a partial
  commit during a merge"). If you run `hex upgrade` while the instance repo is
  mid-merge/rebase, the post-upgrade commit fails loudly and exits nonzero — the
  deploy is live, the tree is fine. Finish or abort the merge/rebase and run the
  printed manual `git` fix (or re-run `hex upgrade`) to record the synced files.

### Known limitation: instance-side gitignore shadowing of new harness source

An instance `.gitignore` commonly carries a blanket ignore rule for
`.hex/harness/src/` (and, in some instances, all of `.hex/`). When a foundation
upgrade introduces **new** harness source files under a shadowed path, `git add`
will not stage them — they are invisible to the tracked-only commit above and stay
orphaned even though the deploy is live. This is a genuine gap the automatic commit
cannot close on its own, because un-ignoring runtime state indiscriminately would
sweep backups and databases into history.

**Recommended policy:**

- Keep the blanket ignore narrow. Ignore runtime state precisely
  (`.hex/iii/data/`, `.hex/*.db`, `.hex/.upgrade-backup-*`, worker `node_modules`),
  not an entire subtree that also contains synced source.
- If `.hex/harness/src/` (or a broader `.hex/` subtree) must stay ignored, add a
  negation for the synced source you want tracked, e.g. an allow rule that
  re-includes `.hex/harness/src/` while the surrounding ignore stands, so new
  source files become trackable and the post-upgrade commit can pick them up.
- After an upgrade that adds new files, verify with
  `git -C "$HEX_DIR" status --porcelain --ignored -- .hex` that no synced source
  is sitting in the ignored set; if it is, adjust the ignore rule and commit the
  files by hand.
- Treat a first upgrade to a new version as the moment to reconcile the ignore
  rules — new source files ship with minor/major bumps, not patch syncs.
