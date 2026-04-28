# hex-events v2: Composable Policy Engine — Design Document

## Overview

hex-events is a SQLite-backed event bus daemon. It polls for unprocessed events,
matches them against policies (Event-Condition-Action rules), and fires actions.

v2 evolves from flat recipes to a composable policy engine that:
- Groups related rules under named policies with standing-order metadata
- Emits timer events onto the bus (same trigger model as all other events)
- Supports delayed/debounced emit via `deferred_events`
- Validates the event dependency graph statically and from observed data
- Enforces standing orders that text-based rules cannot

## Architecture

```
  ┌──────────────────────────────────────────────────────┐
  │  hex-eventd (main loop, 2s poll)                     │
  │                                                      │
  │  ┌─────────────────┐   ┌──────────────────────────┐  │
  │  │ Scheduler       │   │  Deferred Events         │  │
  │  │ Adapter         │   │  heapq (fire_at)         │  │
  │  │ (cron → timer   │   │  backed by deferred_     │  │
  │  │  events)        │   │  events SQLite table     │  │
  │  └────────┬────────┘   └──────────┬───────────────┘  │
  │           │  emit                 │  drain due items  │
  │           ▼                       ▼                   │
  │  ┌─────────────────────────────────────────────────┐  │
  │  │         events table (SQLite WAL)               │  │
  │  │  id, event_type, payload, source, dedup_key,   │  │
  │  │  created_at, processed_at, recipe              │  │
  │  └──────────────────────┬──────────────────────────┘  │
  │                         │  get_unprocessed            │
  │                         ▼                             │
  │  ┌──────────────────────────────────────────────────┐ │
  │  │  Policy Engine                                   │ │
  │  │  load_policies() → Policy[] (hot-reload 10s)    │ │
  │  │  match rules by trigger_event (glob)            │ │
  │  │  evaluate_conditions() (scalar only)            │ │
  │  │  rate_limit check (per policy)                  │ │
  │  │  execute actions (retry 3x exp backoff)         │ │
  │  └──────────────────────────────────────────────────┘ │
  └──────────────────────────────────────────────────────┘
             │ action_log writes
             ▼
  ┌─────────────────┐
  │  action_log     │
  │  SQLite table   │
  └─────────────────┘
```

## Policy YAML Schema

```yaml
# ~/.hex-events/policies/example.yaml
name: example-policy                    # required, unique
description: "Human-readable purpose"  # required
standing_orders: [9]                    # optional: standing order ref IDs
reflection_ids: [R-033]                 # optional: reflection/incident refs

provides:
  events: [policy.violation, landings.check-due]  # events this policy emits

requires:
  events: [git.commit, landings.updated]           # events this policy consumes

rate_limit:                             # optional: prevent fork-bombs
  max_fires: 10
  window: 1m                            # duration string: s/m/h/d

rules:
  - name: arm-check                     # required
    trigger:
      event: git.commit                 # glob patterns supported (e.g. git.*)
    conditions:                         # optional; AND logic
      - field: branch
        op: eq
        value: main
      - field: "count(git.commit, 5m)"  # count(event_type, duration)
        op: lte
        value: 10
    actions:
      - type: emit
        event: landings.check-due
        delay: 10m                      # duration string → deferred_events
        cancel_group: landings-check    # debounce: replaces pending same group
      - type: shell
        command: "echo fired"
        retries: 3                      # optional, default 3
      - type: notify
        message: "Policy fired"
      - type: update-file
        path: /path/to/file
        content: "updated"
```

**Old recipe format** (auto-wrapped for backwards compatibility):
```yaml
name: my-recipe          # → single-rule policy, name = policy name
trigger:
  event: git.push
actions:
  - type: shell
    command: "..."
# provides/requires inferred from trigger + emit actions
```

## Adapter Manifest Schema

```yaml
# ~/.hex-events/adapters/scheduler.yaml
adapter: scheduler
schedules:
  - name: tick-30m
    cron: "*/30 * * * *"        # standard cron expression
    event: timer.tick.30m       # emitted event type
    # dedup_key auto-set: "timer.tick.30m:2026-03-19T14:00"
  - name: tick-daily
    cron: "0 9 * * *"
    event: timer.tick.daily
```

## Event Lifecycle

```
1. INSERT  → events table (dedup_key checked; skip if key exists + processed)
2. POLL    → daemon reads WHERE processed_at IS NULL ORDER BY id
3. MATCH   → policy rules with matching trigger_event (glob)
4. FILTER  → evaluate_conditions() against payload + db.count_events()
5. RATE    → per-policy rate_limit check (last_fires window)
6. EXECUTE → actions in order; retry on failure (3x exp backoff: 1s,2s,4s)
7. LOG     → action_log row per action (including retry attempts)
8. MARK    → events.processed_at = now, events.recipe = comma-joined policy names
```

**Dedup rule:** If `dedup_key` is non-null AND a row with the same key and
`processed_at IS NOT NULL` exists → skip insert silently.

**Recipe column:** Stores comma-separated list of ALL matched policy names,
not just the last one.

## Timer Model

