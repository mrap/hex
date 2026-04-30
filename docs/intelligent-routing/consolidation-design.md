# Autonomous Consolidation ("Dreaming") — Design

> Part of the Intelligent Session Routing initiative.
> Describes background processes that keep hex's memory healthy without user intervention.

---

## Overview

The "dreaming" system replaces five manual workflows with automatic background processes:

| Manual today | Replaced by |
|---|---|
| `/hex-reflect` | Session distillation (trigger: `hex.session.stopped`) |
| None | Memory compression (trigger: `timer.tick.daily`) |
| `evolution/observations.md` edits | Pattern detection (trigger: `timer.tick.6h`) |
| None | Knowledge graph update (trigger: on each distillation) |
| None | Memory pruning (trigger: `timer.tick.weekly`) |

All five components slot into hex-events as policies. They share a single output contract: structured JSON written to `.hex/dream-state.json` that downstream policies can react to.

---

## Component 1: Session Distillation

**Goal:** After every session ends, extract the valuable signal (learnings, decisions, action items) and write it to canonical locations — without waiting for Mike to invoke `/hex-reflect`.

### Trigger

```
event: hex.session.stopped
payload: { session_id, agent_dir, transcript_path, duration_seconds }
```

`hex.session.stopped` is emitted by the session lifecycle manager (see routing-proposal.md) when a session enters idle-timeout or is explicitly stopped. Falls back to a deferred event fired 5 minutes after `hex.session.last_message` with no subsequent activity.

### What it reads

| Source | Purpose |
|---|---|
| `raw/transcripts/<session_id>.jsonl` | Full session transcript |
| `me/learnings.md` | Existing learnings (dedup check) |
| `me/decisions/*.md` | Existing decisions (dedup check) |
| `todo.md` | Existing action items (dedup check) |
| `evolution/behavioral-escalations.md` | Existing corrections (dedup check) |

### What it writes

| Destination | What |
|---|---|
| `me/learnings.md` | Appends new `## [YYYY-MM-DD] <topic>` sections |
| `me/decisions/<slug>.md` | Creates new files for distinct decisions |
| `todo.md` | Appends unfinished action items with date |
| `evolution/reflection-log.md` | Appends timestamped session summary (replaces current session-reflect.sh placeholder) |
| `.hex/dream-state.json` | Updates `last_distillation_at`, `pending_graph_updates` |

### LLM calls

**Call 1 — Extract signal** (1 call per session)
- Model: `claude-haiku-4-5` (cost optimization; haiku handles structured extraction well)
- Input: last 50k tokens of transcript + existing learnings headings (dedup context)
- Output: structured JSON with arrays for `learnings`, `decisions`, `action_items`
- Prompt strategy: few-shot with 3 examples from real hex sessions
- **Estimated cost:** ~$0.004 per session (50k input tokens at $0.08/MTok haiku input)

**Call 2 — Dedup filter** (1 call, only if learnings exist)
- Model: `claude-haiku-4-5`
- Input: new learnings list + existing learnings headings
- Output: filtered list with duplicates removed
- **Estimated cost:** ~$0.001

**Total per session: ~$0.005 (under 1 cent)**

### Implementation skeleton

```python
# system/scripts/dream/distill_session.py
"""
Triggered by: hex-events policy dream-distill-session.yaml
Input env:    SESSION_ID, TRANSCRIPT_PATH, HEX_DIR
"""
import json, os, sys
from pathlib import Path

def extract_signal(transcript_text: str, existing_headings: list[str]) -> dict:
    """Call claude-haiku to extract learnings/decisions/action_items."""
    # Uses Anthropic SDK with prompt caching on system prompt
    ...

def write_learnings(new_items: list[dict], learnings_path: Path):
    """Append to me/learnings.md under a datestamped heading."""
    ...

def write_decisions(decisions: list[dict], decisions_dir: Path):
    """Write me/decisions/<slug>.md for each distinct decision."""
    ...

def write_todos(action_items: list[dict], todo_path: Path):
    """Append to todo.md."""
    ...

if __name__ == "__main__":
    ...
```

