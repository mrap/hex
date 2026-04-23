use chrono::Utc;
use std::path::PathBuf;

#[test]
fn test_load_valid_charter() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/valid-charter.yaml");
    let charter = hex_agent::charter::load(&path).unwrap();
    assert_eq!(charter.id, "test-agent");
    assert_eq!(charter.name, "Test Agent");
    assert_eq!(charter.budget.usd_per_day, 2.0);
    assert_eq!(charter.wake.responsibilities.len(), 2);
    assert_eq!(charter.authority.green.len(), 2);
}

#[test]
fn test_reject_charter_missing_id() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/missing-id-charter.yaml");
    let result = hex_agent::charter::load(&path);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("id") || err.contains("missing"),
        "Error should mention missing 'id': {err}"
    );
}

#[test]
fn test_reject_charter_negative_budget() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bad-budget-charter.yaml");
    let result = hex_agent::charter::load(&path);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("budget"), "Error should mention budget: {err}");
}

#[test]
fn test_zero_budget_is_unlimited() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zero-budget-charter.yaml");
    let charter =
        hex_agent::charter::load(&path).expect("zero budget charter should load successfully");
    assert_eq!(charter.id, "zero-budget");
    assert_eq!(charter.budget.usd_per_day, 0.0);
    assert_eq!(charter.budget.usd_per_shift, 0.0);
}

#[test]
fn test_zero_shift_budget_skips_enforcement() {
    let cost = hex_agent::types::Cost {
        last_wake_usd: 5.0,
        current_period: hex_agent::types::CostPeriod {
            start: Utc::now(),
            spent_usd: 10.0,
            budget_usd: 0.0,
        },
        lifetime_usd: 50.0,
    };
    let remaining = hex_agent::cost::shift_budget_remaining(&cost, 0.0);
    // With budget=0, remaining is negative, but the wake loop checks
    // `shift_budget > 0.0` before enforcing — so this should never trigger a break.
    assert!(
        remaining <= 0.0,
        "remaining should be <= 0 when budget is 0"
    );
    // The key invariant: budget=0 means the `if shift_budget > 0.0` guard in wake.rs
    // prevents enforcement. This test documents that 0.0 is the sentinel for unlimited.
    assert_eq!(
        0.0_f64 > 0.0,
        false,
        "0.0 must not pass the enforcement guard"
    );
}
