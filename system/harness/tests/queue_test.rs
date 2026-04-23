use chrono::{Duration, Utc};
use hex_agent::queue;
use hex_agent::types::*;
use std::collections::HashMap;

#[test]
fn test_promote_due_scheduled_items() {
    let now = Utc::now();
    let mut state = hex_agent::state::initialize("test", 2.0);
    state.queue.scheduled.push(ScheduledItem {
        id: "s-1".into(),
        summary: "Health check".into(),
        interval_seconds: 1800,
        last_run: Some(now - Duration::seconds(2000)),
        next_due: now - Duration::seconds(200),
    });
    state.queue.scheduled.push(ScheduledItem {
        id: "s-2".into(),
        summary: "Initiative review".into(),
        interval_seconds: 21600,
        last_run: Some(now - Duration::seconds(100)),
        next_due: now + Duration::seconds(21500),
    });
    let promoted = queue::promote_scheduled(&mut state.queue, now);
    assert_eq!(promoted, 1);
    assert_eq!(state.queue.active.len(), 1);
    assert_eq!(state.queue.active[0].summary, "Health check");
}

#[test]
fn test_inbox_creates_active_items() {
    let now = Utc::now();
    let mut state = hex_agent::state::initialize("test", 2.0);
    state.inbox.push(Message {
        id: "msg-1".into(),
        from: "cos".into(),
        to: "test".into(),
        subject: "Check v2-arch".into(),
        body: "Dead for 12 hours".into(),
        initiative_id: None,
        response_requested: false,
        in_reply_to: None,
        sent_at: now,
    });
    let created = queue::inbox_to_active(&mut state);
    assert_eq!(created, 1);
    assert_eq!(state.queue.active.len(), 1);
    assert!(state.queue.active[0].summary.contains("Check v2-arch"));
}

fn make_responsibilities(names: &[&str]) -> Vec<Responsibility> {
    names
        .iter()
        .map(|name| Responsibility {
            name: name.to_string(),
            interval: Some(3600),
            description: format!("Do {name}"),
        })
        .collect()
}

#[test]
fn test_auto_seed_empty_queue_all_responsibilities_become_active() {
    let now = Utc::now();
    let mut state = hex_agent::state::initialize("test", 2.0);
    let responsibilities = make_responsibilities(&["health-check", "review", "metrics"]);
    let overrides: HashMap<String, u64> = HashMap::new();

    let added = queue::auto_seed_from_charter(&mut state.queue, &responsibilities, &overrides, now);
    assert_eq!(added, 3, "all 3 responsibilities should be seeded");
    assert_eq!(state.queue.scheduled.len(), 3);

    let promoted = queue::promote_scheduled(&mut state.queue, now);
    assert_eq!(promoted, 3, "all 3 should promote immediately (due now)");
    assert_eq!(state.queue.active.len(), 3);
}

#[test]
fn test_auto_seed_partial_queue_only_missing_responsibility_added() {
    let now = Utc::now();
    let mut state = hex_agent::state::initialize("test", 2.0);
    // Pre-seed 2 of the 3 responsibilities
    for name in &["health-check", "review"] {
        state.queue.scheduled.push(ScheduledItem {
            id: format!("s-{name}"),
            summary: format!("Do {name}"),
            interval_seconds: 3600,
            last_run: Some(now - Duration::seconds(100)),
            next_due: now + Duration::seconds(3500),
        });
    }
    let responsibilities = make_responsibilities(&["health-check", "review", "metrics"]);
    let overrides: HashMap<String, u64> = HashMap::new();

    let added = queue::auto_seed_from_charter(&mut state.queue, &responsibilities, &overrides, now);
    assert_eq!(added, 1, "only 'metrics' should be added");
    assert_eq!(state.queue.scheduled.len(), 3);

    let promoted = queue::promote_scheduled(&mut state.queue, now);
    assert_eq!(promoted, 1, "only newly seeded 'metrics' is due now");
    assert_eq!(state.queue.active.len(), 1);
    assert_eq!(state.queue.active[0].id, "s-metrics");
}

#[test]
fn test_auto_seed_no_responsibilities_queue_stays_empty() {
    let now = Utc::now();
    let mut state = hex_agent::state::initialize("test", 2.0);
    let overrides: HashMap<String, u64> = HashMap::new();

    let added = queue::auto_seed_from_charter(&mut state.queue, &[], &overrides, now);
    assert_eq!(added, 0);
    assert_eq!(state.queue.scheduled.len(), 0);

    let promoted = queue::promote_scheduled(&mut state.queue, now);
    assert_eq!(promoted, 0);
    assert!(state.queue.active.is_empty(), "queue should remain empty — wake would skip");
}