### hex-events policy

```yaml
# system/events/policies/dream-distill-session.yaml
name: dream-distill-session
description: >
  After each session ends, extract learnings, decisions, and action items
  from the transcript and persist to canonical locations.

rules:
  - name: distill-on-session-stop
    trigger:
      event: hex.session.stopped
    actions:
      - type: shell
        command: |
          python3 $HEX_DIR/system/scripts/dream/distill_session.py \
            --session-id "{{ event.session_id }}" \
            --transcript "{{ event.transcript_path }}"
        timeout: 300
        on_success:
          - type: emit
            event: dream.distillation.complete
            payload:
              session_id: "{{ event.session_id }}"
        on_failure:
          - type: emit
            event: dream.distillation.failed
            payload:
              session_id: "{{ event.session_id }}"
```

### Failure modes

| Failure | Behavior |
|---|---|
| Transcript missing | Skip distillation, emit `dream.distillation.skipped` with reason |
| Haiku API timeout | Retry once after 30s, then skip and leave transcript for next daily run |
| `me/learnings.md` locked | Use coordination.py lock; if held >30s, write to `.hex/distillation-queue.jsonl` for next run |
| Duplicate detection false negative | Acceptable; human review of learnings.md can merge later; pruning pass handles it |

---

## Component 2: Memory Compression

**Goal:** Prevent unbounded growth. Old transcripts become summaries. Redundant learnings get consolidated. Stale project context gets archived.

### Trigger

```
event: timer.tick.daily
payload: { utc_hour }  # fires at each daily tick; policy rate-limits to 2am run
```

### What it reads

| Source | Purpose |
|---|---|
| `raw/transcripts/*.jsonl` (>7 days old) | Transcripts eligible for summarisation |
| `me/learnings.md` | Full learnings file |
| `projects/*/context.md` | Project context files |
| `projects/*/checkpoint.md` | Last activity timestamp |

### What it writes

| Destination | What |
|---|---|
| `raw/transcripts/summaries/<date>.md` | Compressed daily summaries (raw .jsonl kept) |
| `me/learnings.md` | Redundant sections collapsed into consolidated entries |
| `projects/_archive/<name>/` | Inactive projects (no checkpoint update in >30 days) |
| `evolution/consolidation-latest.log` | Run report (already written by consolidate.sh) |
| `.hex/dream-state.json` | Updates `last_compression_at`, `archived_projects` |

### LLM calls

**Call 1 — Summarise old transcripts** (1 call per transcript file, batched)
- Model: `claude-haiku-4-5`
- Input: full transcript text (up to 100k tokens)
- Output: 1-page markdown summary (key decisions, corrections, topics covered)
- **Estimated cost:** ~$0.008 per transcript day file

**Call 2 — Consolidate learnings** (1 call per compression run)
- Model: `claude-haiku-4-5`
- Input: full me/learnings.md
- Output: deduplicated, consolidated version
- **Estimated cost:** ~$0.003 per run

**Total per daily run: ~$0.05–0.15** (depending on number of transcripts to compress)

**Rate limit:** Only transcripts older than 7 days are eligible. Summaries already present are skipped. This keeps the daily cost bounded.

### hex-events policy

```yaml
# system/events/policies/dream-memory-compression.yaml
name: dream-memory-compression
description: >
  Nightly compression run: summarise old transcripts, consolidate
  redundant learnings, archive inactive projects.

rate_limit:
  max_fires: 1
  window: 20h

rules:
  - name: compress-at-2am
    trigger:
      event: timer.tick.daily
    conditions:
      - utc_hour: { between: [1, 3] }
    actions:
      - type: shell
        command: |
          python3 $HEX_DIR/system/scripts/dream/compress_memory.py
        timeout: 1800
        on_success:
          - type: emit
            event: dream.compression.complete
```

### Failure modes

