# hex-events Reference

hex-events is a SQLite-backed event-driven policy engine using the
Event-Condition-Action (ECA) model. A persistent daemon polls for new events,
matches them against YAML policies, evaluates conditions, and fires actions.

**Location:** `~/.hex-events/`
**Test command:** `cd ~/.hex-events && python -m pytest tests/ -q`

---

## Event Lifecycle

```
hex_emit.py ──► INSERT events table
                    │
                    │  (daemon polls every 2s)
                    ▼
               get_unprocessed()
                    │
                    ▼
               MATCH: rule.trigger_event (glob)
                    │
                    ▼
               FILTER: evaluate_conditions() [AND logic]
                    │
                    ▼
               RATE: check_rate_limit() [per-policy window]
                    │
                    ▼
               EXECUTE: actions in order [retry 3× exp backoff: 1s,2s,4s]
                    │
                    ▼
               LOG: action_log row per action
                    │
                    ▼
               MARK: events.processed_at = now
```

Deferred events follow a parallel path:

```
emit action (delay:) ──► INSERT deferred_events (fire_at = now + delay)
                              │
                              │  (daemon drain loop, each poll)
                              ▼
                         fire_at <= now?
                              │
                              ▼
                         DELETE from deferred_events
                              │
                              ▼
                         INSERT events table  ──► normal lifecycle
```

---

## Policy YAML Schema

```yaml
# ~/.hex-events/policies/example.yaml
name: example-policy             # required, unique across all policies
description: "What this does"   # required

standing_orders: ["9", "12"]    # optional: CLAUDE.md standing order refs
reflection_ids: [R-033]         # optional: incident/reflection refs

provides:
  events:
    - landings.check-due        # events emitted by this policy
    - policy.violation

requires:
  events:
    - git.commit                # events consumed/triggered by this policy
    - landings.updated

rate_limit:                     # optional — prevents fork-bombs
  max_fires: 10
  window: 1m                    # duration string

max_fires: 5                    # optional — disable policy after N total fires
auto_cleanup: disable           # optional — what to do when a limit (ttl or max_fires) is hit
                                #   disable (default): set enabled: false, leave file on disk
                                #   delete: remove the YAML file from disk

rules:
  - name: rule-name             # required
    trigger:
      event: git.commit         # glob patterns supported (e.g., git.*, boi.*)
    ttl: 7d                     # optional — stop firing this rule after this duration from first fire
    conditions:                 # optional; all must pass (AND logic)
      - field: branch
        op: eq
        value: main
      - field: "count(git.commit, 5m)"
        op: lte
        value: 10
    actions:
      - type: emit
        event: landings.check-due
        delay: 10m
        cancel_group: landings-check
      - type: shell
        command: "echo '{{ event.branch }}'"
        timeout: 60
        retries: 3
      - type: notify
        message: "Commit on {{ event.branch }}"
      - type: update-file
        target: /path/to/file
        pattern: "OLD_VALUE"
        replace: "NEW_VALUE"
```

### `auto_cleanup` — file lifecycle on limit

