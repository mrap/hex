# Code Intelligence (`cq`) — Operator Guide

`cq` is a stateless, index-backed code-intelligence CLI for Rust workspaces
(crate `system/code-intel`, package `scipd`). It answers `def` / `refs` /
`callers` / `symbols` / `search` queries from a SQLite index built by
`rust-analyzer scip`, with per-file git-blob freshness checks. The index fast
path has no daemon and no shared mutable state — safe for any number of
concurrent agents. Phase A2 adds an *optional* live escalation path through
the `scipd` daemon (see "Phase A2 — live escalation" below); everything keeps
working when the daemon is down.

Contracts: [docs/code-intel/SPEC-A1.md](code-intel/SPEC-A1.md) (index path) and
[docs/code-intel/SPEC-A2.md](code-intel/SPEC-A2.md) (live path). This page is
the how-to.

## Concepts

- **Workspace.** A registered Rust repo, identified by
  `workspace-id = first 12 hex chars of sha256(realpath of the primary
  checkout root)`. All state lives under `~/.codeintel/<workspace-id>/`
  (override the root with `$CODEINTEL_HOME`).
- **Generations.** Each `cq index` run publishes an immutable snapshot
  directory `<timestamp>-<rand>/{index.sqlite, manifest.json}`. Publish is
  atomic (tmp-dir rename, then atomic rewrite of the `CURRENT` pointer file);
  the 2 most recent generations are kept, older ones pruned. Readers opening
  the old generation mid-publish keep a consistent view — never a mixed one.
- **Freshness.** Every query compares the blob OIDs recorded at index time
  against the querying worktree's actual git state (`git ls-files -s` +
  `git diff --name-only`). Files that drifted appear in `stale_files`, their
  snippets are withheld, and the query exits 2. `--strict` refuses outright
  (exit 2, `STALE_RESULTS` on stderr). Stale is loud, never silent.
- **Worktrees.** Any `git worktree` of a registered workspace resolves to its
  parent workspace automatically (`git rev-parse --git-common-dir`) and
  queries the existing index instantly — no per-worktree state, cold start in
  milliseconds, teardown leaves zero residue. `cq index` always indexes the
  primary checkout, never a worktree.

## Walkthrough

```bash
cargo build --release -p scipd        # binary at target/release/cq
cq register ~/github.com/mrap/hex-foundation
# {"registered":"ab12cd34ef56","root":"/path/to/hex-foundation"}

cq index --workspace ~/github.com/mrap/hex-foundation
# Runs rust-analyzer scip over the primary checkout (~40s, ~3GB RSS on this
# repo), ingests into SQLite, publishes a generation. Concurrent invocation
# prints {"skipped":"emit-in-flight"} and exits 0 — visible, never doubled.

cd ~/github.com/mrap/hex-foundation   # or any worktree of it
cq def parse_proposal                 # by symbol name
cq def system/harness/src/main.rs:120:8   # or FILE:LINE:COL (1-based)
cq refs should_throttle
cq callers sha256_hex
cq symbols system/harness/src/ledger.rs
cq search gatekeep

cq doctor                             # health: index age, commit lag,
                                      # rust-analyzer presence; exit !=0 on red
```

Every query prints one JSON envelope on stdout:

```json
{
  "source": "index",
  "workspace_id": "ab12cd34ef56",
  "indexed_commit": "9b70565a…",
  "index_age_secs": 4210,
  "stale_files": [],
  "latency_ms": 14,
  "results": [ { "path": "…", "line": 120, "col": 8, "symbol": "…",
                 "display_name": "…", "kind": "function",
                 "role": "definition", "snippet": "fn consolidate(…" } ]
}
```

Lines/cols are 1-based. `snippet` is read from *your worktree* and only for
fresh files; stale files get no snippet and are listed in `stale_files`.

## Error codes (spec §5 — every error is structured JSON on stderr)

