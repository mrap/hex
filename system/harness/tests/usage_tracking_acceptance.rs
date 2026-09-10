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
