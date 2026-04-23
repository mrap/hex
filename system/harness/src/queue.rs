use crate::types::*;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// Seeds scheduled entries for charter responsibilities not yet in the queue.
/// Returns the number of new entries added.
/// On first wake (empty queue) all responsibilities are seeded as due-now.
/// On subsequent wakes, only newly added responsibilities are seeded.
pub fn auto_seed_from_charter(
    queue: &mut Queue,
    responsibilities: &[Responsibility],
    cadence_overrides: &HashMap<String, u64>,
    now: DateTime<Utc>,
) -> usize {
    let mut added = 0;
    for responsibility in responsibilities {
        // Skip event-triggered responsibilities (no interval); only seed timer-based ones.
        let base_interval = match responsibility.interval {
            Some(i) => i,
            None => continue,
        };
        let scheduled_id = format!("s-{}", responsibility.name);
        if !queue.scheduled.iter().any(|s| s.id == scheduled_id) {
            let interval = cadence_overrides
                .get(&responsibility.name)
                .copied()
                .unwrap_or(base_interval);
            queue.scheduled.push(ScheduledItem {
                id: scheduled_id,
                summary: responsibility.description.clone(),
                interval_seconds: interval,
                last_run: None,
                next_due: now,
            });
            added += 1;
        }
    }
    added
}

pub fn promote_scheduled(queue: &mut Queue, now: DateTime<Utc>) -> usize {
    let mut promoted = 0;
    for item in &mut queue.scheduled {
        if item.next_due <= now {
            queue.active.push(ActiveItem {
                id: item.id.clone(),
                summary: item.summary.clone(),
                priority: 0,
                created: now,
                source: format!("scheduled:{}", item.id),
            });
            item.last_run = Some(now);
            item.next_due = now + chrono::Duration::seconds(item.interval_seconds as i64);
            promoted += 1;
        }
    }
    promoted
}

pub fn promote_unblocked(queue: &mut Queue) -> usize {
    let mut to_promote: Vec<(String, String, i32)> = vec![];
    for item in &queue.blocked {
        if check_unblock_condition(item) {
            to_promote.push((item.id.clone(), item.summary.clone(), item.priority));
        }
    }
    let promoted = to_promote.len();
    let promote_ids: Vec<String> = to_promote.iter().map(|t| t.0.clone()).collect();
    queue.blocked.retain(|item| !promote_ids.contains(&item.id));
    for (id, summary, priority) in to_promote {
        queue.active.push(ActiveItem {
            id,
            summary,
            priority,
            created: Utc::now(),
            source: "unblocked".to_string(),
        });
    }
    promoted
}

fn check_unblock_condition(item: &BlockedItem) -> bool {
    match item.blocked_type.as_str() {
        "telemetry" => {
            if let Some(ref path) = item.blocked_ref {
                std::path::Path::new(path).exists()
            } else {
                false
            }
        }
        "timer" => {
            if let Some(ref ts_str) = item.blocked_ref {
                if let Ok(target) = ts_str.parse::<DateTime<Utc>>() {
                    return Utc::now() >= target;
                }
            }
            false
        }
        _ => false,
    }
}

pub fn inbox_to_active(state: &mut AgentState) -> usize {
    let count = state.inbox.len();
    for msg in &state.inbox {
        state.queue.active.push(ActiveItem {
            id: format!("inbox-{}", msg.id),
            summary: format!("[{}] {}", msg.from, msg.subject),
            priority: if msg.response_requested { 1 } else { 5 },
            created: Utc::now(),
            source: format!("message:{}", msg.id),
        });
    }
    count
}
