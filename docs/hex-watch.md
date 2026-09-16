# hex-watch: interface and behavior spec

**Canonical.** Every other mention of hex-watch, session notify, or hex events points here and does not restate. Change this file first, then the code and tests (`system/harness/src/watch/`, `system/harness/src/modules/watch.worker.rs`, `system/harness/tests/watch_cli.rs`).

Purpose: hex owns the wait (Standing Order S10). A watch is "when X happens, do Y once, loudly, and never wait forever." Any hex install gets it; every watcher is visible and managed; every outcome is an event other parts of hex can react to.

---

## 1. Vocabulary

| Term | Meaning |
|---|---|
| Watch | One record: a source, a match, an action, a window (`since` to `expires`). Fires at most once. |
| Source | Where events come from. An adapter polls it. Today: `gmail`, `event`. |
| Match | Source-specific fields that select the event (`query` + `account` for gmail; `event` for event). |
| Hit | One matching event from a poll: `key` (stable dedupe id), `at_ms` (when it happened, epoch ms, 0 = unknown), `fields` (strings for the action's env). |
| Action | A shell command run once on the first valid hit, or a session notify. |
| Outcome | `fired`, `failed`, or `expired`. Every outcome is emitted as a hex event. |
| hex event | A named fact on the iii state substrate: `hex triggers emit <name>` writes `events/<name>`; anything can read or wait on it. |
| Session inbox | Per-session file the session reads on its next turn. How events reach a running agent. |

---

## 2. Interface

### Commands (`hex watch`)

| Command | Behavior |
|---|---|
| `add (--action CMD \| --notify SESSION [--nudge]) [--source gmail\|event] [--match K=V ...] [--query Q] [--account primary\|legacy] [--note TEXT] [--since ISO] [--expires 14d]` | Create a pending watch. Prints the 8-char id. Exit 2 without an action or notify, without a valid match for the source, or with a bad duration; nothing is persisted. |
| `list [--all]` | Every live watch (`pending`, `firing`, `failed`, `expired`) with age, time to expiry, last outcome, note, source, match. Flags watches stuck in `firing`. `--all` includes `done`. |
| `status` | One line: counts by status, next expiry, last tick, poll fail streak. |
| `tick [--dry-run]` | One pass over pending watches. Dry run prints what would fire and runs nothing. Exit 1 when any poll failed. |
| `retry ID` | `failed`, `firing`, or `expired` back to `pending`; same `since`, same `expires`. Exit 2 otherwise. |
| `done ID` | Close by hand. |
| `drop ID` | Delete the record. |
| `notify SESSION\|all TEXT [--now]` | Session delivery (below). |
| `inbox [--peek] [--session NAME]` | Print and clear this session's pending events. |

The loop is the `hex-watch` harness worker: cron `0 */5 * * * * *`, in-process `tick`, one telemetry row per fire, `status=error` when any poll failed so `hex failures` sees a streak. `hex module list` shows it.

### Sources

| Source | Match fields | Producer side | Hit key / time |
|---|---|---|---|
| `gmail` | `query` (Gmail search), `account` (`primary` default, `legacy`) | An email arrives. | Gmail message id / `internalDate`. `after:<since epoch>` is appended to the query server-side. |
| `event` | `event` (name) | `hex triggers emit <name> --data '{...}' [--producer P]` from any process, worker, or agent. | `<name>@<ts>` / envelope `ts`. Reads iii state `events/<name>`. |

Gmail is reached through a configured command (`watch.toml`, below) that prints one JSON object per hit: `{id, internal_ms, date, from, subject, account}`. The default is the instance's `gmail-search --json`. The personal Gmail wiring stays in the instance; foundation only knows the contract.

Adapter contract, for new sources: a `poll` in `watch/sources.rs` plus a `tick::poll` arm. An adapter narrows at the source where it can and errors on transport failures. It never decides "old"; the loop does.

### Action contract

- Runs once through `/bin/sh -c`, exit 0 = `done`, anything else = `failed`, killed after `action_timeout_secs` (exit 124).
- Env is allowlisted, never the daemon's full environment: `PATH`, `HOME`, `USER`, `LANG`, `TMPDIR`, `GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND` (default `file`), `HEX_DIR`, anything in `[action] env_passthrough`, plus `WATCH_ID`, `WATCH_SOURCE`, `WATCH_KEY`, `WATCH_AT` (epoch ms), `WATCH_NOTE`, and `WATCH_<FIELD>` for every hit field (upper-cased, non-alphanumerics to `_`). Gmail adds `MAIL_MSG_ID`, `MAIL_SUBJECT`, `MAIL_FROM`, `MAIL_DATE`, `MAIL_INTERNAL_MS`, `MAIL_ACCOUNT`. Event adds `WATCH_EVENT`, `WATCH_PRODUCER`, `WATCH_TS`, `WATCH_DATA` (JSON), `WATCH_DATA_<KEY>` per top-level data key.
- `--notify SESSION` with no `--action` sets the action to `"$HEX_DIR/.hex/bin/hex" watch notify SESSION "watch $WATCH_ID fired ($WATCH_SOURCE $WATCH_KEY): $WATCH_NOTE"`. `--nudge` appends `--now`.

### Emitted events

| Event | When | Data |
|---|---|---|
| `hex.watch.fired` | Action exited 0 | `id, note, source, match, outcome, since, expires, hit{key, at_ms, fields}` |
| `hex.watch.failed` | Action non-zero, timeout, or config error | same plus `error` |
| `hex.watch.expired` | Window lapsed with no valid hit | same, no hit |

Producer is `hex-watch`. Any watch can chain: `--source event --match event=hex.watch.fired`. From the worker, emits ride the harness `Ctx` (outbox during a drain); from the CLI, `ops::emit`.

### Session delivery

| Command | Behavior |
|---|---|
| `hex watch notify SESSION\|all "TEXT" [--now]` | Appends `- <ts> TEXT` to `$HEX_DIR/.hex/run/inbox/<session>.md`. `all` = every live tmux session. `--now` also types `hex event for this session: TEXT` plus Enter into that tmux session. Exit 1 if an inbox write or a requested send fails; a missing tmux session with `--now` is a warning, inbox only. |
| `hex watch inbox [--peek] [--session NAME]` | Prints the session's inbox inside a `*** hex events for session "<name>" ... ***` banner and clears it (`--peek` keeps it). Session from `--session`, else `HEX_SESSION_NAME`, else the tmux session; none = quiet no-op. |
| Hooks | `hex hook user-prompt-submit` appends the same banner to `additionalContext` on every prompt. `hex watch inbox` is a required `SessionStart` hook (`system/hooks/required-hooks.json`). |

`--now` is opt-in only: typed text arrives as a user prompt (an email subject would become a prompt), submits anything half-typed in that pane, and duplicates the inbox line. The banner path is the safe default and marks the lines as events, not the operator typing.

### Config: `$HEX_DIR/.hex/config/watch.toml`

```toml
default_expires = "14d"        # `hex watch add` default window
action_timeout_secs = 600
[sources.gmail]
command = "\"$HEX_DIR/.hex/bin/gmail-search\" --account {account} --json {query} 5"
[action]
env_passthrough = []           # extra parent-env names the action may see
```

Missing file = defaults. Malformed or unknown key = loud error, every command exits 2.

---

## 3. Behavior guarantees

| Guarantee | How |
|---|---|
| At most once | The record is saved as `firing` before the action runs. A crash or restart mid-action leaves it `firing`; it is never re-run, `list` flags it, `retry` is the human path back. |
| Never before `since` | `since` is stamped at add time (or `--since`). The adapter narrows at the source; the loop then rejects any hit with `at_ms < since`. A hit with unknown time (`at_ms = 0`) is trusted and logged. |
| Never forever | `expires` = add time + duration (default 14d; `--since` does not shorten it). On lapse: status `expired`, alert, `hex hitl` item (project `hex-ops`, P2), `hex.watch.expired` emitted. Exactly once, and never polled. |
| Always visible | Nothing runs that `list` cannot show. `status` is the dashboard line. `hex module list` shows the worker; telemetry rows show every tick. |
| Loud failures (S6) | Poll errors log every tick and make the worker fire `status=error`; 3 failing ticks in a row alert once (`alert::notify`, key `watch-poll-streak`), a clean tick resets and clears the stamp. Config errors (unknown source, missing match) fail that watch once with an alert and stay out of the poll streak. Action failure: `failed`, stderr tail stored, alert. Emit or hitl failure: logged, never blocks the watch. A malformed item file or config is a loud error, never a skipped row. |
| No lost adds | One file per watch. An `add` during a tick lands in its own file; the tick never rewrites files it did not touch. |
| Backward compatible records | The Python v0.x queue (`.hex/run/hex-watch/watches.jsonl`) is imported once on the first tick and renamed `.imported`. v1 records (top-level `query`, no `since`/`expires`) load as gmail, get no since guard, never expire. |

Status enum: `pending -> firing -> done | failed`, `pending -> expired`. `retry` returns `failed`, `firing`, `expired` to `pending`.

---

## 4. Storage

```
$HEX_DIR/.hex/watch/
  items/<id>.json     one file per watch, atomic tmp+rename
  state.json          {"poll_fail_streak": n, "last_tick": ts}
  log.jsonl           append-only transitions: add, import-v1, firing, done, failed, expired, retry, drop
$HEX_DIR/.hex/run/inbox/<session>.md   session inbox
iii state events/<name>                newest envelope {event, producer, ts, data} per event name
```

Record: `{id, source, match, action, note, status, created, since, expires, fired, key, error?, expired_at?, retried?}`, RFC 3339 UTC timestamps.

---

## 5. Known limits

1. Polling, 5 min. Latency 0 to 5 min for every source, including internal events.
2. Events are last-write-wins per name. Two emits between ticks collapse to the newest. Shard the name (`deploy.done/<id>`) when each one matters.
3. `--now` / `--nudge` turns event text into a user prompt in that pane and submits anything half-typed there. Off by default; only for text hex controls.
4. Action is a shell string. The env allowlist bounds what it can read; it does not bound what it can run.
5. When the iii engine is unreachable, each emit waits out the SDK timeout (about 30 s) before it is logged as failed. The worker runs inside the harness next to the engine, so this only bites `hex watch tick` run by hand with the harness down.
6. Gmail only for external sources. HTTP, file, calendar, iMessage adapters are open.

---

## 6. Tests

- `cargo test -p hex-harness --lib watch::` (loop, store, sources, notify, config; fakes for iii, shell, alerts)
- `cargo test -p hex-harness --test watch_cli` (spawned binary: argv, exit codes, files, hook drain)
- Worker registration: `workers::hex_modules::watch::tests`

A bug fix starts with a failing test that cites the incident (`docs/testing-standard.md`).

## 7. History

- 2026-09-16 v0.1 to v0.3: Python `mail-watch` then `hex-watch` in the mrap instance (`.hex/bin`), with a CTO review and a 22-test suite.
- 2026-09-16 v1.0: native `hex watch` in the hex binary, `hex-watch` harness worker, iii state for events, one file per watch, env allowlist, session inbox in the hooks. Decision `hex-watch-native-rust-iii-2026-09-16` (instance).
