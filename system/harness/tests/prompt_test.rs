use hex_agent::prompt;

#[test]
fn test_prompt_contains_charter_and_state() {
    let charter_text = "id: test\nrole: Test Agent\nobjective: Be useful";
    let state = hex_agent::state::initialize("test", 2.0);
    let p = prompt::build(charter_text, &state, "timer.tick.30m", "{}", None);
    assert!(
        p.contains("Test Agent"),
        "prompt should contain charter text"
    );
    assert!(
        p.contains("timer.tick.30m"),
        "prompt should contain trigger"
    );
    assert!(p.contains("active"), "prompt should reference active queue");
    assert!(
        p.contains("AgentResponse"),
        "prompt should describe response format"
    );
}

#[test]
fn test_prompt_includes_response_schema() {
    let state = hex_agent::state::initialize("test", 2.0);
    let p = prompt::build("id: test", &state, "manual", "{}", None);
    assert!(p.contains("trail"), "must describe trail");
    assert!(p.contains("queue_updates"), "must describe queue_updates");
    assert!(p.contains("active_drained"), "must describe active_drained");
    assert!(p.contains("observe"), "must list action types");
    assert!(p.contains("find"), "must list action types");
    assert!(p.contains("decide"), "must list action types");
}

#[test]
fn test_prompt_includes_principles_when_provided() {
    let state = hex_agent::state::initialize("test", 2.0);
    let p = prompt::build(
        "id: test",
        &state,
        "manual",
        "{}",
        Some("## Self-Tuning Cadence\nYou own the tempo."),
    );
    assert!(
        p.contains("Self-Tuning Cadence"),
        "principles should appear in prompt"
    );
    assert!(
        p.contains("You own the tempo"),
        "principles content should be injected"
    );
}

#[test]
fn test_prompt_works_without_principles() {
    let state = hex_agent::state::initialize("test", 2.0);
    let p = prompt::build("id: test", &state, "manual", "{}", None);
    assert!(!p.contains("Self-Tuning"), "no principles text when None");
}

#[test]
fn test_assessment_prompt_structure() {
    let charter = hex_agent::charter::load_from_str(
        "id: test\nname: Test Bot\nrole: Test role\nobjective: Be effective\nkpis:\n  - 'metric >= 5'\nwake:\n  triggers: []\n  responsibilities:\n    - name: task-a\n      interval: 3600\n      description: Do task A\nauthority:\n  green: []\n  yellow: []\n  red: []\nbudget:\n  wakes_per_hour: 10\n  usd_per_day: 1\n  usd_per_shift: 0.5\nkill_switch: /tmp/test-halt"
    ).unwrap();
    let state = hex_agent::state::initialize("test", 1.0);
    let p = prompt::build_assessment(&charter, &state, None);
    assert!(
        p.contains("Self-Assessment Phase"),
        "must have assessment header"
    );
    assert!(p.contains("Test Bot"), "must include agent name");
    assert!(p.contains("metric >= 5"), "must include KPIs");
    assert!(p.contains("task-a"), "must include responsibilities");
    assert!(
        p.contains("AssessmentResponse"),
        "must describe response format"
    );
    assert!(
        p.contains("cadence_overrides"),
        "must describe cadence override format"
    );
}

#[test]
fn test_assessment_prompt_shows_cadence_overrides() {
    let charter = hex_agent::charter::load_from_str(
        "id: test\nname: Test Bot\nrole: Test role\nwake:\n  triggers: []\n  responsibilities:\n    - name: task-a\n      interval: 3600\n      description: Do task A\nauthority:\n  green: []\n  yellow: []\n  red: []\nbudget:\n  wakes_per_hour: 10\n  usd_per_day: 1\n  usd_per_shift: 0.5\nkill_switch: /tmp/test-halt"
    ).unwrap();
    let mut state = hex_agent::state::initialize("test", 1.0);
    state.cadence_overrides.insert("task-a".to_string(), 7200);
    let p = prompt::build_assessment(&charter, &state, None);
    assert!(p.contains("7200s"), "must show overridden interval");
    assert!(
        p.contains("overridden from charter default 3600s"),
        "must note the override"
    );
}
