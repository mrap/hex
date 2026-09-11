use hex::usage_ledger::{ImportOptions, UsageLedger};
use std::fs;
use tempfile::TempDir;
fn line(id: &str, parent: Option<&str>, output: i64) -> String {
    format!(
        r#"{{"type":"token_usage_record","provider":"codex","account_scope":"local","response_id":"{id}","parent_response_id":{},"root_task_family":"build","event_at":"2026-09-10T12:00:00Z","model":"gpt-5","input_tokens":10,"cached_input_tokens":2,"output_tokens":{output}}}"#,
        parent.map(|x| format!("\"{x}\"")).unwrap_or("null".into())
    )
}
fn setup() -> (TempDir, UsageLedger, std::path::PathBuf) {
    let d = TempDir::new().unwrap();
    let p = d.path().join("history.jsonl");
    let l = UsageLedger::open(d.path().join("usage.db")).unwrap();
    (d, l, p)
}
#[test]
fn replay_root_child_and_bounded_batches() {
    let (_d, mut l, p) = setup();
    fs::write(
        &p,
        format!(
            "{}\n{}\n",
            line("root", None, 3),
            line("child", Some("root"), 4)
        ),
    )
    .unwrap();
    assert_eq!(
        l.import_jsonl(
            &p,
            ImportOptions {
                max_records: 1,
                ..Default::default()
            }
        )
        .unwrap()
        .accepted,
        1
    );
    assert_eq!(
        l.import_jsonl(
            &p,
            ImportOptions {
                max_records: 1,
                ..Default::default()
            }
        )
        .unwrap()
        .accepted,
        1
    );
    let rows = l.rows(10, 0).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.response_id == "child")
            .unwrap()
            .parent_response_id
            .as_deref(),
        Some("root")
    );
    assert_eq!(l.import_jsonl(&p, Default::default()).unwrap().accepted, 0);
    assert_eq!(l.coverage().unwrap().accepted, 2)
}
#[test]
fn interruption_is_atomic() {
    let (_d, mut l, p) = setup();
    fs::write(&p, format!("{}\n", line("a", None, 1))).unwrap();
    assert!(l
        .import_jsonl(
            &p,
            ImportOptions {
                abort_before_commit: true,
                ..Default::default()
            }
        )
        .is_err());
    assert_eq!(l.rows(10, 0).unwrap().len(), 0);
    assert_eq!(l.import_jsonl(&p, Default::default()).unwrap().accepted, 1)
}
#[test]
fn rename_truncate_late_and_partial_recover() {
    let (d, mut l, p) = setup();
    fs::write(&p, format!("{}\n", line("a", None, 1))).unwrap();
    l.import_jsonl(&p, Default::default()).unwrap();
    let moved = d.path().join("archived.jsonl");
    fs::rename(&p, &moved).unwrap();
    assert_eq!(
        l.import_jsonl(&moved, Default::default()).unwrap().accepted,
        0
    );
    fs::write(
        &moved,
        format!("{}\n{}", line("late", None, 2), line("partial", None, 3)),
    )
    .unwrap();
    let r = l.import_jsonl(&moved, Default::default()).unwrap();
    assert_eq!(r.accepted, 1);
    assert!(r.pending_partial);
    fs::write(
        &moved,
        format!("{}\n{}\n", line("late", None, 2), line("partial", None, 3)),
    )
    .unwrap();
    assert_eq!(
        l.import_jsonl(&moved, Default::default()).unwrap().accepted,
        1
    )
}
#[test]
fn duplicate_conflict_and_bad_records_are_visible() {
    let (_d, mut l, p) = setup();
    fs::write(
        &p,
        format!(
            "{}\n{}\n{}\n{{bad\n{{\"type\":\"token_usage_record\",\"provider\":\"codex\"}}\n",
            line("same", None, 1),
            line("same", None, 1),
            line("same", None, 9)
        ),
    )
    .unwrap();
    let r = l.import_jsonl(&p, Default::default()).unwrap();
    assert_eq!(
        (r.accepted, r.duplicates, r.conflicts, r.quarantined),
        (1, 1, 1, 2)
    );
    assert!(l.rows(10, 0).unwrap().is_empty());
    let c = l.coverage().unwrap();
    assert_eq!((c.conflicts, c.quarantined), (1, 2))
}

