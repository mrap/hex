# Intelligent Routing Architecture Proposal

> Design document for the hex Intelligent Session Routing initiative.
> Depends on: `memory-architectures.md` (t-1), `memory-benchmark.md` (t-2)
> **Date:** 2026-04-27 | **Author:** BOI Worker q-919 (Iteration 3)

---

## Executive Summary

Hex currently routes messages via a fan-out LLM classifier (`route-message-llm.py`) but has no
automated context loading, session lifecycle management, or session affinity. The user must manually
invoke `/hex-checkpoint`, choose which session to continue, and manage context limits.

This proposal redesigns routing as a four-layer pipeline:

```
Inbound message
      ↓
[1] Message Classification  (intent + topic + urgency)
      ↓
[2] Session Affinity Lookup  (find or create the right session)
      ↓
[3] Context Loading          (hybrid memory → bounded context window)
      ↓
[4] Session Lifecycle Gate   (checkpoint / compact / resume as needed)
      ↓
Agent execution
```

Each layer is designed to be operable with **zero user intervention**.

---

## Layer 1: Message Classification

### What Needs to Be Determined

When a message arrives (from Slack, the hex-ui, or CLI), the routing pipeline must extract:

| Dimension | Values | Why It Matters |
|-----------|--------|----------------|
| **Intent** | question, request, feedback, status_check, command | Determines which agent type should handle it |
| **Topic** | project name, domain (boi, hex, social, personal), or free-form | Controls session affinity and context preloading |
| **Urgency** | immediate (needs reply < 30s), normal, background | Determines whether to queue or immediately dispatch |

### Current State

`route-message-llm.py` does **fan-out classification**: it scores every agent charter against the
incoming message and returns a confidence list. This is a solid foundation. The model is
`google/gemini-2.5-flash-lite` via OpenRouter (1.1s, ~$1/month at current volume).

### Gap Analysis

The current classifier is **sufficient for agent selection** but is missing two things needed for
the full routing pipeline:

1. **No urgency triage** — all messages are treated as equivalent priority.
2. **No topic signal for context preloading** — the router returns which agents should handle it,
   but does not emit a topic that the context loader can use to preload memory.

### Proposed Enhancement: Structured Classification Response

Extend the router prompt to emit a structured classification alongside the per-agent scores:

```json
{
  "agents": [
    {"agent": "hex-main", "confidence": 0.92, "reason": "session management question"},
    {"agent": "boi-meta", "confidence": 0.61, "reason": "BOI architecture topic"}
  ],
  "classification": {
    "intent": "question",
    "topic": "boi",
    "urgency": "normal",
    "topic_keywords": ["BOI", "worker", "iteration", "failure rate"]
  }
}
```

This is a **backward-compatible extension** — the existing agent confidence list remains, and the
new `classification` block is added. No existing consumers break.

**Cost impact:** Same model, same call. Prompt grows by ~100 tokens per call. Net increase: negligible.

### Is route-message-llm.py Sufficient?

**Yes, with the structured response extension.** The core LLM classification approach is correct.
Gemini-2.5-flash-lite achieves 100% accuracy on the current agent routing task. The enhancement
adds topic + urgency metadata in the same call without adding latency.

---

## Layer 2: Session Affinity

### Goal

*Same topic → same session. Different topic → different session. No session → create one.*

Session affinity prevents context fragmentation where related messages get split across multiple
agent sessions with different (or no) context.

### Affinity Key

Sessions are keyed by `(channel_key, topic)`. The channel_key comes from the inbound message
(e.g., `slack:C0AQGHS8RNG:U0AQACA26NS` for hex-main). The topic comes from the classifier output.

```
affinity_key = f"{channel_key}:{topic}"
# e.g., "slack:C0AQGHS8RNG:U0AQACA26NS:boi"
#        "slack:C0AQGHS8RNG:U0AQACA26NS:hex-social"
#        "slack:C0AQGHS8RNG:U0AQACA26NS:personal"
```

### Affinity Table

Maintained in `~/.cc-connect/sessions.db` (new table alongside existing session records):

```sql
CREATE TABLE session_affinity (
    affinity_key     TEXT PRIMARY KEY,
    session_id       TEXT NOT NULL,
    last_active_at   TEXT NOT NULL,     -- ISO8601
    context_pct      REAL DEFAULT 0.0,  -- 0.0–1.0
    checkpointed_at  TEXT,              -- last checkpoint time, nullable
    checkpoint_path  TEXT               -- path to latest .hex/checkpoints/ file
);
```

