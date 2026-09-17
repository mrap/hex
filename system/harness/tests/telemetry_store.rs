// Red test for telemetry module — task Trbx52r52.
//
// Verifies that the telemetry store at system/harness/src/telemetry/mod.rs
// exposes record/recent/status/prune with the documented schema, and that
// events round-trip through a real SQLite file at $HEX_DIR/.hex/telemetry/events.db.
//
// This MUST fail until the module is created and wired into lib.rs.

use hex::telemetry::{self, TelemetryEvent};
use std::sync::Mutex;

// HEX_DIR is process-global; these tests run as parallel threads in one binary,
// so serialize every HEX_DIR mutation on a single lock or they stomp each other
// (one test's tempdir gets swapped out → wrong db opened → lost rows / I/O error).
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_hex_dir<F: FnOnce()>(f: F) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HEX_DIR", tmp.path());
    f();
}

#[test]
fn record_then_recent_roundtrip() {
    with_hex_dir(|| {
        let ev = TelemetryEvent {
            source: "test-worker".to_string(),
            event: "hex::test::roundtrip".to_string(),
            status: "ok".to_string(),
            duration_ms: Some(42),
            exit_code: Some(0),
            detail: Some("hello".to_string()),
        };
        telemetry::record(&ev).expect("record should succeed");

        let rows = telemetry::recent(10).expect("recent should succeed");
        assert_eq!(rows.len(), 1, "expected exactly one row");
        let row = &rows[0];
        assert_eq!(row.source, "test-worker");
        assert_eq!(row.event, "hex::test::roundtrip");
        assert_eq!(row.status, "ok");
        assert_eq!(row.duration_ms, Some(42));
        assert_eq!(row.exit_code, Some(0));
    });
}

#[test]
fn status_aggregates_ok_and_error_counts() {
    with_hex_dir(|| {
        for status in ["ok", "ok", "error"] {
            telemetry::record(&TelemetryEvent {
                source: "w".to_string(),
                event: "hex::agg::x".to_string(),
                status: status.to_string(),
                duration_ms: Some(1),
                exit_code: Some(0),
                detail: None,
            })
            .unwrap();
        }
        let status_rows = telemetry::status().expect("status should succeed");
        let row = status_rows
            .iter()
            .find(|r| r.event == "hex::agg::x")
            .expect("agg event present");
        assert_eq!(row.run_count, 3);
        assert_eq!(row.ok_count, 2);
        assert_eq!(row.error_count, 1);
    });
}

#[test]
fn prune_removes_old_rows_when_keep_days_zero() {
    with_hex_dir(|| {
        telemetry::record(&TelemetryEvent {
            source: "w".to_string(),
            event: "hex::prune::x".to_string(),
            status: "ok".to_string(),
            duration_ms: None,
            exit_code: None,
            detail: None,
        })
        .unwrap();
        let removed = telemetry::prune(0).expect("prune should succeed");
        assert!(
            removed >= 1,
            "expected at least one row pruned, got {removed}"
        );
        let rows = telemetry::recent(10).expect("recent after prune");
        assert!(
            rows.is_empty(),
            "expected empty after prune, got {} rows",
            rows.len()
        );
    });
}

// ---------------------------------------------------------------------------
// HEX_DIR sandbox for cargo-launched processes (incident 2026-09-17: a test
// that recorded telemetry without isolating HEX_DIR wrote a stray
// `harness/iii::shutdown` row into the live instance store because hex
// sessions export HEX_DIR). `.cargo/config.toml` forces HEX_DIR to a sandbox
// path for every process cargo or nextest launches; these tests prove the
// sandbox is active and that the built binary still honors its own HEX_DIR
// outside cargo (production telemetry is not redirected).
// ---------------------------------------------------------------------------

const SANDBOX_HEX_DIR: &str = "/tmp/hex-test-hex-dir";

#[test]
fn cargo_launched_test_sees_sandbox_hex_dir() {
    let hex_dir = std::env::var("HEX_DIR").unwrap_or_default();
    assert_eq!(
        hex_dir, SANDBOX_HEX_DIR,
        "HEX_DIR is not the sandbox: `.cargo/config.toml` [env] HEX_DIR (force = true) is missing or was weakened; a test that forgets to isolate HEX_DIR would write into a live store"
    );
}

#[test]
fn sandbox_hex_dir_is_a_working_telemetry_store() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Do not touch HEX_DIR here: the point is that the ambient sandbox value
    // is a real, writable store, not a dead path.
    assert_eq!(
        std::env::var("HEX_DIR").unwrap_or_default(),
        SANDBOX_HEX_DIR
    );
    telemetry::record(&TelemetryEvent {
        source: "test".to_string(),
        event: "hex::sandbox::probe".to_string(),
        status: "ok".to_string(),
        duration_ms: None,
        exit_code: None,
        detail: None,
    })
    .expect("record into the sandbox store");
    assert!(
        std::path::Path::new(SANDBOX_HEX_DIR)
            .join(".hex/telemetry/events.db")
            .exists(),
        "sandbox events.db was not created"
    );
}

#[test]
fn built_binary_outside_cargo_honors_explicit_hex_dir() {
    let live = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hex"))
        .env("HEX_DIR", live.path())
        .args([
            "telemetry",
            "record",
            "--source",
            "test",
            "--event",
            "hex::sandbox::binary",
            "--status",
            "ok",
        ])
        .output()
        .expect("spawn hex binary");
    assert!(
        out.status.success(),
        "hex telemetry record failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        live.path().join(".hex/telemetry/events.db").exists(),
        "the binary did not write to its explicit HEX_DIR"
    );
    assert!(
        !live.path().starts_with(SANDBOX_HEX_DIR),
        "tempdir unexpectedly inside the sandbox"
    );
}
