use std::{fs, io::Write, process::Command};
use tempfile::TempDir;
fn bin() -> String {
    std::env::var("CARGO_BIN_EXE_hex").expect("hex binary")
}
fn record(id: &str) -> String {
    format!("{{\"type\":\"token_usage_record\",\"provider\":\"codex\",\"response_id\":\"{id}\",\"event_at\":\"2026-09-10T00:00:00Z\",\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1}}\n")
}
#[test]
fn discovery_imports_active_archive_and_rollout_while_skipping_missing_rollout() {
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
    assert!(status.success());
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
    fs::write(&source,"{\"type\":\"token_usage_record\",\"provider\":\"codex\",\"account_scope\":\"test\",\"response_id\":\"r1\",\"event_at\":\"2026-09-10T00:00:00Z\",\"model\":\"gpt-5.6-luna\",\"input_tokens\":5,\"cached_input_tokens\":1,\"output_tokens\":2}\n{\"type\":\"session_meta\",\"timestamp\":\"2026-09-10T00:00:00Z\",\"payload\":{\"id\":\"snapshot\"}}\n{\"type\":\"token_count\",\"timestamp\":\"2026-09-10T01:00:00Z\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"input_tokens\":2,\"cached_input_tokens\":0,\"output_tokens\":1}}}}\n").unwrap();
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
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(!text.contains("\"r1\""));
    assert!(text.contains("\"credits_micro\":\"81\""));
    assert_eq!(body["schema"], "hex.usage-report.v2");
    assert_eq!(body["windows"]["current"]["start"], "2026-09-10T00:00:00+00:00");
    assert_eq!(body["windows"]["current"]["end"], "2026-09-11T00:00:00+00:00");
    assert_eq!(body["windows"]["preceding"]["end"], "2026-09-10T00:00:00+00:00");
    assert!(body["windows"]["current"]["measured"]["cache_write_input_tokens"].is_null());
    assert!(body["windows"]["current"]["measured"]["reasoning_output_tokens"].is_null());
    assert!(body["windows"]["current"]["measured"]["provider_total_tokens"].is_null());
    assert_eq!(body["coverage"]["noncanonical"], 1);
    assert_eq!(body["coverage"]["stale_sources"], 0);
    assert_eq!(body["coverage"]["source_backlog"], 0);
    assert_eq!(body["completeness"], "incomplete");
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
    let detail: serde_json::Value = serde_json::from_str(&fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(detail["contributor_detail"]["response_ids"], serde_json::json!(["r1"]));
    assert_eq!(detail["contributor_detail"]["total_matches"], 1);
    let paths = hex::workers::hex_modules::module_paths();
    assert!(paths
        .iter()
        .any(|(n, p)| n == "hex-usage-tracking" && p.ends_with("usage_tracking.worker.rs")));
    assert!(!paths
        .iter()
        .any(|(_, p)| p.contains("launchd") || p.contains("service")));
}

#[test]
fn report_streams_more_than_the_old_cap_and_keeps_half_open_boundaries() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("source.jsonl");
    let ledger = root.path().join("ledger.db");
    let output = root.path().join("report.json");
    let mut file = fs::File::create(&source).unwrap();
    for id in 0..100_001 {
        writeln!(file, "{{\"type\":\"token_usage_record\",\"provider\":\"codex\",\"response_id\":\"current-{id}\",\"event_at\":\"2026-09-10T12:00:00Z\",\"model\":\"gpt-5.6-luna\",\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1}}").unwrap();
    }
    for (id, event_at) in [("preceding", "2026-09-09T00:00:00Z"), ("end", "2026-09-11T00:00:00Z")] {
        writeln!(file, "{{\"type\":\"token_usage_record\",\"provider\":\"codex\",\"response_id\":\"{id}\",\"event_at\":\"{event_at}\",\"model\":\"gpt-5.6-luna\",\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1}}").unwrap();
    }
    let collect = Command::new(bin()).env("HEX_DIR", root.path()).args(["usage", "collect", "--source", source.to_str().unwrap(), "--ledger", ledger.to_str().unwrap(), "--max-records", "100010"]).status().unwrap();
    assert!(collect.success());
    let report = Command::new(bin()).env("HEX_DIR", root.path()).args(["usage", "report", "--ledger", ledger.to_str().unwrap(), "--output", output.to_str().unwrap(), "--cutoff", "2026-09-11T16:30:00Z"]).status().unwrap();
    assert!(report.success());
    let body: serde_json::Value = serde_json::from_str(&fs::read_to_string(output).unwrap()).unwrap();
    assert_eq!(body["windows"]["current"]["measured"]["responses"], 100_001);
    assert_eq!(body["windows"]["preceding"]["measured"]["responses"], 1);
    assert_eq!(body["windows"]["change_tokens"], "200000");
}
