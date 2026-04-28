# hex-events Policy Skill Reference

Machine-readable source of truth for the `/hex-event` skill. Update this file when hex-events adds new features.

---

## Policy YAML — Top-Level Fields

| Field | Type | Required | Default | Valid Values |
|-------|------|----------|---------|--------------|
| `name` | string | yes | — | unique identifier |
| `description` | string | no | `""` | free text |
| `lifecycle` | string | no | `persistent` | `persistent`, `oneshot-delete`, `oneshot-disable` |
| `max_fires` | int | no | null | positive integer |
| `enabled` | bool | no | `true` | `true`, `false` |
| `rules` | list | yes | — | non-empty list of Rule objects |
| `provides.events` | list[string] | no | `[]` | event names this policy emits |
| `requires.events` | list[string] | no | `[]` | event names this policy depends on (informational) |
| `rate_limit` | dict | no | null | `{max_fires: int, window: "<duration>"}` |
| `standing_orders` | list[string] | no | `[]` | standing order references |
| `reflection_ids` | list[string] | no | `[]` | reflection references |

---

## Lifecycle Modes

| Value | Behavior |
|-------|----------|
| `persistent` | Policy remains active indefinitely; fires every time the trigger event matches |
| `oneshot-delete` | Fires once, then the policy file is deleted from disk |
| `oneshot-disable` | Fires once, then `enabled: false` is written to the file; file is kept |

---

## TTL Format

Format: `\d+[smhd]` — a positive integer followed by a unit.

| Unit | Meaning |
|------|---------|
| `s` | seconds |
| `m` | minutes |
| `h` | hours |
| `d` | days |

Examples: `60s`, `30m`, `24h`, `7d`

TTL is measured from the **policy file's mtime**. If the file is older than the TTL, the rule is skipped. Set on individual rules (not the policy level).

---

## Rule Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Unique rule identifier within the policy |
| `trigger.event` | string | yes | Event name to match; supports glob patterns (`boi.*`, `task.*`) |
| `ttl` | string | no | Skip rule if policy file is older than this duration |
| `conditions` | list | no | AND-logic list of Condition objects |
| `actions` | list | yes | Non-empty list of Action objects |

---

## Condition Types

### Field Condition

```yaml
- field: "payload.status"   # dot-notation: payload.<key>.<subkey>
  op: eq
  value: "done"
```

| Field | Description |
|-------|-------------|
| `field` | Dot-notation path into event payload. Prefix `payload.` traverses the payload dict. |
| `op` | Comparison operator (see table below) |
| `value` | Expected value (string, int, float, or bool) |

#### Condition Operators

| Op | Meaning | Example |
|----|---------|---------|
| `eq` | Equal | `op: eq, value: "done"` |
| `neq` | Not equal | `op: neq, value: "failed"` |
| `gt` | Greater than | `op: gt, value: 5` |
| `gte` | Greater than or equal | `op: gte, value: 3` |
| `lt` | Less than | `op: lt, value: 100` |
| `lte` | Less than or equal | `op: lte, value: 10` |
| `contains` | String contains | `op: contains, value: "error"` |
| `glob` | Fnmatch glob | `op: glob, value: "build.*"` |
| `regex` | Regex search | `op: regex, value: "^err.*"` |

### Count Condition

```yaml
- field: "count(event.name, 5m)"
  op: gte
  value: 3
```

Counts matching events in a time window. Format: `count(<event_type>, <duration>)`. Optional payload filter: `count(event.name, 5m, status=done)`.

### Shell Condition

```yaml
- type: shell
  command: "test -f /tmp/flag"
```

Passes if the shell command exits with code 0. Timeout: 30 seconds. Supports Jinja2 templates.

---

## Action Types

### `shell`

Execute a shell command.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `command` | string | yes | — | Shell command; supports Jinja2 templates |
| `timeout` | int | no | 60 | Timeout in seconds |
| `retries` | int | no | 3 | Number of retries on failure |
| `on_success` | list[Action] | no | — | Actions to run after success |
| `on_failure` | list[Action] | no | — | Actions to run after failure |

```yaml
- type: shell
  command: "bash $AGENT_DIR/scripts/run.sh"
  timeout: 120
  retries: 0
  on_success:
    - type: emit
      event: "task.done"
  on_failure:
    - type: notify
      message: "Failed: {{ action.stdout }}"
```

### `emit`

Emit a new hex-events event.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `event` | string | yes | — | Event name to emit |
| `payload` | dict | no | `{}` | Payload dict; values support Jinja2 |
| `delay` | string | no | — | Duration to defer emission (e.g. `5s`) |
| `cancel_group` | string | no | — | Cancel pending emits with same group name |
| `source` | string | no | — | Sets event source field |

```yaml
- type: emit
  event: "compile.done"
  payload:
    status: "ok"
    branch: "{{ event.branch }}"
  delay: "2s"
```

### `notify`

Send a notification via `~/.claude/scripts/hex-notify.sh`.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `message` | string | yes | Notification text; supports Jinja2 templates |

```yaml
- type: notify
  message: "Build done: {{ event.payload.result }}"
```

### `update-file`

Atomically update a file using regex replace.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `target` | string | yes | Absolute path to the file |
| `pattern` | string | yes | Regex pattern to match |
| `replace` | string | yes | Replacement string |

```yaml
- type: update-file
  target: "/path/to/file.yaml"
  pattern: 'status: \w+'
  replace: "status: done"
```

### `dagu` *(non-validated)*

Trigger a Dagu workflow. Not currently in the validator's VALID_ACTION_TYPES; use with caution.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `workflow` | string | yes | Workflow YAML filename |

---

## Jinja2 Template Context

Available in `command`, `message`, payload values, and shell condition `command`.

| Variable | Description |
|----------|-------------|
| `{{ event.<field> }}` | Any top-level field in the triggering event |
| `{{ event.payload.<key> }}` | Nested payload field |
| `{{ workflow.name }}` | Current workflow name (if policy is inside a workflow dir) |
| `{{ workflow.config.<key> }}` | Workflow config value from `_config.yaml` |
| `{{ action.stdout }}` | stdout from previous shell action (in `on_success`/`on_failure`) |
| `{{ now }}` | `datetime.utcnow()` (available in shell conditions) |

---

## Rate Limiting

```yaml
rate_limit:
  max_fires: 1      # max fires within window
  window: "10m"     # duration string
```

Rate limit applies to the whole policy. If the policy has fired `max_fires` times within `window`, it is skipped.

---

## Workflow Directories

Policies can be organized into workflow subdirectories under `~/.hex-events/policies/`:

```
policies/
  my-workflow/
    _config.yaml      # workflow config: name, enabled, config: {}
    policy-a.yaml
    policy-b.yaml
```

`_config.yaml` fields: `name` (string), `enabled` (bool), `config` (dict of shared values). Create `.disabled` file in the directory to disable the entire workflow.

---

## Validation

```bash
python3 ~/.hex-events/hex_events_cli.py validate <policy.yaml>
```

Exit 0 = valid. Errors printed to stdout.

**Validated fields:** `name`, `lifecycle`, `max_fires`, rule `name`, rule `ttl`, `trigger.event`, `actions` (non-empty, valid type, required fields per type), `condition.op`.

**Valid action types (validator):** `shell`, `emit`, `notify`, `update-file`

**Valid lifecycle values:** `persistent`, `oneshot-delete`, `oneshot-disable`

**Valid condition ops:** `eq`, `neq`, `contains`, `gt`, `lt`, `gte`, `lte`, `glob`, `regex`