| Failure | Behavior |
|---|---|
| LLM cost spike (many transcripts) | Hard limit: max 10 transcripts per run, queue remainder for next day |
| me/learnings.md write conflict | Coordination lock; skip if held, retry next daily run |
| Project archive false positive | Archive only moves to `_archive/`; recovery is `mv _archive/X projects/X` |
| Compression run takes >30min | Timeout kills it; partial work committed; next run continues from last summary |

---

## Component 3: Pattern Detection

**Goal:** Across multiple sessions, detect recurring corrections, tool patterns, and context-loading habits that should become standing orders or automations.

### Trigger

```
event: timer.tick.6h
payload: { tick_count }
```

*Note: `timer.tick.6h` must be added to the hex-events scheduler (currently has `timer.tick.daily.6am` and `timer.tick.hourly`). Add a 6-hour interval in `adapters/scheduler.py`.)*

### What it reads

| Source | Purpose |
|---|---|
| `evolution/behavioral-escalations.md` | Existing escalations (dedup) |
| `.hex/dream-state.json` | `last_pattern_scan_at` (skip if <5h ago) |
| `raw/transcripts/` (last 7 days of .md summaries) | Session summaries to scan |
| `me/learnings.md` | Current learnings (pattern candidates to elevate) |

### What it writes

| Destination | What |
|---|---|
| `evolution/observations.md` | Appends new auto-detected patterns with `[AUTO]` tag |
| `evolution/standing-order-candidates.md` | New file: patterns appearing 3+ times in 7 days |
| `.hex/dream-state.json` | Updates `last_pattern_scan_at` |

### LLM calls

**Call 1 — Pattern extraction** (1 call per 6h run)
- Model: `claude-haiku-4-5`
- Input: last 7 days of session summaries (not raw transcripts) + existing observations headings
- Output: JSON array of patterns with frequency counts and source session IDs
- **Estimated cost:** ~$0.003 per run (using summaries, not raw transcripts)

**Call 2 — Standing order candidate judgment** (conditional, only if new patterns found)
- Model: `claude-sonnet-4-6` (higher quality needed for this judgment call)
- Input: candidate patterns + existing standing orders
- Output: ranked list with "promote to standing order" / "observe more" / "false positive" classification
- **Estimated cost:** ~$0.01 per run

**Total per 6h run: ~$0.003–0.013**
**Daily cost: ~$0.012–0.052**

### hex-events policy

```yaml
# system/events/policies/dream-pattern-detection.yaml
name: dream-pattern-detection
description: >
  Every 6 hours, scan recent session summaries for recurring patterns
  that should become standing orders or automations.

rate_limit:
  max_fires: 1
  window: 5h

rules:
  - name: detect-patterns
    trigger:
      event: timer.tick.6h
    actions:
      - type: shell
        command: |
          python3 $HEX_DIR/system/scripts/dream/detect_patterns.py
        timeout: 600
        on_success:
          - type: emit
            event: dream.patterns.detected
```

### Failure modes

| Failure | Behavior |
|---|---|
| No summaries available yet | Skip, log warning; wait until compression has run at least once |
| False positive patterns | Tagged `[AUTO]` so Mike can review; never auto-promote to hooks without review |
| Haiku misses a pattern | Acceptable; pattern will resurface in future runs; human review of observations.md is the safety net |

---

## Component 4: Knowledge Graph Update

**Goal:** As new distillation results arrive, keep the graph of people, projects, and relationships current. This enables future "who is X and what are they working on?" queries without needing to scan transcripts.

### Trigger

```
event: dream.distillation.complete
payload: { session_id }
```

Fires automatically after every successful Session Distillation run (Component 1).

### What it reads

| Source | Purpose |
|---|---|
| `.hex/distillation-output/<session_id>.json` | Output of the distillation step |
| `projects/*/context.md` | Existing project states |
| `me/people/*.md` | Existing people profiles |
| `.hex/knowledge-graph.json` | Current graph state |

### What it writes

| Destination | What |
|---|---|
| `.hex/knowledge-graph.json` | Incremental update: new nodes, edges, updated status fields |
| `projects/<name>/context.md` | Status field updated if distillation mentions project state change |
| `me/people/<name>.md` | New mentions or relationship updates appended |