#[test]
fn unknown_accounts_are_source_namespaced_and_no_change_reads_no_lines() {
    let (d, mut ledger, first) = setup();
    let second = d.path().join("second.jsonl");
    let record = line("same-id", None, 1).replace("\"account_scope\":\"local\",", "");
    fs::write(&first, format!("{record}\n")).unwrap();
    fs::write(&second, format!("{record}\n")).unwrap();
    ledger.import_jsonl(&first, Default::default()).unwrap();
    ledger.import_jsonl(&second, Default::default()).unwrap();
    assert_eq!(ledger.rows(10, 0).unwrap().len(), 2);
    assert_eq!(
        ledger
            .import_jsonl(&first, Default::default())
            .unwrap()
            .bytes_read,
        0
    );
}

#[test]
fn one_large_invocation_drains_multiple_pretransaction_chunks() {
    let (_d, mut ledger, path) = setup();
    let mut fixture = String::new();
    for index in 0..1_025 {
        fixture.push_str(&line(&format!("chunk-{index}"), None, 1));
        fixture.push('\n');
    }
    fs::write(&path, fixture).unwrap();
    let result = ledger
        .import_jsonl(
            &path,
            ImportOptions {
                max_records: 2_000,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.accepted, 1_025);
    assert!(!result.backlog);
    assert_eq!(ledger.rows(2_000, 0).unwrap().len(), 1_025);
}

#[test]
fn codex_session_token_snapshots_are_noncanonical_stream_state() {
    let (_d, mut ledger, path) = setup();
    let snapshot = |time: &str, input: i64, cached: i64, output: i64, total: i64| {
        format!(
            r#"{{"type":"event_msg","timestamp":"{time}","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output},"total_tokens":{total}}},"total_token_usage":{{"input_tokens":999,"output_tokens":999}}}}}}}}"#
        )
    };
    fs::write(
        &path,
        format!(
            "{}\n{}\n{}\n",
            r#"{"type":"session_meta","timestamp":"2026-09-10T00:00:00Z","payload":{"id":"session-a"}}"#,
            snapshot("2026-09-10T00:00:00Z", 10, 2, 3, 13),
            snapshot("2026-09-10T00:01:00Z", 4, 1, 2, 19)
        ),
    )
    .unwrap();
    let result = ledger.import_jsonl(&path, Default::default()).unwrap();
    assert_eq!((result.accepted, result.noncanonical), (0, 2));
    assert!(ledger.rows(10, 0).unwrap().is_empty());
    assert_eq!(ledger.coverage().unwrap().noncanonical, 2);
}

#[test]
fn bounded_codex_state_survives_reopen_with_context_and_cumulative_totals() {
    let (dir, mut ledger, path) = setup();
    let db = dir.path().join("usage.db");
    let snapshot = |time: &str, input: i64, cached: i64, output: i64| {
        format!(
            r#"{{"type":"event_msg","timestamp":"{time}","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output}}},"last_token_usage":{{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}}}}}"#
        )
    };
    fs::write(&path, format!(
        "{}\n{}\n{}\n{}\n",
        r#"{"type":"session_meta","timestamp":"2026-09-10T00:00:00Z","payload":{"id":"session-a","source":{"subagent":{"thread_spawn":{"parent_thread_id":"root-thread"}}}}}"#,
        r#"{"type":"turn_context","timestamp":"2026-09-10T00:00:01Z","payload":{"model":"gpt-5.6-terra","effort":"medium","root_task_family":"build"}}"#,
        snapshot("2026-09-10T00:00:02Z", 10, 2, 3),
        snapshot("2026-09-10T00:00:03Z", 15, 3, 5),
    )).unwrap();
    assert_eq!(
        ledger
            .import_jsonl(
                &path,
                ImportOptions {
                    max_records: 3,
                    ..Default::default()
                }
            )
            .unwrap()
            .noncanonical,
        1
    );
    drop(ledger);
    let mut reopened = UsageLedger::open(db).unwrap();
    assert_eq!(
        reopened
            .import_jsonl(
                &path,
                ImportOptions {
                    max_records: 1,
                    ..Default::default()
                }
            )
            .unwrap()
            .noncanonical,
        1
    );
    assert!(reopened.rows(10, 0).unwrap().is_empty());
    assert_eq!(reopened.coverage().unwrap().noncanonical, 2);
}

#[test]
fn codex_extra_counters_are_reported_once_and_resets_use_last_usage() {
    let (_dir, mut ledger, path) = setup();
    let snapshot = |time: &str, total: &str, last: &str| {
        format!(
            r#"{{"type":"event_msg","timestamp":"{time}","payload":{{"type":"token_count","info":{{"total_token_usage":{total},"last_token_usage":{last}}}}}}}"#
        )
    };
    let first = r#"{"input_tokens":10,"cached_input_tokens":2,"cache_write_input_tokens":7,"output_tokens":3,"reasoning_output_tokens":2,"total_tokens":13}"#;
    let second = r#"{"input_tokens":15,"cached_input_tokens":3,"cache_write_input_tokens":8,"output_tokens":5,"reasoning_output_tokens":3,"total_tokens":20}"#;
    let reset = r#"{"input_tokens":4,"cached_input_tokens":1,"cache_write_input_tokens":1,"output_tokens":2,"reasoning_output_tokens":1,"total_tokens":6}"#;
    fs::write(&path, format!(
        "{}\n{}\n{}\n{}\n",
        r#"{"type":"session_meta","timestamp":"2026-09-10T00:00:00Z","payload":{"id":"session-extra"}}"#,
        snapshot("2026-09-10T00:00:01Z", first, first),
        snapshot("2026-09-10T00:00:02Z", second, r#"{"input_tokens":5,"cached_input_tokens":1,"cache_write_input_tokens":1,"output_tokens":2,"reasoning_output_tokens":1,"total_tokens":7}"#),
        snapshot("2026-09-10T00:00:03Z", reset, reset),
    )).unwrap();
    assert_eq!(
        ledger
            .import_jsonl(&path, Default::default())
            .unwrap()
            .noncanonical,
        3
    );
    assert!(ledger.rows(10, 0).unwrap().is_empty());
    assert_eq!(ledger.coverage().unwrap().noncanonical, 3);
}

#[test]
fn missing_extra_counters_remain_unknown() {
    let (_dir, mut ledger, path) = setup();
    fs::write(&path, format!("{}\n", line("no-extras", None, 3))).unwrap();
    ledger.import_jsonl(&path, Default::default()).unwrap();
    let row = ledger.rows(1, 0).unwrap().pop().unwrap();
    assert_eq!(row.cache_write_input_tokens, None);
    assert_eq!(row.reasoning_output_tokens, None);
    assert_eq!(row.total_tokens, None);
}

#[test]
fn codex_token_count_without_provider_defaults_to_codex() {
    let (_dir, mut ledger, path) = setup();
    fs::write(
        &path,
        concat!(
            r#"{"type":"session_meta","timestamp":"2026-09-10T00:00:00Z","payload":{"id":"providerless"}}"#,
            "\n",
            r#"{"type":"token_count","timestamp":"2026-09-10T00:00:01Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
            "\n"
        ),
    )
    .unwrap();
    let result = ledger.import_jsonl(&path, Default::default()).unwrap();
    assert_eq!(
        (result.accepted, result.noncanonical, result.quarantined),
        (0, 1, 0)
    );
    assert!(ledger.rows(1, 0).unwrap().is_empty());
}

#[test]
fn unrelated_events_advance_cursor_without_quarantine() {
    let (_dir, mut ledger, path) = setup();
    fs::write(
        &path,
        concat!(
            r#"{"type":"response_item","timestamp":"2026-09-10T00:00:00Z","payload":{"type":"function_call"}}"#,
            "\n",
            r#"{"type":"turn_context","timestamp":"2026-09-10T00:00:01Z","payload":{"model":"gpt-5.6-terra"}}"#,
            "\n"
        ),
    )
    .unwrap();
    let result = ledger.import_jsonl(&path, Default::default()).unwrap();
    assert_eq!((result.accepted, result.quarantined), (0, 0));
    assert_eq!(
        ledger
            .import_jsonl(&path, Default::default())
            .unwrap()
            .bytes_read,
        0
    );
}

#[test]
fn payload_backed_codex_response_uses_codex_identity_and_usage() {
    let (_dir, mut ledger, path) = setup();
    fs::write(
        &path,
        concat!(
            r#"{"ordinal":12,"timestamp":"2026-09-10T00:00:00Z","type":"token_usage_record","payload":{"response_id":"response-1","root_turn_id":"root-turn","session_id":"session-1","thread_id":"parent-thread","turn_id":"turn-1","usage":{"input_tokens":10,"cached_input_tokens":2,"cache_write_input_tokens":1,"output_tokens":3,"reasoning_output_tokens":2,"total_tokens":13}}}"#,
            "\n"
        ),
    )
    .unwrap();
    let result = ledger.import_jsonl(&path, Default::default()).unwrap();
    assert_eq!((result.accepted, result.quarantined), (1, 0));
    let row = ledger.rows(1, 0).unwrap().pop().unwrap();
    assert_eq!(row.provider, "codex");
    assert!(row.account_scope.starts_with("unknown-local-source:"));
    assert_eq!(row.response_id, "response-1");
    assert_eq!(row.parent_response_id.as_deref(), Some("parent-thread"));
    assert_eq!(row.root_task_family.as_deref(), Some("root-turn"));
    assert_eq!(row.event_at.as_deref(), Some("2026-09-10T00:00:00Z"));
    assert_eq!(row.cache_write_input_tokens, Some(1));
    assert_eq!(row.reasoning_output_tokens, Some(2));
    assert_eq!(row.total_tokens, Some(13));
}

#[test]
fn token_snapshot_and_later_response_do_not_double_count() {
    let (_dir, mut ledger, path) = setup();
    fs::write(&path, concat!(
        r#"{"type":"session_meta","timestamp":"2026-09-10T00:00:00Z","payload":{"id":"session-dedupe"}}"#, "\n",
        r#"{"type":"event_msg","timestamp":"2026-09-10T00:00:01Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#, "\n",
        r#"{"type":"token_usage_record","timestamp":"2026-09-10T00:00:02Z","payload":{"response_id":"resp_stable","session_id":"session-dedupe","usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"total_tokens":13}}}"#, "\n",
    )).unwrap();
    let result = ledger.import_jsonl(&path, Default::default()).unwrap();
    assert_eq!(
        (result.accepted, result.noncanonical, result.quarantined),
        (1, 1, 0)
    );
    let rows = ledger.rows(10, 0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].response_id, "resp_stable");
    assert_eq!(rows[0].total_tokens, Some(13));
}

#[test]
fn payload_response_hydrates_session_attribution_after_reopen() {
    let (dir, mut ledger, path) = setup();
    let db = dir.path().join("usage.db");
    fs::write(&path, concat!(
        r#"{"type":"session_meta","timestamp":"2026-09-10T00:00:00Z","payload":{"id":"session-attr","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}}}"#, "\n",
        r#"{"type":"turn_context","timestamp":"2026-09-10T00:00:01Z","payload":{"model":"gpt-5.6-terra","effort":"medium","root_task_family":"build"}}"#, "\n",
    )).unwrap();
    ledger.import_jsonl(&path, Default::default()).unwrap();
    drop(ledger);
    fs::write(&path, concat!(
        r#"{"type":"session_meta","timestamp":"2026-09-10T00:00:00Z","payload":{"id":"session-attr","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}}}"#, "\n",
        r#"{"type":"turn_context","timestamp":"2026-09-10T00:00:01Z","payload":{"model":"gpt-5.6-terra","effort":"medium","root_task_family":"build"}}"#, "\n",
        r#"{"type":"token_usage_record","timestamp":"2026-09-10T00:00:02Z","payload":{"response_id":"resp_attr","session_id":"session-attr","usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}"#, "\n",
    )).unwrap();
    let mut reopened = UsageLedger::open(db).unwrap();
    reopened.import_jsonl(&path, Default::default()).unwrap();
    let row = reopened.rows(1, 0).unwrap().pop().unwrap();
    assert_eq!(row.model.as_deref(), Some("gpt-5.6-terra"));
    assert_eq!(row.effort.as_deref(), Some("medium"));
    assert_eq!(row.root_task_family.as_deref(), Some("build"));
    assert_eq!(row.parent_response_id.as_deref(), Some("parent"));
}

#[test]
fn later_context_backfills_response_without_overwriting_explicit_model() {
    let (dir, mut ledger, path) = setup();
    let db = dir.path().join("usage.db");
    fs::write(&path, concat!(
        r#"{"type":"session_meta","timestamp":"2026-09-10T00:00:00Z","payload":{"id":"late"}}"#, "\n",
        r#"{"type":"token_usage_record","timestamp":"2026-09-10T00:00:01Z","payload":{"response_id":"resp-late","session_id":"late","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1}}}"#, "\n",
        r#"{"type":"token_usage_record","timestamp":"2026-09-10T00:00:02Z","payload":{"response_id":"resp-explicit","session_id":"late","model":"explicit","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1}}}"#, "\n",
        r#"{"type":"turn_context","timestamp":"2026-09-10T00:00:03Z","payload":{"model":"gpt-5.6-terra","effort":"medium"}}"#, "\n",
    )).unwrap();
    ledger
        .import_jsonl(
            &path,
            ImportOptions {
                max_records: 3,
                ..Default::default()
            },
        )
        .unwrap();
    drop(ledger);
    let mut reopened = UsageLedger::open(db).unwrap();
    reopened.import_jsonl(&path, Default::default()).unwrap();
    let rows = reopened.rows(10, 0).unwrap();
    assert_eq!(
        rows.iter()
            .find(|r| r.response_id == "resp-late")
            .unwrap()
            .model
            .as_deref(),
        Some("gpt-5.6-terra")
    );
    assert_eq!(
        rows.iter()
            .find(|r| r.response_id == "resp-late")
            .unwrap()
            .effort
            .as_deref(),
        Some("medium")
    );
    assert_eq!(
        rows.iter()
            .find(|r| r.response_id == "resp-explicit")
            .unwrap()
            .model
            .as_deref(),
        Some("explicit")
    );
}

#[test]
fn payload_root_session_does_not_replace_active_file_context() {
    let (_dir, mut ledger, path) = setup();
    fs::write(&path, concat!(
        r#"{"type":"session_meta","timestamp":"2026-09-10T00:00:00Z","payload":{"id":"file-session"}}"#, "\n",
        r#"{"type":"turn_context","timestamp":"2026-09-10T00:00:01Z","payload":{"model":"gpt-6-astra","effort":"high"}}"#, "\n",
        r#"{"type":"token_usage_record","timestamp":"2026-09-10T00:00:02Z","payload":{"response_id":"resp-root","session_id":"root-session","root_turn_id":"root-turn","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1}}}"#, "\n",
    )).unwrap();
    ledger.import_jsonl(&path, Default::default()).unwrap();
    let row = ledger.rows(1, 0).unwrap().pop().unwrap();
    assert_eq!(row.model.as_deref(), Some("gpt-6-astra"));
    assert_eq!(row.effort.as_deref(), Some("high"));
    assert_eq!(row.root_task_family.as_deref(), Some("root-turn"));
}
