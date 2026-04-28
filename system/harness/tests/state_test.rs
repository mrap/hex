use hex::state;
use std::path::PathBuf;

#[test]
fn test_initialize_new_state() {
    let s = state::initialize("test-agent", 2.0);
    assert_eq!(s.agent_id, "test-agent");
    assert_eq!(s.wake_count, 0);
    assert!(s.queue.active.is_empty());
    assert!(s.queue.blocked.is_empty());
    assert!(s.queue.scheduled.is_empty());
    assert!(s.trail.is_empty());
    assert!(s.inbox.is_empty());
    assert_eq!(s.cost.lifetime_usd, 0.0);
    assert_eq!(s.cost.current_period.budget_usd, 2.0);
}

#[test]
fn test_save_and_load_roundtrip() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("state.json");
    let original = state::initialize("roundtrip-test", 5.0);
    state::save(&original, &path).unwrap();
    let loaded = state::load(&path).unwrap();
    assert_eq!(loaded.agent_id, original.agent_id);
    assert_eq!(loaded.wake_count, original.wake_count);
    assert_eq!(loaded.cost.current_period.budget_usd, 5.0);
}

#[test]
fn test_load_nonexistent_returns_error() {
    let result = state::load(&PathBuf::from("/nonexistent/state.json"));
    assert!(result.is_err());
}