| Condition | stderr JSON `error.code` | exit |
|---|---|---|
| OK, all result files fresh | — | 0 |
| OK, but ≥1 result file stale (or `--strict` refused) | `STALE_RESULTS` annotation / refusal | 2 |
| No index / `CURRENT` missing / SQLite unopenable | `NO_INDEX` | 3 |
| CWD not in a registered workspace | `UNREGISTERED_WORKSPACE` | 4 |
| Workspace registered but not a Rust workspace | `UNSUPPORTED_WORKSPACE` | 4 |
| Symbol/position resolves to nothing | `NOT_FOUND` | 5 |
| Emit subprocess failed (`cq index`) | `EMIT_FAILED` + captured stderr tail | 6 |

`cq` never exits 0 with empty results due to an internal failure; unexpected
internal errors exit 1 with `error.code = "INTERNAL"`.

## Phase A2 — live escalation

A1 answers from an immutable index and flags `stale_files` it cannot speak
for. A2 adds the live path so those cases get real answers: the **`scipd`
daemon** owns a capped LRU pool of live rust-analyzer instances, each rooted
at exactly ONE worktree, and `cq` talks to it over a unix socket
(`~/.codeintel/scipd.sock`, newline-delimited JSON). Live truth is the **disk
state of the worktree** — live answers reflect what is on disk right now,
edited or not, committed or not.

### The daemon

```bash
cargo build --release -p scipd        # binaries at target/release/{cq,scipd}
mkdir -p ~/.codeintel/logs
sed -e "s|__SCIPD_BIN__|$HOME/github.com/mrap/hex-foundation/target/release/scipd|" \
    -e "s|__HOME__|$HOME|" \
    system/templates/launchd/com.hex.scipd.plist \
    > ~/Library/LaunchAgents/com.hex.scipd.plist
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.hex.scipd.plist
cq doctor                             # scipd section goes "ok" once the socket answers
```

One daemon per user, serving every registered workspace. KeepAlive restarts
it on crash; a second manually-started scipd refuses loudly if the socket is
already owned. Pool policy: instances are spawned on demand per worktree,
LRU-evicted past `pool_cap`, reaped when idle past the TTL or when their
worktree vanishes, and killed by a memory watchdog measuring **physical
footprint** (`footprint -p`; plain `ps` RSS under-reports an idle
rust-analyzer by >50x on macOS). Every pool transition is logged to stderr
(→ `scipd.err.log`) and visible in `cq doctor`'s scipd section.

### Escalation semantics (`source` / `escalated`, exit codes)

Query verbs (`def`/`refs`/`callers`) always compute the A1 index answer
first. If the target file or ≥1 result file is stale AND the daemon is
reachable, the query is retried against the live instance for that worktree:

| Situation | Envelope | Exit |
|---|---|---|
| Live instance ready | live answer, `"source":"live"`, no `escalated` | 0 |
| Instance still warming (priming) | index answer + `"escalated":{"reason":"warming","elapsed_secs":N,"workspace":…}` | A1 rules (stale → 2) |
| Daemon down / socket dead | index answer + `"escalated":{"reason":"daemon-unavailable","detail":…}` | A1 rules (stale → 2) |

`cq` **never blocks on warming** — a query during a prime returns in
milliseconds with the index answer and a loud `escalated` notice; re-run it
once the instance is warm (the real-repo prime is ~1-2 minutes). `--live`
forces the live path for any query (error `LIVE_UNAVAILABLE`, **exit 7**, if
impossible); `--no-live` forces pure A1 behavior and never touches the
socket.

New error rows on top of the A1 table:

| Condition | `error.code` | exit |
|---|---|---|
| Live path required but unavailable (`rename`, `--live`) | `LIVE_UNAVAILABLE` | 7 |
| Rename edit application aborted (content mismatch) | `RENAME_ABORTED` | 7 |
| `cargo check` itself failed to run | `CHECK_FAILED` | 8 |

### `cq rename` (always live, never from the index)

```bash
cq rename src/ops.rs:14:8 new_name           # print the edit plan, write nothing
cq rename src/ops.rs:14:8 new_name --apply   # apply it to the worktree
```

Emits the normalized WorkspaceEdit:
`{edits:[{path,line,col,end_line,end_col,old_text,new_text}],applied:…}`.
Apply is all-or-nothing: every edit's `old_text` is content-asserted against
the file first; any mismatch aborts the whole rename (`RENAME_ABORTED`, exit
7, zero files written). Daemon down or instance warming → `LIVE_UNAVAILABLE`,
exit 7 — retry once warm.

