use std::{fs, process::Command};
use tempfile::TempDir;
fn bin() -> String {
    std::env::var("CARGO_BIN_EXE_hex").expect("hex binary")
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
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let text = fs::read_to_string(&output).unwrap();
    assert!(text.contains("\"r1\""));
    assert!(text.contains("\"accepted\":1"));
    let paths = hex::workers::hex_modules::module_paths();
    assert!(paths
        .iter()
        .any(|(n, p)| n == "hex-usage-tracking" && p.ends_with("usage_tracking.worker.rs")));
    assert!(!paths
        .iter()
        .any(|(_, p)| p.contains("launchd") || p.contains("service")));
}
