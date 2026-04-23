use crate::types::{AgentState, Charter};

pub fn build(
    charter_text: &str,
    state: &AgentState,
    trigger: &str,
    payload: &str,
    principles_text: Option<&str>,
) -> String {
    let trail_recent: Vec<_> = state.trail.iter().rev().take(20).collect();
    let trail_json = serde_json::to_string_pretty(&trail_recent).unwrap_or_default();
    let queue_json = serde_json::to_string_pretty(&state.queue).unwrap_or_default();
    let memory_json = serde_json::to_string_pretty(&state.memory).unwrap_or_default();
    let inbox_json = serde_json::to_string_pretty(&state.inbox).unwrap_or_default();
    let cost_json = serde_json::to_string_pretty(&state.cost).unwrap_or_default();
    let initiatives_json = serde_json::to_string_pretty(&state.initiatives).unwrap_or_default();

    let principles_section = principles_text
        .map(|p| format!("\n---\n\n{p}\n"))
        .unwrap_or_default();

    format!(
        r#"# Charter

{charter_text}
{principles_section}
---

# Wake Context

- Trigger: {trigger}
- Payload: {payload}
- Wake count: {wake_count}
- Last wake: {last_wake}

## Queue

```json
{queue_json}
```

## Inbox (unread messages)

```json
{inbox_json}
```

## Recent trail (last 20 entries)

```json
{trail_json}
```

## Working memory

```json
{memory_json}
```

## Initiatives

```json
{initiatives_json}
```

## Cost

```json
{cost_json}
```

---

# Instructions

Work your active queue. For each item, use the action types below to observe, analyze, decide, and act. You choose the workflow -- the harness logs everything.

When you're done with all active items (or have moved them to blocked/parked), set `active_drained: true`.

## Action types

Each trail entry must have `type` and `detail` fields. Required detail fields per type:

| Type | Required fields |
|------|----------------|
| observe | what, noted |
| find | finding, evidence |
| decide | decision, alternatives, reasoning |
| act | action, result |
| verify | check, evidence, status |
| delegate | initiative_id, to, context |
| park | item_id, reason, resume_condition |
| reframe | abandoned, reason, new_framing |
| message_sent | to, subject, body |

## Response format

Return a single JSON object (AgentResponse) with exactly these fields:

```json
{{
  "trail": [
    {{"ts": "ISO-8601", "type": "observe", "detail": {{"what": "...", "noted": "..."}}, "queue_item": "t-1"}}
  ],
  "queue_updates": {{
    "completed": ["t-1"],
    "added_active": [],
    "moved_to_blocked": [],
    "parked": []
  }},
  "memory_updates": {{"key": "value"}},
  "outbound_messages": [],
  "active_drained": true
}}
```

Respond ONLY with the JSON object. No prose before or after.
"#,
        wake_count = state.wake_count,
        last_wake = state
            .last_wake
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "never".into()),
    )
}

pub fn build_assessment(
    charter: &Charter,
    state: &AgentState,
    principles_text: Option<&str>,
) -> String {
    let trail_recent: Vec<_> = state.trail.iter().rev().take(50).collect();
    let trail_json = serde_json::to_string_pretty(&trail_recent).unwrap_or_default();
    let memory_json = serde_json::to_string_pretty(&state.memory).unwrap_or_default();
    let cost_json = serde_json::to_string_pretty(&state.cost).unwrap_or_default();
    let cadence_json = serde_json::to_string_pretty(&state.cadence_overrides).unwrap_or_default();

    let kpis_section = charter
        .kpis
        .as_ref()
        .map(|kpis| {
            kpis.iter()
                .map(|k| format!("- {k}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "(none defined)".to_string());

    let responsibilities_section = charter
        .wake
        .responsibilities
        .iter()
        .map(|r| {
            let effective = state
                .cadence_overrides
                .get(&r.name)
                .copied()
                .or(r.interval);
            let override_note = if state.cadence_overrides.contains_key(&r.name) {
                format!(" (overridden from charter default {}s)", r.interval.map_or_else(|| "event".to_string(), |i| i.to_string()))
            } else {
                String::new()
            };
            let interval_str = effective.map_or_else(|| "event-triggered".to_string(), |i| format!("every {}s", i));
            format!(
                "- *{}*: {} {} — {}",
                r.name,
                interval_str,
                override_note,
                r.description.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let principles_section = principles_text
        .map(|p| format!("\n---\n\n{p}\n"))
        .unwrap_or_default();

    format!(
        r#"# Self-Assessment Phase

You are {name} ({role}).
{principles_section}
## Your Objective

{objective}

## Your KPIs

{kpis_section}

## Current Responsibility Cadences

{responsibilities_section}

## Active Cadence Overrides

```json
{cadence_json}
```

## Recent Trail (last 50 entries)

```json
{trail_json}
```

## Working Memory

```json
{memory_json}
```

## Cost

```json
{cost_json}
```

---

# Instructions

Step back and assess your own effectiveness. This is not a work phase — this is reflection.

Consider:
1. Are your current cadences right? Too frequent wastes budget. Too infrequent misses opportunities.
2. Is your strategy working? Look at your trail — are you making progress on KPIs or spinning?
3. What should you do differently next shift?

For each observation, log a trail entry with type "assess".

## Response format

Return a single JSON object (AssessmentResponse):

```json
{{{{
  "trail": [
    {{{{"ts": "ISO-8601", "type": "assess", "detail": {{{{"area": "cadence", "finding": "experiment-execution runs every 6h but experiments take 3-5 days to produce signal", "adjustment": "moving to 24h"}}}}}}}}
  ],
  "cadence_overrides": [
    {{{{"responsibility": "experiment-execution", "old_interval": 21600, "new_interval": 86400, "reason": "experiments need days not hours to produce signal"}}}}
  ],
  "strategy_updates": {{{{"key": "value to persist in working memory"}}}},
  "recommendations": ["any recommendations for the fleet or for Mike"]
}}}}
```

Rules:
- Every cadence change MUST have a reason grounded in evidence from your trail.
- If everything is working, say so and return empty overrides. Don't change for the sake of change.
- Strategy updates persist to your working memory for future wakes.

Respond ONLY with the JSON object. No prose before or after.
"#,
        name = charter.name,
        role = charter.role,
        objective = charter.objective.as_deref().unwrap_or("(none)"),
    )
}