### Lookup Logic

```
1. Classify message → get topic
2. Compute affinity_key = channel_key + ":" + topic
3. Look up affinity_key in session_affinity
4. If found and context_pct < 0.85:
     → resume existing session (use session_id)
5. If found and context_pct >= 0.85:
     → auto-compact (see Layer 4), then resume
6. If not found:
     → create new session, insert into session_affinity
```

### Multi-Topic Messages

If the classifier returns high confidence for two different topics (e.g., boi + social), the
router fans out to two sessions in parallel — one per topic. This already mirrors the existing
fan-out behavior, just now with session-affinity tracking.

---

## Layer 3: Context Loading

### What Context to Load

When a session is created or resumed, the pipeline needs to load *relevant* context into the
agent's window. Based on the memory benchmark (t-2), the hierarchy is:

| Priority | Source | How Loaded | Relevance |
|----------|--------|------------|-----------|
| 1 (highest) | Latest checkpoint file for this affinity key | Direct file read | Always load first |
| 2 | Hybrid memory query on topic_keywords | FTS5 + semantic RRF | Top 5–10 chunks |
| 3 | Project `context.md` for matched project | Direct file read | If topic → known project |
| 4 | Recent session transcript summary | Last N lines of session log | Last 3 exchanges as warmup |
| 5 (lowest) | Behavioral corrections | `behavioral_memory.py` lookup | Top 3 patterns |

### Token Budget Management

The context loader must respect the model's context window. Strategy:

```
total_budget = model_context_window  (e.g., 200,000 tokens for claude-sonnet-4-6)
reserved_for_response = 4,000 tokens
reserved_for_system_prompt = 8,000 tokens
available_for_context = total_budget - reserved_for_response - reserved_for_system_prompt

Load in priority order:
  1. Checkpoint (if exists): up to min(50,000, available_for_context * 0.5) tokens
  2. Memory chunks: up to 10,000 tokens
  3. Project context.md: up to 5,000 tokens
  4. Recent exchanges: up to 3,000 tokens
  5. Behavioral corrections: up to 2,000 tokens
  Total cap: available_for_context
```