### LLM calls

**Call 1 — Entity/relationship extraction** (1 call per distillation)
- Model: `claude-haiku-4-5`
- Input: distillation JSON (learnings + decisions) — not the raw transcript
- Output: JSON delta: `{new_nodes: [...], updated_nodes: [...], new_edges: [...]}`
- **Estimated cost:** ~$0.001 per session (distillation output is small)

**Total per session: ~$0.001**
**Daily cost: ~$0.005** (5 sessions/day average)

### Knowledge graph schema

```json
{
  "nodes": {
    "<id>": {
      "type": "person | project | tool | concept",
      "name": "...",
      "status": "active | stale | archived",
      "last_mentioned": "2026-04-27",
      "summary": "...",
      "source_sessions": ["session_id_1", "session_id_2"]
    }
  },
  "edges": [
    {
      "from": "<node_id>",
      "to": "<node_id>",
      "relationship": "works_on | decided | uses | mentioned_with",
      "first_seen": "2026-04-20",
      "last_seen": "2026-04-27",
      "strength": 1
    }
  ],
  "meta": {
    "last_updated": "2026-04-27T14:30:00Z",
    "total_sessions_processed": 42
  }
}
```

### Failure modes

| Failure | Behavior |
|---|---|
| knowledge-graph.json corrupted | Read from `.hex/knowledge-graph.json.bak` (written before each update) |
| Entity resolution ambiguity (two "Mike"s?) | Flag in graph node with `ambiguous: true`; human resolves |
| Context.md update conflicts | Skip if coordination lock held; graph update committed anyway |

---

## Component 5: Memory Pruning

**Goal:** Remove or archive entries that are stale, contradicted, or duplicated — preventing hex from acting on outdated information.

### Trigger

```
event: timer.tick.weekly
payload: { iso_week }
```

*Note: Add `timer.tick.weekly` to scheduler. Fires Sunday at 3am UTC.*

### What it reads

| Source | Purpose |
|---|---|
| `me/learnings.md` | All learnings |
| `me/decisions/*.md` | All decisions |
| `evolution/behavioral-escalations.md` | All corrections |
| `raw/transcripts/summaries/` (last 30 days) | Evidence for contradiction detection |
| `.hex/knowledge-graph.json` | Node staleness signals |

### What it writes

| Destination | What |
|---|---|
| `me/learnings.md` | Learnings with `[STALE?]` tag prepended (never auto-deleted) |
| `me/decisions/_archive/` | Superseded decisions moved here |
| `evolution/pruning-report.md` | Human-readable report of what was flagged |
| `.hex/dream-state.json` | Updates `last_pruning_at` |

### Pruning rules (deterministic, no LLM needed for most)

| Rule | Condition | Action |
|---|---|---|
| **Stale learning** | Learning references a project/tool not mentioned in last 90 days | Tag `[STALE?]` |
| **Superseded decision** | Two decisions on same topic, newer one explicitly contradicts older | Archive older |
| **Duplicate learning** | cosine similarity >0.95 between two learning entries (using cached embeddings from t-2 work) | Tag both `[DUPLICATE?]`; suggest merge |
| **Contradicted correction** | Behavioral escalation not triggered in last 60 days | Tag `[INACTIVE]` |

**LLM calls:** Only needed for contradiction detection when deterministic rules can't resolve.
- **Estimated cost:** ~$0.01 per weekly run

### hex-events policy

```yaml
# system/events/policies/dream-memory-pruning.yaml
name: dream-memory-pruning
description: >
  Weekly scan to flag stale learnings, archive superseded decisions,
  and surface duplicate entries for review.

rate_limit:
  max_fires: 1
  window: 6d

rules:
  - name: prune-weekly
    trigger:
      event: timer.tick.weekly
    actions:
      - type: shell
        command: |
          python3 $HEX_DIR/system/scripts/dream/prune_memory.py
        timeout: 600
        on_success:
          - type: emit
            event: dream.pruning.complete
```