**Warning — macro-body blindness applies to live rename too:** rust-analyzer
does not rename tokens inside `macro_rules!` *bodies* (pinned by the golden
suite). Renaming a function that is called inside a macro body will update
the definition and every normal call site but leave the macro-body token
behind, **breaking compilation**. Grep for the symbol inside `macro_rules!`
blocks before applying; calls passed as macro *arguments* are renamed fine.

### `cq check`

```bash
cq check              # whole worktree
cq check src/ops.rs   # filter diagnostics to one file (exit still reflects the whole tree)
```

Runs `cargo check --message-format=json` in the querying worktree with
`CARGO_TARGET_DIR=<worktree>/target-cq` — per-worktree target dirs mean
concurrent checks in different worktrees never contend on a cargo lock. Add
`target-cq/` to `.gitignore` in repos you check often. Output:
`{diagnostics:[{path,line,col,level,code,message}],checked_in_ms}`. Exit 0
clean / 1 diagnostics present / 8 when cargo itself failed to run
(`CHECK_FAILED`).

### Config knobs (`~/.codeintel/scipd.toml`)

Missing file = defaults below; malformed file = loud startup failure (never
default-on-parse-failure). Defaults set from smoke test #3 (2026-06-11).

| Knob | Default | Behavior |
|---|---|---|
| `pool_cap` | 2 | LRU pool capacity; overflow evicts the least-recently-used instance (SIGTERM→SIGKILL), logged + visible in status |
| `idle_ttl_secs` | 1800 | reaper kills instances idle past the TTL |
| `mem_limit_mb` | 3500 | per-instance watchdog limit, measured as **physical footprint** (not `ps` RSS); over limit → kill + log; next query respawns |
| `pool_alarm_mb` | 7000 | pool-wide footprint alarm — log + status note only, no kill |
| `spawn_grace_secs` | 180 | no memory kill within this window after spawn (priming spikes are expected) |
| `warm_fallback_secs` | 240 | if rust-analyzer never reports quiescent (build-script-heavy repos), probe with a cheap request after this long; a successful probe promotes to Ready — never Ready on time alone |

Not configurable by design: vanish reap (always on) and any
"max warm wait" (cq never blocks on warming).

### Troubleshooting (live path)

Run `cq doctor` first — its `scipd` section reports socket reachability,
pool occupancy, and per-instance `{worktree, state, rss_mb, age}`. Daemon
unreachable is a **warning** (A1 still works), not red — unless launchd
claims the agent is loaded while the socket is dead ("scipd loaded but
socket dead", red: check `scipd.err.log` and
`launchctl print gui/$(id -u)/com.hex.scipd`).

| Symptom | Cause / fix |
|---|---|
| `escalated.reason:"warming"` on every query | Normal during a prime — a real repo takes ~1-2 minutes to warm (the fixture takes seconds). Keep working; the index answer you got is still correct for the indexed commit. If warming persists past ~5 minutes, check `scipd.err.log` (the `warm_fallback_secs` probe should have fired at 240s). |
| `escalated.reason:"daemon-unavailable"` | scipd isn't running or the socket is dead. `launchctl print gui/$(id -u)/com.hex.scipd`, then `tail ~/.codeintel/logs/scipd.err.log`. A1 answers keep flowing meanwhile. |
| `LIVE_UNAVAILABLE` (exit 7) on rename | Rename is live-only. Start the daemon, or wait out the warming and retry. |
| `RENAME_ABORTED` (exit 7) | A file changed between planning and applying the rename; nothing was written. Re-run the rename against the current content. |
| `CHECK_FAILED` (exit 8) | cargo itself failed to launch (not compile errors — those are exit 1). Check PATH; the error JSON carries the spawn failure. |
| Instance keeps getting killed | Memory watchdog: footprint over `mem_limit_mb` (default 3500). Raise the limit in `scipd.toml` or accept respawn-per-burst. Kills within 180s of spawn never happen (grace). |
| scipd restart loop in `scipd.err.log` | Malformed `scipd.toml` is a fatal startup error and KeepAlive keeps relaunching. Fix or delete the file. |

