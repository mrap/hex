use chrono::{DateTime, Utc};
use hex::usage_ledger::{Coverage, UsageRow};
use hex::usage_reporting::{report, EstimateUnit, MicroUnits, AUDITED_RATE_VERSION};

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}
fn row(
    id: &str,
    at: &str,
    model: Option<&str>,
    family: Option<&str>,
    parent: Option<&str>,
    input: Option<i64>,
    cached: Option<i64>,
    output: Option<i64>,
) -> UsageRow {
    UsageRow {
        provider: "codex".into(),
        account_scope: "local".into(),
        response_id: id.into(),
        parent_response_id: parent.map(str::to_owned),
        root_task_family: family.map(str::to_owned),
        event_at: Some(at.into()),
        model: model.map(str::to_owned),
        effort: None,
        input_tokens: input,
        cached_input_tokens: cached,
        output_tokens: output,
    }
}
#[test]
fn exact_fixed_point_credit_math_and_rate_provenance() {
    let rows = vec![row(
        "a",
        "2026-09-10T01:00:00Z",
        Some("gpt-5.6-sol"),
        Some("build"),
        None,
        Some(1_000_001),
        Some(1),
        Some(2),
    )];
    let r = report(
        &rows,
        Coverage::default(),
        t("2026-09-10T00:00:00Z"),
        t("2026-09-11T00:00:00Z"),
    );
    // 1,000,000 fresh * 100 + cached * 10 + output * 500 microcredits.
    assert_eq!(r.modeled_credits.value, MicroUnits(100_001_010));
    assert_eq!(r.modeled_credits.rate_version, AUDITED_RATE_VERSION);
    assert_eq!(r.modeled_credits.unit, EstimateUnit::CreditEquivalent);
    assert!(r
        .modeled_credits
        .labels
        .contains("speed_unavailable_standard_rate_only"));
    assert!(r.modeled_api_usd.incomplete);
    assert!(r
        .modeled_api_usd
        .labels
        .contains("api_usd_rate_not_defined_by_audited_facts"));
    assert!(r
        .actual_billed_debited
        .labels
        .contains("actual_billed_or_debited_unavailable"));
}
#[test]
fn report_answer_key_is_stable_and_does_not_double_count_children() {
    let rows = vec![
        row(
            "root",
            "2026-09-10T03:00:00Z",
            Some("gpt-5.6-terra"),
            Some("alpha"),
            None,
            Some(100),
            Some(20),
            Some(10),
        ),
        row(
            "child-b",
            "2026-09-10T04:00:00Z",
            Some("gpt-6-astra"),
            Some("alpha"),
            Some("root"),
            Some(50),
            Some(0),
            Some(10),
        ),
        row(
            "child-a",
            "2026-09-10T05:00:00Z",
            Some("gpt-6-astra"),
            Some("alpha"),
            Some("root"),
            Some(50),
            Some(0),
            Some(10),
        ),
        row(
            "prior",
            "2026-09-09T03:00:00Z",
            Some("gpt-5.6-luna"),
            Some("beta"),
            None,
            Some(10),
            Some(0),
            Some(1),
        ),
    ];
    let r = report(
        &rows,
        Coverage::default(),
        t("2026-09-10T00:00:00Z"),
        t("2026-09-11T00:00:00Z"),
    );
    assert_eq!(r.measured.total(), 230);
    assert_eq!(r.by_family[0].key, "alpha");
    assert_eq!(r.by_model[0].key, "gpt-6-astra");
    assert_eq!(r.child_coordination.measured.total(), 120);
    assert_eq!(r.child_coordination_share_millionths, Some(521_739));
    assert_eq!(
        r.child_coordination.response_ids,
        vec!["child-a", "child-b"]
    );
    assert_eq!(r.preceding_measured.total(), 11);
    assert_eq!(r.preceding_change_tokens, Some(219));
}
#[test]
fn ties_sort_by_key_and_unknowns_are_visible() {
    let rows = vec![
        row(
            "z",
            "2026-09-10T01:00:00Z",
            Some("gpt-5.6-luna"),
            Some("zeta"),
            None,
            Some(10),
            Some(0),
            Some(0),
        ),
        row(
            "a",
            "2026-09-10T02:00:00Z",
            Some("gpt-5.6-terra"),
            Some("alpha"),
            None,
            Some(10),
            Some(0),
            Some(0),
        ),
        row(
            "unknown",
            "2026-09-10T03:00:00Z",
            None,
            None,
            None,
            Some(10),
            Some(0),
            Some(0),
        ),
        row(
            "bad",
            "2026-09-10T04:00:00Z",
            Some("gpt-5.6-sol"),
            Some("alpha"),
            None,
            None,
            None,
            None,
        ),
    ];
    let r = report(
        &rows,
        Coverage {
            conflicts: 1,
            quarantined: 1,
            pending_sources: 1,
            stale_sources: 1,
            ..Coverage::default()
        },
        t("2026-09-10T00:00:00Z"),
        t("2026-09-11T00:00:00Z"),
    );
    assert_eq!(r.by_family[0].key, "alpha");
    assert_eq!(r.by_family[1].key, "unknown");
    assert_eq!(r.by_model[0].key, "gpt-5.6-luna");
    assert!(r.modeled_credits.labels.contains("unknown_model_rate"));
    for label in [
        "missing_or_invalid_usage",
        "unknown_model",
        "conflicting_usage_records",
        "quarantined_records",
        "source_backlog",
        "stale_source",
    ] {
        assert!(r.incomplete_labels.contains(label), "{label}");
    }
}