### Failure modes

| Failure | Behavior |
|---|---|
| False positive pruning | Items are tagged, never auto-deleted; Mike reviews pruning-report.md |
| Embeddings not available | Fall back to FTS5 title similarity only; tag fewer items |
| me/learnings.md very large | Process in 100-line chunks; write partial results |

---

## Shared Infrastructure

### `dream-state.json`

Central state file read by all dream components to coordinate and avoid redundant work.

```json
{
  "last_distillation_at": "2026-04-27T14:30:00Z",
  "last_compression_at": "2026-04-27T02:15:00Z",
  "last_pattern_scan_at": "2026-04-27T12:00:00Z",
  "last_graph_update_at": "2026-04-27T14:31:00Z",
  "last_pruning_at": "2026-04-21T03:00:00Z",
  "pending_graph_updates": [],
  "distillation_queue": [],
  "compression_queue": []
}
```

### `system/scripts/dream/` directory structure

```
system/scripts/dream/
  __init__.py
  distill_session.py     # Component 1
  compress_memory.py     # Component 2
  detect_patterns.py     # Component 3
  update_graph.py        # Component 4
  prune_memory.py        # Component 5
  dream_state.py         # Shared: read/write dream-state.json
  llm_client.py          # Shared: Anthropic SDK wrapper with caching
```

### Cost summary

| Component | Trigger | Per-run cost | Daily cost |
|---|---|---|---|
| Session distillation | Per session | ~$0.005 | ~$0.025 (5 sessions/day) |
| Memory compression | Daily | ~$0.05–0.15 | ~$0.10 |
| Pattern detection | Every 6h | ~$0.003–0.013 | ~$0.012–0.052 |
| Graph update | Per distillation | ~$0.001 | ~$0.005 |
| Memory pruning | Weekly | ~$0.01 | ~$0.001 |
| **Total** | | | **~$0.14–0.18/day** |

$0.15/day ≈ $55/year for fully autonomous memory health. This assumes haiku for distillation/compression/detection and sonnet only for standing order judgment calls.

---

## New hex-events Timer Events Required

Two new timer intervals needed (add to `adapters/scheduler.py`):

| Event | Interval | Purpose |
|---|---|---|
| `timer.tick.6h` | Every 6 hours | Pattern detection |
| `timer.tick.weekly` | Weekly (Sunday 3am UTC) | Memory pruning |

The existing `timer.tick.daily` (already present) covers memory compression.

---

## Rollout Order

The components have data dependencies that dictate rollout order:

1. **Session distillation first** — produces the summaries that compression and pattern detection depend on
2. **Graph update second** — depends on distillation output JSON
3. **Memory compression third** — needs at least 7 days of distillation output before first useful run
4. **Pattern detection fourth** — needs summaries from compression
5. **Memory pruning last** — needs embeddings from t-2 work and sufficient history

The policies are independent (separate YAML files), so each can be enabled as its component is ready without affecting others.

---

## Design Rationale

**Why haiku for most calls, not sonnet?**
Distillation, compression, and graph extraction are structured extraction tasks where the input format is consistent and the output is JSON. Haiku handles these reliably at 1/20th the cost. Sonnet is reserved for judgment calls (standing order promotion) where nuance matters.

**Why tag rather than delete?**
Pruning false positives are more damaging than stale entries. Tagging with `[STALE?]` preserves the human's ability to review before permanent removal. The knowledge graph provides a second signal for staleness, reducing false positive rate over time.

**Why not a single "dream agent" that does everything?**
Each component has a different trigger cadence, failure surface, and cost profile. Splitting into independent policies means a failure in compression doesn't block distillation. It also makes it easy to disable one component without affecting others (e.g., pause compression during a cost spike).

**Why `dream.distillation.complete` triggers graph update instead of a timer?**
The graph should reflect session content as soon as it's extracted — a 6h delay would make "who is working on what right now?" queries stale. Tying graph update to distillation completion keeps latency low without adding another polling interval.