This implements **context-window-aware retrieval** (Architecture #6 from t-1): load until the
token budget is filled, prioritizing recency and relevance.

### Auto-Load vs. Lazy-Load

**Recommendation: preload at session dispatch time, not on-demand.**

Rationale:
- The routing pipeline already knows topic + affinity before the agent starts.
- Preloading means the agent begins with full context, rather than needing a "what do I know
  about X?" retrieval call inside the session (which adds latency and consumes context).
- The only exception: very long sessions (context_pct > 0.5 at resume time) should lazy-load
  secondary sources to leave room for new content.

### Context Loader API

```python
# system/scripts/context_loader.py

def load_context(
    affinity_key: str,
    topic_keywords: list[str],
    model_context_tokens: int = 200_000,
) -> ContextBundle:
    """
    Returns a ContextBundle with:
      - checkpoint_path: str | None
      - memory_chunks: list[MemoryChunk]   # from hybrid retrieval
      - project_context: str | None
      - behavioral_corrections: list[str]
      - total_token_estimate: int
    """
    ...
```

---

## Layer 4: Session Lifecycle

### The Problem

Claude Code sessions accumulate context indefinitely. When context fills, the session becomes
unresponsive or incoherent. Currently, `/hex-checkpoint` is invoked manually. This is friction.

### Auto-Checkpoint at 70%

When `context_pct` reaches 0.70, the routing layer triggers an automatic checkpoint **before**
dispatching the next message:

```
1. Dispatch subagent: run /hex-checkpoint (background, non-blocking)
2. Update session_affinity: checkpointed_at = now, checkpoint_path = new checkpoint
3. Continue routing: the message is delivered to the same session
   (checkpoint runs alongside; session is not reset yet)
```

The checkpoint at 70% is **proactive** — there is still plenty of room for the checkpoint
process itself (which adds content) and for the user's next N messages.

### Auto-Compact at 85%

When `context_pct` reaches 0.85, the routing layer does a **hard compact before delivery**:

```
1. Wait for any in-progress checkpoint to complete (max 30s timeout)
2. Reset the session: clear agent_session_id in cc-connect session file
   (session reset now done via /hex-shutdown slash command)
3. On next message delivery, context loader re-hydrates from checkpoint
4. Update session_affinity: context_pct = estimated post-compact value
```

This ensures the session never crosses the hard limit while preserving context via checkpoint.

### Idle Timeout: 60-Minute Auto-Checkpoint

A background cron (hex-events `timer.tick.hourly`) scans `session_affinity` for sessions where
`last_active_at < now - 60min` and `checkpointed_at < last_active_at`:

```python
# In hex-events action: session-idle-checkpoint
SELECT * FROM session_affinity
WHERE last_active_at < datetime('now', '-60 minutes')
  AND (checkpointed_at IS NULL OR checkpointed_at < last_active_at)
```

For each idle session, dispatch `/hex-checkpoint` as a background BOI task. This means sessions
are always checkpointed before they go cold, so the next resume has current state.

### Seamless Resume

From the user's perspective:

1. **Message arrives** → classifier runs (1.1s)
2. **Session affinity lookup** → finds existing session for this topic
3. **Context loader** → attaches checkpoint + relevant memory (100–500ms depending on retrieval)
4. **Message delivered** → agent responds with full context

The user never sees "starting fresh." There is no startup friction.

### Session State Diagram

```
          new topic
              │
              ▼
         ┌─────────┐
         │  INIT   │  ← create affinity entry, preload context
         └────┬────┘
              │
              ▼
         ┌─────────┐
         │ ACTIVE  │  ← messages flowing, context_pct climbing
         └────┬────┘
              │
       ┌──────┴──────┐
       │             │
   context≥70%  idle>60min
       │             │
       ▼             ▼
  ┌──────────┐  ┌──────────┐
  │CHECKPOINT│  │ DORMANT  │ ← idle checkpoint, no active session
  └────┬─────┘  └────┬─────┘
       │             │
       │         next msg
       │             │
       └──────┬───────┘
              │
              ▼
         ┌─────────┐
         │ ACTIVE  │  (resumed)
         └────┬────┘
              │
        context≥85%
              │
              ▼
         ┌─────────┐
         │ COMPACT │  ← reset session, re-hydrate from checkpoint
         └────┬────┘
              │
              ▼
         ┌─────────┐
         │ ACTIVE  │  (fresh window, full context)
         └─────────┘
```

---

## Layer 5: Multi-Channel Routing

### Current Architecture

```
Slack  →  cc-connect (bridge)  →  Claude Code session
                                         ↑
                                   hex binary (internal)
```

`cc-connect` owns session IDs and maps Slack channel keys to agent sessions. The `hex` binary
(run inside Claude Code) handles all context operations — checkpoint, compact, memory retrieval.

### The Question

> Should the hex binary own session routing instead of cc-connect? Or does cc-connect stay as
> the Slack bridge and hex handles context internally?

### Recommendation: Transport/Context Separation

**cc-connect stays as the transport layer. A new `hex-session-manager` component owns context.**

Rationale:

| Concern | cc-connect | hex-session-manager |
|---------|-----------|---------------------|
| Protocol bridging (Slack → CC) | ✓ stays here | ✗ not its job |
| Session ID lifecycle | ✓ owns CC session IDs | ✗ delegates to cc-connect |
| Topic classification | ✗ no LLM | ✓ calls route-message-llm.py |
| Context loading | ✗ no memory access | ✓ full memory access |
| Affinity table | ✗ | ✓ owns session_affinity |
| Checkpoint trigger | ✗ (currently manual) | ✓ policy-driven |

The new flow:

```
Slack message
     ↓
cc-connect (transport: Slack → HTTP)
     ↓
hex-session-manager (NEW)
  → classify message (route-message-llm.py + classification extension)
  → affinity lookup
  → context loading (context_loader.py)
  → lifecycle check (checkpoint/compact if needed)
  → forward to cc-connect with session_id + preloaded context
     ↓
cc-connect (delivers to Claude Code session with context)
     ↓
Agent execution
```

`cc-connect` becomes a pure Slack ↔ CC bridge. All intelligence moves to `hex-session-manager`.

### Other Channels (UI, CLI)

The same `hex-session-manager` handles UI messages (via hex-ui's `/api/message` endpoint) and
CLI invocations (`hex message`). Channel key format:

- Slack: `slack:CHANNEL_ID:USER_ID`
- hex-ui: `ui:session_uuid`
- CLI: `cli:hostname:pid`

All channels share the same affinity table and session lifecycle logic.

---

## Implementation Sketch

### New Files

```
system/scripts/
  context_loader.py          ← token-budget-aware context assembly
  session_manager.py         ← affinity table + lifecycle state machine
  route-message-llm.py       ← MODIFIED: add classification block to response

system/events/actions/
  session-idle-checkpoint/   ← hex-events action: checkpoint idle sessions
    action.sh

~/.cc-connect/
  sessions.db                ← add session_affinity table (migration)
```

### Migration Plan

1. **Phase 0** (no behavior change): Add `session_affinity` table to sessions.db. Log affinity
   events passively. Validate classification data.

2. **Phase 1** (context loading): Enable context_loader.py for new sessions. Agent starts with
   preloaded checkpoint + relevant memory. Manual checkpoint/compact still works.

3. **Phase 2** (lifecycle automation): Enable auto-checkpoint at 70%, auto-compact at 85%,
   idle-checkpoint via hex-events. Remove the need for manual `/hex-checkpoint`.

4. **Phase 3** (full session manager): Deploy hex-session-manager as cc-connect middleware.
   cc-connect becomes a pure bridge.

---

## Open Questions

1. **Context_pct tracking**: Claude Code does not expose token usage in a machine-readable way
   during a session. Options: (a) count tokens client-side via tiktoken, (b) use the
   `context-budget` ECC skill's heuristics, (c) use time + message count as a proxy.
   Recommended: (b) — ECC context-budget provides estimated % used.

2. **Cold-start latency**: Preloading context at session init adds ~100–500ms. For most messages
   this is fine (classification already takes 1.1s). For urgent messages, this may push total
   routing overhead to ~2s. Acceptable for Slack; monitor for CLI use cases.

3. **Checkpoint race conditions**: If two messages arrive simultaneously in the same topic session,
   both could trigger a checkpoint. The coordination.py lock (already in use in BOI) should wrap
   checkpoint dispatch.

---

## Decision Rationale: Context Loading Strategy

**Decision:** Preload at dispatch time (eager), not lazy-load on-demand inside the session.

| Option | Description | Score |
|--------|-------------|:-----:|
| **Eager preload** | Load checkpoint + memory before message delivery | 4.5 |
| Lazy on-demand | Agent requests context as needed via tool calls | 3.0 |
| No-load (current) | Agent starts cold, user provides context manually | 1.5 |

**Margin:** 4.5 vs 3.0 — moderate

**Key trade-off:** Eager preload adds ~100–500ms per message but gives the agent full context
from word one. Lazy loading saves dispatch latency but requires the agent to spend tokens and
time fetching context inside the session, and may miss context the agent doesn't know to ask for.

**Assumptions that could change the verdict:**
- If context loading latency consistently exceeds 2s, lazy-load for less-urgent messages becomes
  more attractive.
- If the classification topic is frequently wrong, eager preloading the wrong context wastes the
  token budget.

**Dissenting view:** Lazy loading is more efficient when the agent already has relevant context
from a recent checkpoint and doesn't need fresh memory retrieval. Forcing preload every message
wastes tokens on context the agent may never reference.

---

## Decision Rationale: cc-connect vs. hex-session-manager

**Decision:** Keep cc-connect as a transport bridge; build hex-session-manager for all routing logic.

| Option | Description | Score |
|--------|-------------|:-----:|
| **hex-session-manager (new component)** | Thin middleware layer between cc-connect and agent | 4.5 |
| Extend cc-connect | Add routing logic directly into cc-connect | 2.5 |
| hex binary owns routing | All routing inside Claude Code session | 2.0 |

**Margin:** 4.5 vs 2.5 — clear winner

**Key trade-off:** A new component adds operational surface area (one more process to run and monitor),
but keeps cc-connect focused on protocol bridging and makes the routing logic independently testable
and upgradeable without touching the Slack bridge.

**Assumptions that could change the verdict:**
- If cc-connect already has a plugin/middleware system, extending it directly may be simpler.
- If hex-session-manager adds significant memory footprint (it shouldn't — it's a lightweight Python daemon), reconsider.

**Dissenting view:** Fewer moving parts is better. Extending cc-connect directly avoids introducing
a new daemon, a new IPC boundary, and new failure modes. Complexity often hides in the seams between
components.