### Wall-clock timers (Scheduler Adapter)
- Evaluates cron expressions each daemon tick
- Emits `timer.tick.<name>` event with `dedup_key = "event_type:ISO8601_slot"`
- Dedup prevents double-emission if daemon polls faster than cron period
- **Restart catch-up:** on startup, check last emitted event per schedule;
  emit one catch-up tick if missed. `emit_latest_only` is hardcoded — emits
  one tick regardless of how many were missed, preventing restart storms.

### Relative timers (Delayed Emit)
- `delay: 10m` on any `emit` action → insert into `deferred_events` table
- `cancel_group: name` → DELETE existing rows with same cancel_group first
  (last-write-wins debounce)
- Daemon holds `deferred_events` in memory heapq; reloads from SQLite on startup
- **Drain:** delete from `deferred_events` first, then insert into `events`
  (dual-write safety: lost event is better than doubled event)

### Deadline pattern
Use `cancel_group` to implement deadlines:
```yaml
# Rule 1: on git.commit, arm a check 10m later (debounced)
- type: emit
  event: check.due
  delay: 10m
  cancel_group: check-gate
# Rule 2: on landings.updated, cancel the pending check
- type: emit
  event: check.cancelled
  cancel_group: check-gate  # replaces the pending check.due
```

## Deferred Events Table

```sql
CREATE TABLE deferred_events (
    id          INTEGER PRIMARY KEY,
    event_type  TEXT NOT NULL,
    payload     TEXT NOT NULL DEFAULT '{}',
    source      TEXT NOT NULL DEFAULT 'deferred',
    fire_at     TEXT NOT NULL,           -- ISO8601 UTC
    cancel_group TEXT,                   -- nullable
    created_at  TEXT DEFAULT (datetime('now'))
);
CREATE INDEX idx_deferred_fire_at ON deferred_events(fire_at);
CREATE INDEX idx_deferred_cancel_group ON deferred_events(cancel_group)
    WHERE cancel_group IS NOT NULL;
```

## Duration Strings

All time values use duration strings: `10s`, `5m`, `2h`, `7d`.

`parse_duration(s) -> float (seconds)`:
- `s` suffix = seconds
- `m` suffix = minutes (×60)
- `h` suffix = hours (×3600)
- `d` suffix = days (×86400)

`count(event_type, 10m)` in conditions passes the duration string to
`db.count_events()` which converts to seconds internally.

## Validation Model

`hex-events validate`:
1. Load all policies from `policies/` dir
2. Load adapter configs from `adapters/*.yaml`
3. Extract event graph: `provides.events` → node, `requires.events` → node
4. Check: every required event has at least one provider (policy or adapter)
5. Warn: provided events with no consumer (orphan)
6. Check: no cycles in the event dependency graph (DFS)
7. Output: summary table + PASS/FAIL

`hex-events graph --observed`:
- Query events table: distinct event_types in last 7 days
- Query action_log: which policies fired per event_type
- Display observed graph, compare with static graph
- Highlight: events seen but not declared, declared but never seen

## Migration: Recipes → Policies

**Backwards compatible:** The `load_policies()` loader detects old recipe format
(has `trigger` at top-level, no `rules` array) and auto-wraps it:
- Policy name = recipe name
- Single rule with trigger + conditions + actions from recipe
- `provides.events` inferred from `emit` action `event` fields
- `requires.events` inferred from `trigger.event` field

**To convert manually:**
```yaml
# Before (recipe)
name: commit-changelog
trigger:
  event: git.push
actions:
  - type: shell
    command: "..."

# After (policy)
name: commit-changelog
description: "Append commit to today's landing log"
provides:
  events: []
requires:
  events: [git.push]
rules:
  - name: append-to-landing
    trigger:
      event: git.push
    actions:
      - type: shell
        command: "..."
```

Run `hex-events validate` after conversion to verify event graph is consistent.

## Non-goals

- **No external message brokers.** SQLite WAL is sufficient for local daemon use.
- **No distributed operation.** Single-host only; no clustering or replication.
- **No complex CEP.** No temporal joins, stream windows, or multi-event correlation
  beyond `count()`. Keep conditions scalar.
- **No Python packaging.** stdlib + PyYAML only. No pip-installable distribution.
- **No GUI.** CLI (`hex-events`) is the only interface.
- **No hot-reload of adapters.** Adapter config changes require daemon restart.
- **No per-rule rate limits.** Rate limiting is per-policy only.

## Testing

### Unit and Integration Tests

Run the full test suite (requires venv set up via `install.sh`):

```bash
make test
# or directly:
./venv/bin/python3 -m pytest tests/ -v --tb=short
```

### Docker Smoke Test

The smoke test verifies end-to-end behavior in a clean container with no pre-existing state:

```bash
make smoke
# or directly:
docker build -t hex-events-test -f tests/Dockerfile.install .
docker run --rm hex-events-test
```

### What the Smoke Test Verifies

1. **Install script** (`install.sh`) runs successfully in a clean environment
2. **Dependencies** are importable (`yaml`, `croniter`, `jinja2`)
3. **Database** is initialized (`events.db` exists with correct schema)
4. **Daemon** starts in the background without errors
5. **Event emission** works (`hex_emit.py` successfully writes to the DB)
6. **Event processing** completes (emitted event gets `processed_at` set)
7. **Validator** passes (`hex_events_cli.py validate` exits 0)
8. **Full test suite** passes inside the container
9. **Daemon shutdown** completes cleanly

### Running All Checks

```bash
make all  # install + test + smoke
```
