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
fn codex_session_token_snapshots_use_last_usage_not_cumulative_total() {
    let (_d, mut ledger, path) = setup();
    let snapshot = |time: &str, input: i64, cached: i64, output: i64, total: i64| {
        format!(
            r#"{{"type":"event_msg","timestamp":"{time}","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output},"total_tokens":{total}}},"total_token_usage":{{"input_tokens":999,"output_tokens":999}}}}}}}}"#
        )
    };
    fs::write(
        &path,
        format!(
            "{}\n{}\n",
            snapshot("2026-09-10T00:00:00Z", 10, 2, 3, 13),
            snapshot("2026-09-10T00:01:00Z", 4, 1, 2, 19)
        ),
    )
    .unwrap();
    assert_eq!(
        ledger
            .import_jsonl(&path, Default::default())
            .unwrap()
            .accepted,
        2
    );
    let rows = ledger.rows(10, 0).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.input_tokens.unwrap()).sum::<i64>(),
        14
    );
    assert_eq!(
        rows.iter().map(|r| r.output_tokens.unwrap()).sum::<i64>(),
        5
    );
}
