# hex-events: Migration Guide v1 → v2

This guide explains how to convert existing v1 recipes to v2 policies, covers
the backwards-compatible loader behaviour, and shows the 3 live recipes
converted as concrete examples.

---

## What Changed

| Concept | v1 | v2 |
|---------|----|----|
| Unit | Recipe (single ECA rule) | Policy (named group of rules + metadata) |
| Format | `trigger` + `actions` | `rules` list with `name`, `trigger`, `actions` |
| Metadata | None | `description`, `standing_orders`, `reflection_ids` |
| Event graph | Implicit | Explicit `provides`/`requires` declarations |
| Rate limiting | None | Per-policy `rate_limit: {max_fires, window}` |
| Timers | Not supported | `timer.tick.*` events via scheduler adapter |
| Delayed emit | Not supported | `delay: 10m` + `cancel_group` on emit action |
| Dedup | Not supported | `dedup_key` on events prevents double-processing |

---

## Backwards Compatibility

**Old recipe files continue to work unchanged.** The v2 policy loader
auto-wraps any YAML file that has `trigger` + `actions` + `name` at the top
level into a single-rule Policy object with inferred `provides`/`requires`.

This means you can migrate incrementally — no flag day required.

```
recipes/my-recipe.yaml      ← still loaded by old daemon via load_recipes()
policies/my-policy.yaml     ← loaded by new policy loader
```

The daemon currently loads from `recipes/`; to switch to policy-aware loading
update `RECIPES_DIR` in `hex_eventd.py` to point at `policies/` and use
`load_policies()` instead of `load_recipes()`.

---

## How to Convert a Recipe to a Policy

### Step 1: Move the file

```bash
cp ~/.hex-events/recipes/my-recipe.yaml ~/.hex-events/policies/my-policy.yaml
```

### Step 2: Wrap in the policy format

**Before (v1 recipe):**
```yaml
name: my-recipe
trigger:
  event: some.event
conditions:
  - field: path
    op: contains
    value: important/
actions:
  - type: emit
    event: some.processed
```

**After (v2 policy):**
```yaml
name: my-recipe
description: Human-readable description of what this policy enforces.
standing_orders: []        # e.g. ["9"] to reference SO-9
reflection_ids: []         # e.g. ["R-033"]
provides:
  events:
    - some.processed       # events this policy emits
requires:
  events:
    - some.event           # events this policy triggers on
rules:
  - name: my-recipe.main
    trigger:
      event: some.event
    conditions:
      - field: path
        op: contains
        value: important/
    actions:
      - type: emit
        event: some.processed
```

### Step 3: Validate

```bash
cd ~/.hex-events && python3 hex_events_cli.py validate
```

Expected output when all requires are satisfied:

```
[OK] All requires satisfied.
[OK] No orphan provides.
[OK] No cycles detected.
Validation: VALID
```

**Note on external events:** Events like `git.commit` or `file.created` that
come from external adapters (git hooks, fswatch) will appear as "unsatisfied"
in the static validator unless they are listed in an adapter config. This is
expected — add an adapter manifest or mark them as externally provided:

```yaml
# adapters/scheduler.yaml
schedules:
  - name: external-git-commit
    # No cron needed — just declare it exists for static validation
    event: git.commit
```

---

## Live Recipe Conversions

### 1. `commit-changelog` → `commit-changelog` policy

**Original (`recipes/commit-changelog.yaml`):**
```yaml
name: commit-changelog
trigger:
  event: git.push
actions:
  - type: shell
    command: >
      cd ~ &&
      LAST_COMMIT=$(git log -1 --format='%h %s') &&
      LANDINGS=$(ls landings/ | sort | tail -1) &&
      echo "- $(date +%H:%M) — Git push: $LAST_COMMIT" >> "landings/$LANDINGS"
```

**Converted (`policies/commit-changelog.yaml`):**
```yaml
name: commit-changelog
description: Appends a changelog entry to the current landings doc on git push.
standing_orders: []
reflection_ids: []
provides:
  events: []           # shell action has no event output
requires:
  events:
    - git.push
rules:
  - name: append-changelog
    trigger:
      event: git.push
    actions:
      - type: shell
        command: >
          cd ~ &&
          LAST_COMMIT=$(git log -1 --format='%h %s') &&
          LANDINGS=$(ls landings/ | sort | tail -1) &&
          echo "- $(date +%H:%M) — Git push: $LAST_COMMIT" >> "landings/$LANDINGS"
```

---

### 2. `file-triage` → `file-triage` policy

**Original (`recipes/file-triage.yaml`):**
```yaml
name: file-triage
trigger:
  event: file.created
conditions:
  - field: path
    op: contains
    value: raw/captures/
actions:
  - type: shell
    command: 'echo "New capture: {{ event.path }}" >> ~/.hex-events/daemon.log'
```

**Converted (`policies/file-triage.yaml`):**
```yaml
name: file-triage
description: Logs new raw capture files to daemon.log.
standing_orders: []
reflection_ids: []
provides:
  events: []
requires:
  events:
    - file.created
rules:
  - name: log-new-capture
    trigger:
      event: file.created
    conditions:
      - field: path
        op: contains
        value: raw/captures/
    actions:
      - type: shell
        command: 'echo "New capture: {{ event.path }}" >> ~/.hex-events/daemon.log'
```

---

### 3. `landing-refresh` → `landing-refresh` policy

**Original (`recipes/landing-refresh.yaml`):**
```yaml
name: landing-refresh
trigger:
  event: file.modified
conditions:
  - field: path
    op: contains
    value: landings/
actions:
  - type: shell
    command: bash ~/.claude/scripts/landings-dashboard.sh --refresh 2>/dev/null || true
```

**Converted (`policies/landing-refresh.yaml`):**
```yaml
name: landing-refresh
description: Refreshes the landings dashboard when any landings file is modified.
standing_orders: []
reflection_ids: []
provides:
  events: []
requires:
  events:
    - file.modified
rules:
  - name: refresh-dashboard
    trigger:
      event: file.modified
    conditions:
      - field: path
        op: contains
        value: landings/
    actions:
      - type: shell
        command: bash ~/.claude/scripts/landings-dashboard.sh --refresh 2>/dev/null || true
```

---

## Rollback Plan

If you need to revert to v1 recipes:

1. The original recipe files in `recipes/` are unchanged — no rollback needed
   for those files.
2. If you deleted a recipe, restore from git: `git checkout HEAD recipes/`
3. The daemon can be pointed back to `RECIPES_DIR` by reverting any changes to
   `hex_eventd.py`.
4. New policy features (deferred emit, scheduler, dedup) are additive and do
   not affect v1 recipe processing.

---

## Verifying the Migration

```bash
# 1. Run all tests
cd ~/.hex-events && python3 -m pytest tests/ -v

# 2. Validate the static policy graph
cd ~/.hex-events && python3 hex_events_cli.py validate

# 3. Check the event graph
cd ~/.hex-events && python3 hex_events_cli.py graph

# 4. Observe events from last 7 days against the graph
cd ~/.hex-events && python3 hex_events_cli.py graph --observed
```
