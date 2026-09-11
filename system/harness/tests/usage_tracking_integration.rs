use std::{fs, process::Command};
use tempfile::TempDir;
fn bin() -> String {
    std::env::var("CARGO_BIN_EXE_hex").expect("hex binary")
}
fn record(id: &str) -> String {
    format!("{{\"type\":\"token_usage_record\",\"provider\":\"codex\",\"response_id\":\"{id}\",\"event_at\":\"2026-09-10T00:00:00Z\",\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1}}\n")
}
#[test]
fn discovery_imports_active_archive_and_rollout_while_reporting_missing_rollout() {
    let root = TempDir::new().unwrap();
    let codex = root.path().join("codex");
    let ledger = root.path().join("ledger.db");
    let active = codex.join("sessions/a.jsonl");
    let archive = codex.join("archived_sessions/b.jsonl");
    let rollout = root.path().join("rollout.jsonl");
    fs::create_dir_all(active.parent().unwrap()).unwrap();
    fs::create_dir_all(archive.parent().unwrap()).unwrap();
    fs::write(&active, record("active")).unwrap();
    fs::write(&archive, record("archive")).unwrap();
    fs::write(&rollout, record("rollout")).unwrap();
    let db = rusqlite::Connection::open(codex.join("state_5.sqlite")).unwrap();
    db.execute("CREATE TABLE threads(rollout_path TEXT)", [])
        .unwrap();
    db.execute(
        "INSERT INTO threads VALUES(?1)",
        [rollout.to_string_lossy().to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads VALUES(?1)",
        [rollout.to_string_lossy().to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads VALUES(?1)",
        [root
            .path()
            .join("missing.jsonl")
            .to_string_lossy()
            .to_string()],
    )
    .unwrap();
    drop(db);
    let status = Command::new(bin())
        .env("HEX_DIR", root.path())
        .args([
            "usage",
            "collect",
            "--codex-root",
            codex.to_str().unwrap(),
            "--ledger",
            ledger.to_str().unwrap(),
            "--max-records",
            "100",
        ])
        .status()
        .unwrap();
    assert!(!status.success());
    let rows = hex::usage_ledger::UsageLedger::open(&ledger)
        .unwrap()
        .rows(10, 0)
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|r| r.response_id == "active"));
    assert!(rows.iter().any(|r| r.response_id == "archive"));
    assert!(rows.iter().any(|r| r.response_id == "rollout"));
}
#[test]
fn collect_and_report_are_local_disposable_and_worker_is_harness_only() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("source.jsonl");
    let ledger = root.path().join("ledger.db");
    let output = root.path().join("report.json");
    fs::write(&source,"{\"type\":\"token_usage_record\",\"provider\":\"codex\",\"account_scope\":\"test\",\"response_id\":\"r1\",\"event_at\":\"2026-09-10T00:00:00Z\",\"model\":\"gpt-5.6-luna\",\"input_tokens\":5,\"cached_input_tokens\":1,\"output_tokens\":2}\n").unwrap();
    let status = Command::new(bin())
        .env("HEX_DIR", root.path())
        .args([
            "usage",
            "collect",
            "--source",
            source.to_str().unwrap(),
            "--ledger",
            ledger.to_str().unwrap(),
            "--max-records",
            "1000",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new(bin())
        .env("HEX_DIR", root.path())
        .args([
            "usage",
            "report",
            "--ledger",
            ledger.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--cutoff",
            "2026-09-11T00:00:00Z",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let text = fs::read_to_string(&output).unwrap();
    assert!(!text.contains("\"r1\""));
    assert!(text.contains("\"credits_micro\":\"81\""));
    assert!(text.contains("\"accepted\":1"));
    assert!(text.contains("\"by_model\""));
    assert!(text.contains("\"by_family\""));
    assert!(text.contains("\"child_coordination\""));
    let first = text.clone();
    let status = Command::new(bin())
        .env("HEX_DIR", root.path())
        .args([
            "usage",
            "report",
            "--ledger",
            ledger.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--cutoff",
            "2026-09-11T00:00:00Z",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read_to_string(&output).unwrap(), first);
    let status = Command::new(bin())
        .env("HEX_DIR", root.path())
        .args([
            "usage",
            "report",
            "--ledger",
            ledger.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--cutoff",
            "2026-09-11T00:00:00Z",
            "--detail-dimension",
            "model",
            "--detail-key",
            "gpt-5.6-luna",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(fs::read_to_string(&output).unwrap().contains("\"r1\""));
    let paths = hex::workers::hex_modules::module_paths();
    assert!(paths
        .iter()
        .any(|(n, p)| n == "hex-usage-tracking" && p.ends_with("usage_tracking.worker.rs")));
    assert!(!paths
        .iter()
        .any(|(_, p)| p.contains("launchd") || p.contains("service")));
}
