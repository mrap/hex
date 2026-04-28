# hex-events

Policy-based event engine for the hex AI agent system. A persistent daemon (`hex_eventd.py`) polls a SQLite event bus, evaluates YAML policies against incoming events, and fires actions (shell commands, event chaining, notifications).

## Architecture

```
Events in                    Daemon loop (2s poll)              Actions out
─────────────────           ──────────────────────            ────────────────
hex_emit.py ──────┐         ┌─ drain deferred events          shell commands
scheduler adapter ─┼──→ DB ──┼─ get unprocessed events ──→    emit (chain)
fswatch/git hooks ─┘    │   ├─ match policies by event type   notify
                        │   ├─ evaluate conditions (AND)       update-file
                        │   ├─ check rate limits               dagu
                        │   └─ fire actions with retry
                        │
                   events.db (SQLite WAL mode)
```

**Key files:**

| File | Purpose |
|------|---------|
| `hex_eventd.py` | Main daemon. Singleton via flock. Polls DB, matches policies, fires actions. |
| `hex_emit.py` | CLI to emit events. `./hex_emit.py <event_type> '<json_payload>' <source>` |
| `hex_events_cli.py` | Debug CLI: `status`, `history`, `inspect`, `trace`, `validate`, `graph` |
| `policy.py` | Policy/Rule/Condition/Action dataclasses. Loads YAML, supports old recipe + new policy format. |
| `conditions.py` | Condition evaluator. Field comparisons, shell conditions, `count()` aggregation. |
| `db.py` | SQLite wrapper. Events, action log, deferred events, policy eval log. |
| `actions/` | Action plugin registry. Each action type is a class with `run()`. |
| `adapters/scheduler.py` | Cron-based timer events. Config in `adapters/scheduler.yaml`. |
| `policies/` | All policy YAML files. Subdirectories are workflows with shared `_config.yaml`. |
| `scripts/` | Shell scripts called by policy actions. |
| `policy_validator.py` | Schema validation for policy YAML. |

## How the Daemon Works

1. Acquires singleton lock (`hex_eventd.lock` via flock)
2. Kills any competing `hex_eventd.py` processes
3. Cleans stale WAL/SHM files, verifies DB is writable
4. Enters main loop (every 2 seconds):
   - Hot-reloads policies from `policies/` every 10 seconds
   - Ticks the scheduler adapter (emits cron-based timer events with dedup)
   - Drains deferred events whose `fire_at` has passed
   - Fetches unprocessed events, matches against policies, evaluates conditions, fires actions
   - Tracks health state, writes `health.json` every 60 seconds
   - Runs janitor hourly (deletes events older than 7 days)
5. Handles DB lock errors with automatic recovery (checkpoint WAL, reconnect)

## Policy Format

Policies are YAML files in `policies/`. Two formats are supported:

### New format (preferred)

```yaml
name: my-policy
description: What this policy does
rate_limit:
  max_fires: 3
  window: 1h
lifecycle: persistent          # persistent | oneshot-delete | oneshot-disable
max_fires: 10                  # auto-disable after N fires (optional)

provides:
  events: [output.event]       # events this policy emits (documentation)
requires:
  events: [input.event]        # events this policy consumes (documentation)

rules:
  - name: my-rule
    trigger:
      event: some.event.type   # supports glob patterns (e.g., boi.spec.*)
    conditions:                # all must pass (AND logic)
      - field: status
        op: eq
        value: failed
      - field: payload.nested.field
        op: gt
        value: 5
    actions:
      - type: shell
        command: bash ~/script.sh "{{ event.spec_id }}"
        timeout: 60
        retries: 3
        on_success:
          - type: emit
            event: next.event
            payload:
              result: "{{ action.stdout }}"
        on_failure:
          - type: notify
            message: "Failed: {{ action.stderr }}"
```

### Condition operators

`eq`, `neq`, `gt`, `gte`, `lt`, `lte`, `contains`, `glob`, `regex`

Special condition types:
- **Shell condition:** `type: shell`, `command: "test -f /path"` (exit 0 = pass)
- **Count aggregation:** `field: "count(event.type, 1h)"` counts events in a time window. Supports payload filter: `count(event.type, 1h, field=value)`

### Field resolution

- `field: status` looks up `payload["status"]`
- `field: payload.nested.key` traverses `payload["nested"]["key"]`

### Workflows (grouped policies)

Subdirectories in `policies/` are workflows. They share config via `_config.yaml`:

```
policies/
  boi-lifecycle/
    _config.yaml        # name, enabled, shared config
    completion-gate.yaml
    output-persistence.yaml
```

`_config.yaml`:
```yaml
name: boi-lifecycle
enabled: true
config:
  scripts_dir: ~/.hex-events/scripts
```

