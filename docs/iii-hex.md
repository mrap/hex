# hex on iii — the abstraction layer (primer)

**What this is:** how *hex* uses iii — our command surfaces, our conventions, and the
one canonical producer→consumer flow. **What this is NOT:** a generic iii reference.
For iii itself (functions, triggers, state, queues, SDKs) use the installed `iii-*`
skills — `iii-getting-started`, `iii-functions-and-triggers`, `iii-trigger-schemas`,
`iii-state-reactions`, `iii-queue-processing`, `iii-state-management`. This doc only
covers the hex layer on top.

> Status: the **concepts and conventions** below are stable. The CLI shipped as
> `hex module …` (worker lifecycle) + `hex triggers …` (event production) — NOT the
> `hex worker run` form an earlier draft assumed (that YAML worker host was never built).
> Verify exact flags against `hex module --help` / `hex triggers --help`.

---

## The mental model (the part worth memorizing)

iii is three nouns and a set of channels.

| Noun | One line | hex example |
|---|---|---|
| **Function** | a unit of work — "do this" | `hex::landings::reconcile` |
| **Trigger** | a *subscription*: "fire function F when X happens." Registered **once**, when the consumer sets up. Has a **type**. | a `state` trigger watching scope `boi` |
| **Worker** | a typed-Rust unit binding a function to a trigger, run in-process by the harness engine | `Worker::new("hex-backup").on_cron_named(...)`; inspect via `hex module list` |

A **trigger's type is the channel it listens on:**

| Type | Fires when… | Semantics |
|---|---|---|
| `cron` | the clock hits a schedule | time-driven |
| `state` | a KV key changes | **current value** (last-write-wins) |
| `queue` | a message lands on a named queue | discrete, durable, FIFO |
| `pubsub` | a topic is published | broadcast, fire-and-forget |
| `stream`/`http`/`log` | other sources | — |

**Producing an event = writing to the channel the trigger listens on** (set state /
enqueue / publish). The producer doesn't call the consumer; it writes a channel, and
whatever trigger is listening there fires. That indirection **is** the decoupling —
the whole point of reactive ops ("when X, do Y" without X knowing about Y).

**Static vs dynamic — where attribution lives.** A trigger can carry `metadata`, but
it's **static** (set at registration, same every firing — describes the *subscription*).
Per-event data (who produced it, the payload) is **dynamic** and rides in the **channel
payload**, never on the trigger. So "who emitted this" goes in the event value, not the
trigger.

---

## The hex layer

We do **not** scatter `iii_sdk::` calls across hex. One seam owns iii.

- **`system/harness/src/ops.rs`** — the *only* place (besides the worker host) that calls
  `iii_sdk::`. Exposes hex-native `emit(...)`, state read/write, connect. If iii's API
  changes or we swap substrate, only this file changes.
- **Workers are typed Rust, not YAML.** Each is a `Worker` (`system/harness/src/worker/mod.rs`)
  binding a function to a trigger — e.g. `Worker::new("hex-backup").on_cron_named("daily",
  CRON_DAILY, run_backup)` (`system/harness/src/modules/backup.worker.rs`). They are collected
  in `hex_modules::module_registry()` (surfaced by `hex::workers::registry()`) and run
  in-process by the harness engine (`hex harness serve`) — no node, no per-worker binary.
- **`hex module list | status <name> | enable <name> | disable <name>`** — worker (module)
  lifecycle. There is **no `hex worker run`**; an earlier draft of this doc described a
  `{ worker_name, jobs }` YAML host that was never built.
- **`hex triggers emit <event> [--data <json>] [--producer <name>]`** — the producer.
  One `iii.trigger(...)` call under the hood; writes the event onto a channel so reactive
  workers fire. Shell/hook callers use the CLI; Rust callers use `ops::emit(...)` directly
  — same code path, the CLI is just `clap` + the lib.

### Authoring a worker (real model)

A worker is typed Rust — bind a function to a trigger via the `Worker` builder, then register
it so `hex harness serve` runs it:

