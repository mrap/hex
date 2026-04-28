use chrono::Utc;
use hex::state;
use hex::types::TrailEntry;
use hex::wake::{check_and_handle_loop, compute_action_hash};

fn make_trail_entry(entry_type: &str, key: &str, val: &str) -> TrailEntry {
    TrailEntry {
        ts: Utc::now(),
        entry_type: entry_type.to_string(),
        detail: serde_json::json!({ key: val }),
        queue_item: None,
    }
}

#[test]
fn test_loop_not_triggered_on_varied_actions() {
    let dir = tempfile::TempDir::new().unwrap();
    let state_path = dir.path().join("state.json");
    let mut agent_state = state::initialize("test-varied", 1.0);

    let e1 = make_trail_entry("observe", "target", "service-alpha");
    let e2 = make_trail_entry("observe", "target", "service-beta");
    let e3 = make_trail_entry("observe", "target", "service-gamma");

    // Use a fake hex_dir and audit_dir that exist (state dir is enough for audit)
    let hex_dir = dir.path().to_path_buf();
    let audit_dir = dir.path().join("audit");
    std::fs::create_dir_all(&audit_dir).unwrap();

    let interval = 3600u64;
    let r1 = check_and_handle_loop(&mut agent_state, &[e1], interval, &hex_dir, &audit_dir);
    let r2 = check_and_handle_loop(&mut agent_state, &[e2], interval, &hex_dir, &audit_dir);
    let r3 = check_and_handle_loop(&mut agent_state, &[e3], interval, &hex_dir, &audit_dir);

    assert!(!r1, "loop should not trigger on first varied entry");
    assert!(!r2, "loop should not trigger on second varied entry");
    assert!(!r3, "loop should not trigger on three distinct entries");

    // No HALT-loop file should exist anywhere in the temp dir
    let halt_files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("HALT-loop"))
        .collect();
    assert!(halt_files.is_empty(), "no HALT-loop file should be written for varied actions");

    // Save state to ensure it serializes cleanly with the new field
    state::save(&agent_state, &state_path).unwrap();
    let loaded = state::load(&state_path).unwrap();
    assert_eq!(loaded.recent_action_hashes.len(), 3);
}

#[test]
fn test_loop_triggered_on_repeated_observe() {
    let dir = tempfile::TempDir::new().unwrap();

    // Point HOME at temp dir so HALT-loop file lands there
    let fake_home = dir.path().to_path_buf();
    std::env::set_var("HOME", &fake_home);

    let mut agent_state = state::initialize("test-repeat", 1.0);

    let same_entry = make_trail_entry("observe", "target", "same-service");

    let hex_dir = dir.path().to_path_buf();
    let audit_dir = dir.path().join("audit");
    std::fs::create_dir_all(&audit_dir).unwrap();

    let interval = 3600u64;
    let r1 = check_and_handle_loop(&mut agent_state, &[same_entry.clone()], interval, &hex_dir, &audit_dir);
    let r2 = check_and_handle_loop(&mut agent_state, &[same_entry.clone()], interval, &hex_dir, &audit_dir);
    let r3 = check_and_handle_loop(&mut agent_state, &[same_entry.clone()], interval, &hex_dir, &audit_dir);

    assert!(!r1, "loop should not trigger after first occurrence");
    assert!(!r2, "loop should not trigger after second occurrence");
    assert!(r3, "loop should trigger after third identical observe");

    let halt_path = fake_home.join(".hex-test-repeat-HALT-loop");
    assert!(
        halt_path.exists(),
        "HALT-loop file should be written at {:?}",
        halt_path
    );

    let content = std::fs::read_to_string(&halt_path).unwrap();
    assert!(content.contains("Loop detected"), "HALT file should contain loop message");
}

#[test]
fn test_compute_action_hash_deterministic() {
    let detail = serde_json::json!({"z": "last", "a": "first"});
    let h1 = compute_action_hash("agent-x", "observe", &detail);
    let h2 = compute_action_hash("agent-x", "observe", &detail);
    assert_eq!(h1, h2, "same inputs must produce same hash");
    assert_eq!(h1.len(), 16, "hash must be 16 chars");
}

#[test]
fn test_compute_action_hash_key_order_invariant() {
    // Keys sorted, so order in JSON shouldn't matter
    let d1 = serde_json::json!({"a": "1", "b": "2"});
    let d2 = serde_json::json!({"b": "2", "a": "1"});
    let h1 = compute_action_hash("agent-y", "verify", &d1);
    let h2 = compute_action_hash("agent-y", "verify", &d2);
    assert_eq!(h1, h2, "hash must be key-order invariant");
}