Workflow config is available in Jinja2 templates as `{{ workflow.config.scripts_dir }}`.

Disable a workflow by adding a `.disabled` file or setting `enabled: false` in `_config.yaml`.

## Action Types

| Type | Params | Description |
|------|--------|-------------|
| `shell` | `command`, `timeout` (default 60), `retries` (default 3) | Run shell command. Jinja2 templating with `{{ event.* }}`. |
| `emit` | `event`, `payload`, `delay`, `cancel_group`, `source` | Emit a new event (chaining). `delay: 5m` defers it. `cancel_group` replaces pending deferred events with same group. |
| `notify` | `message` | Send notification via `hex-notify.sh`. |
| `update-file` | (see source) | Update a file on disk. |
| `dagu` | (see source) | Trigger a Dagu workflow. |

All actions support `on_success` and `on_failure` sub-actions. Sub-actions have access to `{{ action.stdout }}`, `{{ action.stderr }}`, `{{ action.returncode }}`.

Actions retry with exponential backoff (1s, 2s, 4s...) up to `retries` count.

## Emitting Events

### From shell
```bash
# Direct
./hex_emit.py boi.spec.completed '{"spec_id": "abc"}' hex:boi

# Or via the installed symlink
python3 ~/.hex-events/hex_emit.py event.type '{}' source
```

### From a policy action
```yaml
actions:
  - type: emit
    event: next.event.type
    payload:
      key: "{{ event.some_field }}"
    delay: 5m              # optional: defer by duration
    cancel_group: my-group # optional: replaces pending deferred with same group
```

### Timer events (cron)
Configured in `adapters/scheduler.yaml`. The scheduler adapter emits deduped timer events:
```yaml
schedules:
  - name: 30m-tick
    cron: "*/30 * * * *"
    event: timer.tick.30m
```

## Running Tests

```bash
# All tests
make test
# Or directly
./venv/bin/python3 -m pytest tests/ -v --tb=short

# Smoke test (Docker)
make smoke

# Specific test file
./venv/bin/python3 -m pytest tests/test_conditions.py -v
```

## Static Verification (v0.2.0)

The compiler statically verifies bundle policies before they land in `~/.hex-events/policies/`. Invoked by `hex-integration install`; also available standalone.

```bash
# Event catalog: {event → {producers[], consumers[]}}
./hex_events_cli.py list-events [--format json]

# Validate without writing (exit 0 = clean, 1 = errors, 2 = warnings only)
./hex_events_cli.py check <bundle-dir-or-policy>
./hex_events_cli.py check <path> --format json
./hex_events_cli.py check <path> --permissive  # warn-only for legacy corpus
./hex_events_cli.py check --all               # full scan of ~/.hex-events/policies/

# Compile: check + write manifest headers atomically
./hex_events_cli.py compile <bundle-dir>
./hex_events_cli.py compile <bundle-dir> --dry-run
```

Validators live in `validators/`: `schema.py`, `producer_check.py`, `deadcode.py`.

Compiled policies carry `# compiler_version:` and `# checks_passed:` manifest headers.

## Adding a New Policy

1. Create a YAML file in `policies/` (or inside a workflow subdirectory)
2. Use the format above. At minimum: `name`, `rules` with `trigger` and `actions`
3. The daemon hot-reloads policies every 10 seconds -- no restart needed
4. Validate: `./hex_events_cli.py validate`
5. Debug: `./hex_events_cli.py trace --policy my-policy --since 1`

To disable a policy: set `enabled: false` in the YAML file.

## Adding a New Action Type

1. Create a file in `actions/` (e.g., `actions/my_action.py`)
2. Implement a class with `run(self, params, event_payload, db=None, workflow_context=None) -> dict`
3. Decorate with `@register("my-action-type")`
4. Import it in `actions/__init__.py`
5. Add the type to `VALID_ACTION_TYPES` in `policy_validator.py`

## Installation

```bash
bash install.sh
```

This symlinks `~/.hex-events` to the repo directory, creates a virtualenv, and installs dependencies. The daemon runs as a LaunchAgent (macOS) or can be started manually:

```bash
./venv/bin/python3 hex_eventd.py
```

## Database

SQLite with WAL mode at `events.db`. Tables:

- `events` -- event bus (event_type, payload JSON, source, processed_at, dedup_key)
- `action_log` -- what actions fired for each event
- `deferred_events` -- delayed events waiting to fire
- `policy_eval_log` -- full evaluation trace (which policies matched, conditions passed/failed, rate limited)

Janitor auto-deletes events older than 7 days.

## Dependencies

- Python 3.10+
- PyYAML, croniter, Jinja2, pytest (see `requirements.txt`)