## Scheduling (nightly reindex, 02:30)

Nightly indexing runs via the harness worker **`hex-codeintel-indexer`**
(`system/harness/src/modules/code_intel.worker.rs`), cron `0 30 2 * * * *`
(02:30 daily). It loads the registry from `$CODEINTEL_HOME` (default
`~/.codeintel`) and indexes **every registered workspace sequentially** —
emits are ~3GB transient RSS each, so they never run concurrently. A
`SkippedInFlight` is a visible log line, not an error; a per-workspace
failure is logged loudly and the run continues to the remaining workspaces,
then errors with a summary (telemetry picks it up — no silent partial
success).

No per-workspace plist installs: register workspaces with `cq register
<path>` and the worker picks them up. The module is deployed with
`hex harness restart` after an upgrade (rebuild = install). Verify with
`hex module status hex-codeintel-indexer`.

Categorical distinction: scheduled **jobs** are hex workers (typed Rust, foundation
registry), not launchd. scipd is an **existing** long-running daemon documented here as-is; its launchd
plist (`system/templates/launchd/com.hex.scipd.plist`) and the install steps above are
unchanged. **New** long-running daemons should ride the engine via **iii-exec** (see the
AGENTS.md "Automation" rule), not a new plist.

## Known limitation: calls inside `macro_rules!` bodies

Call sites that live **inside a `macro_rules!` body** emit no SCIP occurrences,
so the calling function is invisible to `cq callers` (pinned by the golden
suite: `macro_caller` is asserted ABSENT from `callers(double)`; gate record in
`system/code-intel/tests/fixtures/callers-gate.json`). Calls passed **as macro
arguments** (`assert!(foo())`, `format!("{}", bar())`, `anyhow!`,
`tokio::spawn(async { … })`) keep their spans and ARE captured — measured 0%
false negatives on macro-heavy ground truth, which is why `callers` ships with
no `quality` flag. If you suspect a macro-bodied caller, fall back to `grep`
for that one edge.

## Troubleshooting

Run `cq doctor` first — it reports per-workspace index age, commit lag, last
emit status, generation list, and whether `rust-analyzer` is on PATH, and exits
nonzero with explicit `red_reasons` when anything is wrong.

| Symptom | Cause / fix |
|---|---|
| `UNREGISTERED_WORKSPACE` (exit 4) | CWD isn't inside a registered repo. `cq register <path>` (or pass `--workspace`). |
| `NO_INDEX` (exit 3) | Registered but never indexed, or store damaged. Run `cq index --workspace <path>`. |
| Exit 2 + `stale_files` | Your worktree drifted from the indexed commit (edits or different checkout). Results are still correct *positions for the indexed commit*; reindex to clear. |
| `EMIT_FAILED` (exit 6) | `rust-analyzer scip` crashed; stderr tail is in the error JSON and the failed `<gen>.tmp/` dir is kept for post-mortem. Check rust-analyzer version (`cq doctor`). |
| `{"skipped":"emit-in-flight"}` | Another `cq index` holds the flock. Wait for it; nothing was lost. |
| Doctor red: index older than 7 days | The nightly harness worker isn't running — check `hex harness status`, `hex module status hex-codeintel-indexer`, and `hex telemetry` for failed runs. |
| Slow first query after reboot | Cold page cache on `index.sqlite`; subsequent queries are warm (<500ms p95 budget). |

## E2E acceptance

`tests/e2e/code-intel-e2e.sh` proves SPEC-A1 S3–S8 against hex-foundation
itself; `tests/e2e/code-intel-live-e2e.sh` proves SPEC-A2 S5/S7/S8 (live
escalation, daemon-down degradation, `cq check`) the same way, starting its
own scipd against a throwaway home. Both are hermetic (clone to /tmp,
throwaway `CODEINTEL_HOME`) and gated — run manually, not part of unit CI:

```bash
bash tests/e2e/code-intel-e2e.sh
bash tests/e2e/code-intel-live-e2e.sh   # pays a real rust-analyzer prime (~2 min)
```