`auto_cleanup` controls what happens to the policy **file** when a limit is reached
(`max_fires` exhausted or a rule's `ttl` expires). It is **orthogonal** to `ttl`,
`max_fires`, and `lifecycle` — any combination is valid.

| Value | Behavior |
|-------|----------|
| `disable` | Set `enabled: false` in the file. File stays on disk. *(default)* |
| `delete` | Remove the YAML file from disk entirely. |

When neither `ttl` nor `max_fires` is set, `auto_cleanup` is a no-op — there is
nothing to trigger it.

**Ephemeral policy — delete after TTL expires:**

```yaml
name: temp-branch-watch
description: "Notify once per day about stale branches, auto-removes after 30 days"
auto_cleanup: delete

rules:
  - name: daily-check
    trigger:
      event: timer.tick.daily.9am
    ttl: 30d
    actions:
      - type: notify
        message: "Stale branch check triggered"
```

When any rule's `ttl` expires and `auto_cleanup: delete` is set, the policy file
is removed from disk. No accumulation of dead `enabled: false` files.

**Bounded policy — delete after N fires:**

```yaml
name: onboard-reminder
description: "Send 3 onboarding nudges then self-destruct"
max_fires: 3
auto_cleanup: delete

rules:
  - name: nudge
    trigger:
      event: timer.tick.daily.9am
    actions:
      - type: notify
        message: "Onboarding reminder: check your setup"
```

After firing 3 times, the file is deleted automatically.

**Composing both limits — whichever triggers first wins:**

```yaml
name: sprint-alerts
description: "Alert up to 10 times or for 2 weeks, whichever comes first"
max_fires: 10
auto_cleanup: delete

rules:
  - name: alert
    trigger:
      event: ci.failure
    ttl: 14d
    actions:
      - type: notify
        message: "CI failed: {{ event.branch }}"
```

`auto_cleanup: delete` fires as soon as either limit is hit.

### Old recipe format (still supported)

```yaml
name: my-recipe
trigger:
  event: git.push
actions:
  - type: shell
    command: "..."
# provides/requires are inferred from trigger + emit actions
```

Old-format files are auto-wrapped as single-rule policies on load. Prefer the
new format for new policies.

---

## Conditions

Conditions use AND logic — all must pass for the rule to fire.
Field values come from the event payload. Missing fields → condition fails.

### Scalar operators

| Operator | Meaning              | Example                     |
|----------|----------------------|-----------------------------|
| `eq`     | equal                | `op: eq`, `value: main`     |
| `neq`    | not equal            | `op: neq`, `value: skip`    |
| `gt`     | greater than         | `op: gt`, `value: 5`        |
| `gte`    | greater than or equal| `op: gte`, `value: 1`       |
| `lt`     | less than            | `op: lt`, `value: 100`      |
| `lte`    | less than or equal   | `op: lte`, `value: 10`      |
| `contains` | substring match    | `op: contains`, `value: "err"` |

### `count()` function

```yaml
- field: "count(event_type, duration)"
  op: lte
  value: 10
```

Counts events of `event_type` in the DB within the given time window.
Uses duration strings (`10s`, `5m`, `2h`, `7d`). Returns an integer.

```yaml
# Example: fire only if fewer than 5 failures in the last hour
- field: "count(boi.spec.failed, 1h)"
  op: lt
  value: 5
```

### Duration strings

| Format | Meaning        | Seconds  |
|--------|----------------|----------|
| `30s`  | 30 seconds     | 30       |
| `5m`   | 5 minutes      | 300      |
| `2h`   | 2 hours        | 7200     |
| `7d`   | 7 days         | 604800   |

A bare integer (e.g. `"1"`) is treated as hours for backwards compatibility.

---

## Action Types

All action `command`/`message`/`target`/`replace` fields support Jinja2
templates with `{{ event.field_name }}` and `{{ action.field_name }}`.

### `shell`

Run a shell command.

```yaml
- type: shell
  command: "bash ~/.hex-events/scripts/verify.sh '{{ event.spec_id }}'"
  timeout: 60          # seconds, default 60
  retries: 3           # retry attempts on failure, default 3
  on_success:
    - type: emit
      event: verify.passed
      payload:
        spec_id: "{{ event.spec_id }}"
  on_failure:
    - type: emit
      event: verify.failed
      payload:
        error: "{{ action.stderr }}"
```

`on_success` and `on_failure` are lists of sub-actions executed after the
shell command completes. They have access to `{{ action.stdout }}` and
`{{ action.stderr }}`.

### `emit`

Emit a new event immediately or with a delay.

```yaml
# Immediate
- type: emit
  event: boi.spec.completed
  payload:
    spec_id: "{{ event.spec_id }}"

# Deferred (debounced)
- type: emit
  event: landings.check-due
  delay: 10m
  cancel_group: landings-check   # replaces any pending event in this group
```

**`cancel_group` (debounce pattern):** When a deferred emit specifies
`cancel_group`, any existing deferred event with the same group is deleted
before inserting the new one. This implements a debounce — only the most
recent deferred event fires.

```yaml
# Deadline pattern: arm a check; cancel if landings update arrives first
# Rule 1 (on git.commit): arm check 10m out
- type: emit
  event: check.due
  delay: 10m
  cancel_group: check-gate

# Rule 2 (on landings.updated): cancel the pending check
- type: emit
  event: check.cancelled
  cancel_group: check-gate  # deletes the pending check.due
```

### `notify`

Send a macOS notification (delegates to `~/.claude/scripts/hex-notify.sh`).

```yaml
- type: notify
  message: "BOI spec {{ event.spec_id }} completed"
```

### `update-file`

Regex find-and-replace on a file. Atomic write (tmp → rename).

```yaml
- type: update-file
  target: "/path/to/file.md"
  pattern: "LAST_RUN: .*"
  replace: "LAST_RUN: {{ event.timestamp }}"
```

---

## Scheduler Adapter

The scheduler adapter emits timer events at cron intervals.
Configured in `~/.hex-events/adapters/scheduler.yaml`.

```yaml
schedules:
  - name: 30m-tick
    cron: "*/30 * * * *"
    event: timer.tick.30m

  - name: daily-9am
    cron: "0 9 * * *"
    event: timer.tick.daily.9am
```

**Dedup:** Each emission uses `dedup_key = "event_type:YYYY-MM-DDTHH:MM"`.
If the daemon polls more frequently than the cron period, the event fires
exactly once per window.

**Startup catch-up:** On daemon start, one missed tick per schedule is emitted
(the most recent). Does not emit multiple ticks for long outages — one tick
regardless of how many were missed (prevents restart storms).

**Payload:** `{"scheduled_at": "2026-03-23T09:00"}` (plus `"catchup": true`
for catch-up ticks).

**External adapters** (fswatch, git hooks) emit events by calling `hex_emit.py`
directly. Declare them in `scheduler.yaml` under entries without a `cron` key
so they appear in the static validation graph.

---

## Static Validation

### `hex-events validate`

Loads all policies and adapter configs, builds the event dependency graph,
and checks:

1. **Unsatisfied requirements** (error): a `requires.events` entry has no
   provider (policy or adapter)
2. **Orphan provides** (warning): an event is provided but no policy consumes it
3. **Cycles** (error): circular event dependency chain

```bash
cd ~/.hex-events && python hex_events_cli.py validate
```

### `hex-events graph [--observed]`

Without `--observed`: prints the static event dependency graph from policy
declarations.

With `--observed`: queries the DB for the last 7 days of events and action_log
entries, then compares against the static graph. Highlights:
- Events declared but never observed
- Events observed but not declared (undocumented sources)

```bash
python hex_events_cli.py graph
python hex_events_cli.py graph --observed
```

---

## CLI Reference

All commands use `~/.hex-events/events.db` by default.

### `hex_emit.py` — emit an event

```bash
python ~/.hex-events/hex_emit.py <event_type> [payload_json] [source]
python ~/.hex-events/hex_emit.py --db /path/to/db <event_type> [json] [source]

# Examples
python ~/.hex-events/hex_emit.py boi.spec.completed '{"spec_id":"q-99"}'
python ~/.hex-events/hex_emit.py git.commit '{"branch":"main"}' fswatch
```

Designed to be fast: no policy loading, no daemon. INSERT and exit.

### `hex_events_cli.py` — query and debug

```bash
python ~/.hex-events/hex_events_cli.py <command> [options]
```

| Command | Description |
|---------|-------------|
| `status` | Daemon running? Recipe count? Unprocessed event count |
| `history [--since N]` | Event timeline (last N hours, default all) |
| `inspect <event-id>` | Full trace for one event: payload, actions, results |
| `recipes` | List loaded recipes (old format) |
| `validate` | Static policy graph validation (see above) |
| `graph [--observed]` | Event dependency graph (see above) |
| `trace <event-id>` | Policy evaluation trace for a specific event |
| `trace --policy <name> [--since N]` | Policy fire history over last N hours |

---

## Database Schema

**`events`** — the main event bus

```sql
CREATE TABLE events (
    id           INTEGER PRIMARY KEY,
    event_type   TEXT NOT NULL,
    payload      TEXT NOT NULL,         -- JSON string
    source       TEXT NOT NULL,         -- who emitted (hex-emit, scheduler, etc.)
    created_at   TEXT DEFAULT (datetime('now')),
    processed_at TEXT,                  -- NULL until daemon processes it
    recipe       TEXT,                  -- comma-joined matched policy names
    dedup_key    TEXT                   -- non-null → skip if already processed
);
```

**`action_log`** — records every action execution

```sql
CREATE TABLE action_log (
    id             INTEGER PRIMARY KEY,
    event_id       INTEGER REFERENCES events(id),
    recipe         TEXT NOT NULL,       -- policy name that fired
    action_type    TEXT NOT NULL,       -- shell, emit, notify, update-file
    action_detail  TEXT,                -- JSON of action params
    status         TEXT NOT NULL,       -- success, error, rate_limited
    error_message  TEXT,
    action_result  TEXT,                -- JSON; on success: {"retry_count": <int>, ...}
    executed_at    TEXT DEFAULT (datetime('now'))
);
```

**`deferred_events`** — pending delayed emits

```sql
CREATE TABLE deferred_events (
    id           INTEGER PRIMARY KEY,
    event_type   TEXT NOT NULL,
    payload      TEXT NOT NULL,
    source       TEXT NOT NULL,
    fire_at      TEXT NOT NULL,         -- ISO8601 UTC; fire when <= now
    cancel_group TEXT,                  -- NULL or debounce group name
    created_at   TEXT DEFAULT (datetime('now'))
);
```

**`policy_eval_log`** — per-event policy evaluation trace

```sql
CREATE TABLE policy_eval_log (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id          INTEGER REFERENCES events(id),
    policy_name       TEXT NOT NULL,
    rule_name         TEXT NOT NULL,
    matched           BOOLEAN NOT NULL,     -- trigger matched?
    conditions_passed BOOLEAN,              -- NULL if not evaluated
    condition_details TEXT,                 -- JSON array of per-condition results
    rate_limited      BOOLEAN DEFAULT 0,
    action_taken      BOOLEAN DEFAULT 0,
    evaluated_at      TEXT NOT NULL
);
```

---

## Writing a New Policy

**Step-by-step:**

1. **Identify the trigger event.** What event causes this rule to fire?
   Check `hex-events graph` for what's already being emitted.

2. **Create the policy file:**
   ```bash
   vi ~/.hex-events/policies/my-policy.yaml
   ```

3. **Use the template:**
   ```yaml
   name: my-policy
   description: "One sentence explaining what this enforces"
   standing_orders: []         # add SO numbers if this enforces a standing order
   reflection_ids: []          # add R-NNN if from an incident

   provides:
     events:
       - my-policy.result      # events this policy emits

   requires:
     events:
       - trigger.event.name    # events this policy consumes

   rules:
     - name: main-rule
       trigger:
         event: trigger.event.name
       conditions: []          # add conditions if needed
       actions:
         - type: shell
           command: "echo 'trigger fired: {{ event.field }}'"
   ```

4. **Validate the graph:**
   ```bash
   python ~/.hex-events/hex_events_cli.py validate
   ```
   Fix any `unsatisfied` errors before deploying.

5. **Test with a dry-run emit:**
   ```bash
   python ~/.hex-events/hex_emit.py trigger.event.name '{"field":"test-value"}'
   python ~/.hex-events/hex_events_cli.py history --since 1
   python ~/.hex-events/hex_events_cli.py inspect <event-id>
   ```

6. **Trace policy execution:**
   ```bash
   python ~/.hex-events/hex_events_cli.py trace <event-id>
   python ~/.hex-events/hex_events_cli.py trace --policy my-policy --since 1
   ```

The daemon hot-reloads policies every 10 seconds — no restart needed.

---

## How to Disable a Policy

To temporarily disable without deleting: rename the file extension.

```bash
mv ~/.hex-events/policies/my-policy.yaml ~/.hex-events/policies/my-policy.yaml.disabled
```

The daemon reloads within 10 seconds. No events are lost — they are still
written to the bus, just not matched.

To re-enable: rename back to `.yaml`.