```rust
// system/harness/src/modules/backup.worker.rs (illustrative)
Worker::new("hex-backup")
    .on_cron_named("daily", CRON_DAILY, run_backup)   // cron trigger (7-field expr)
// .on_event(...) binds a state/queue trigger instead
```

Add it to `hex_modules::module_registry()` so the engine schedules it. The bound function
receives the trigger event as a typed argument (not an env var). Authoring a new worker is a
**foundation change** (it compiles into the harness), not a config edit; inspect/pause running
workers with `hex module list` / `hex module disable <name>`.

---

## Our conventions

- **Channel for events: `state`** (for now). We enrich the value with a producer envelope
  (iii does *not* surface the writer to the consumer — `StateCallRequest` is only
  `{event_type, scope, key, old_value, new_value}`):

  ```jsonc
  // state value at scope=<ns>, key=<event>  →  consumer reads III_EVENT.new_value
  { "event": "boi.spec.complete", "producer": "boi-completion-hook",
    "ts": "2026-06-05T00:31:00Z", "data": { "spec_id": "S5bke4kkf" } }
  ```

- **Decouple on the event *name*, not the function.** Producers emit a fact
  (`boi.spec.complete`); consumers bind a trigger to that scope/key. Neither imports the
  other. Adding a new reactive consumer never edits the producer.

- **Known caveat — state is last-write-wins.** Two emits to the same `scope/key` between
  trigger fires: the first is clobbered. The envelope gives attribution, not delivery
  guarantees. When loss matters for discrete events, **shard the key** (e.g.
  `key = "boi.spec.complete/<spec_id>"`) so distinct events don't collide, or move that
  event to a `queue` trigger (durable, FIFO). Decide per-event; don't blanket-upgrade.

---

## The canonical flow

```
PRODUCER                          CHANNEL              CONSUMER
hex triggers emit  ──writes──►   iii STATE    ──►   TRIGGER (type=state)  ──fires──►  FUNCTION
  boi.spec.complete                key=event                                          hex::landings::reconcile
  {event,producer,ts,data}                                                            reads III_EVENT.new_value
```

- Producer: a hook/wrapper (or `ops::emit` from Rust) — does **not** know about landings.
- Channel: iii state, keyed by the event name.
- Consumer: the landings worker, whose `state` trigger was registered once at startup.

---

## Instance engine workers (`engine-workers.yaml`)

The harness's in-process engine builds from `EngineConfig::default_config()` — it reads no
config file. Instances extend it declaratively: the harness merges the `workers:` list from
`$HEX_DIR/.hex/iii/engine-workers.yaml` (instance-owned; `.hex/iii/` is an additive upgrade
dir, so the file survives `/hex-upgrade`) into the engine config at boot. Merge semantics:
a name matching a default module/worker **replaces it in place** (how an instance
reconfigures a default — e.g. `iii-observability` → memory exporter so the console trace
explorer has data); other names append. Restart the harness to apply. Malformed file →
loud skip (stderr + alert), never a harness crash-loop.

**This is the no-LaunchAgents path for persistent local processes.** Declare an `iii-exec`
entry whose final command is the daemon (e.g. the iii console UI); the engine supervises it —
own session, stdout/stderr into the harness log, process-group SIGTERM→SIGKILL on shutdown.
Template: `system/iii/engine-workers.example.yaml` (foundation ships ONLY the example —
a real `engine-workers.yaml` in foundation would clobber every instance's copy on upgrade).

Known limit: `iii-exec` does not respawn a daemon that dies on its own (only watch-glob
restarts + engine lifecycle). Pair critical daemons with a doctor check.

---

## Guardrails (carry from the iii decisions)

- iii is the **additive default** for new mechanisms — not a migration of BOI/launchd/
  memory/harness (`me/decisions/iii-additive-default-substrate-*`).
- **Engine-down must fail loud (S6)**, never silently no-op. iii is a SPOF for anything on it.
- Each iii mechanism is **independently disablable**.
- See also: `me/decisions/build-operations-on-iii-2026-06-04.md`,
  `me/decisions/hex-abstraction-over-iii-2026-06-04.md`.
