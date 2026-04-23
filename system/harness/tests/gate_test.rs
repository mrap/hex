use chrono::Utc;
use hex_agent::gate;
use hex_agent::types::TrailEntry;

fn make_entry(entry_type: &str, detail: serde_json::Value) -> TrailEntry {
    TrailEntry {
        ts: Utc::now(),
        entry_type: entry_type.to_string(),
        detail,
        queue_item: None,
    }
}

#[test]
fn test_valid_observe() {
    let entry = make_entry(
        "observe",
        serde_json::json!({"what": "log.jsonl", "noted": "healthy"}),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_valid_find() {
    let entry = make_entry(
        "find",
        serde_json::json!({"finding": "Path is wrong", "evidence": "log shows errors"}),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_valid_decide() {
    let entry = make_entry(
        "decide",
        serde_json::json!({"decision": "Add breaker", "alternatives": ["alert"], "reasoning": "Need prevention"}),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_valid_act() {
    let entry = make_entry(
        "act",
        serde_json::json!({"action": "Wrote function", "result": "Test passes"}),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_valid_verify() {
    let entry = make_entry(
        "verify",
        serde_json::json!({"check": "infra test", "evidence": "35/35", "status": "unconfirmed"}),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_reject_find_missing_evidence() {
    let entry = make_entry("find", serde_json::json!({"finding": "Something wrong"}));
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("evidence"));
}

#[test]
fn test_reject_decide_missing_reasoning() {
    let entry = make_entry(
        "decide",
        serde_json::json!({"decision": "Do something", "alternatives": []}),
    );
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("reasoning"));
}

#[test]
fn test_reject_unknown_type() {
    let entry = make_entry("hallucinate", serde_json::json!({"stuff": "things"}));
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown"));
}

#[test]
fn test_reject_empty_required_field() {
    let entry = make_entry(
        "find",
        serde_json::json!({"finding": "", "evidence": "some evidence"}),
    );
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("finding"));
}
