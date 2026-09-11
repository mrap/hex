use hex::capability_exec::{execute_capability, ExecContext};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// Keep fixture writes and process creation in one case at a time. On Linux,
// another case can fork while a script is writable and retain that descriptor
// until exec, making an otherwise closed script fail to execute with ETXTBSY.
static CASE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Create a mock sandbox dir with a run-test.sh that exec's the given binary.
fn make_sandbox(dir: &TempDir) -> PathBuf {
    let sandbox = dir.path().join("containers");
    fs::create_dir_all(&sandbox).unwrap();
    let script = "#!/bin/sh\nexec \"$@\"\n";
    let script_path = sandbox.join("run-test.sh");
    fs::write(&script_path, script).unwrap();
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();
    sandbox
}

/// Create a script in registry_dir/bin/<fn_id> with the given body.
fn make_bin(registry_dir: &Path, fn_id: &str, body: &str) {
    let bin_dir = registry_dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let bin_path = bin_dir.join(fn_id);
    fs::write(&bin_path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut perms = fs::metadata(&bin_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin_path, perms).unwrap();
}

fn base_ctx() -> ExecContext {
    ExecContext {
        caller: "agent-caller".to_string(),
        created_by: "agent-creator".to_string(),
        wake_n: 1,
        timeout_secs: 5,
        output_cap_bytes: 1024,
        calls_per_wake_cap: 10,
    }
}

// ── Test: sandbox-not-bare-host ───────────────────────────────────────────────

#[test]
fn test_sandbox_not_bare_host_no_sandbox_dir() {
    let _case = CASE_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    fs::create_dir_all(&registry_dir).unwrap();

    // Sandbox dir does not contain run-test.sh → must fail
    let no_sandbox = dir.path().join("nonexistent_containers");
    let mut count = 0u32;

    let result = execute_capability(
        &registry_dir,
        "fn-test",
        &[],
        &base_ctx(),
        &no_sandbox,
        &mut count,
    );

    assert!(
        result.is_err(),
        "execution must fail when sandbox script is absent"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("sandbox") || err.contains("container") || err.contains("refuse"),
        "error must mention sandbox refusal, got: {err}"
    );
}

#[test]
fn test_sandbox_not_bare_host_empty_sandbox_dir() {
    let _case = CASE_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    fs::create_dir_all(&registry_dir).unwrap();

    // Sandbox dir exists but has no run-test.sh
    let empty_sandbox = dir.path().join("empty_containers");
    fs::create_dir_all(&empty_sandbox).unwrap();

    let mut count = 0u32;
    let result = execute_capability(
        &registry_dir,
        "fn-test",
        &[],
        &base_ctx(),
        &empty_sandbox,
        &mut count,
    );

    assert!(
        result.is_err(),
        "execution must fail when run-test.sh is absent from sandbox"
    );
}

// ── Test: timeout kill ────────────────────────────────────────────────────────

#[test]
fn test_timeout_kill_reports_timed_out() {
    let _case = CASE_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    let sandbox = make_sandbox(&dir);

    // Script that loops forever
    make_bin(&registry_dir, "fn-hang", "exec sleep 60");

    let ctx = ExecContext {
        timeout_secs: 1,
        ..base_ctx()
    };

    let mut count = 0u32;
    let start = std::time::Instant::now();
    let result = execute_capability(&registry_dir, "fn-hang", &[], &ctx, &sandbox, &mut count);
    let elapsed = start.elapsed();

    assert!(
        result.is_ok(),
        "execute_capability must return Ok (timed-out result), got: {:?}",
        result.err()
    );
    let exec_result = result.unwrap();
    assert!(
        exec_result.timed_out,
        "timed_out must be true for a script that exceeds timeout"
    );
    assert!(
        elapsed.as_secs() < 5,
        "timeout must fire quickly (took {}s)",
        elapsed.as_secs()
    );
}

#[test]
fn test_timeout_kill_records_minus_one_exit_code() {
    let _case = CASE_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    let sandbox = make_sandbox(&dir);

    make_bin(&registry_dir, "fn-hang2", "exec sleep 60");

    let ctx = ExecContext {
        timeout_secs: 1,
        ..base_ctx()
    };

    let mut count = 0u32;
    let result =
        execute_capability(&registry_dir, "fn-hang2", &[], &ctx, &sandbox, &mut count).unwrap();

    assert_eq!(
        result.exit_code, -1,
        "exit_code must be -1 for a killed/timed-out process"
    );
}

// ── Test: output truncation ───────────────────────────────────────────────────

#[test]
fn test_output_truncation_stdout() {
    let _case = CASE_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    let sandbox = make_sandbox(&dir);

    // Script outputs 2000 'x' characters
    make_bin(
        &registry_dir,
        "fn-bigout",
        "i=0; while [ $i -lt 2000 ]; do printf x; i=$((i+1)); done",
    );

    let ctx = ExecContext {
        output_cap_bytes: 50,
        ..base_ctx()
    };

    let mut count = 0u32;
    let result =
        execute_capability(&registry_dir, "fn-bigout", &[], &ctx, &sandbox, &mut count).unwrap();

    assert!(
        result.output_truncated,
        "output_truncated must be true when script exceeds cap"
    );
    assert!(
        result.stdout.len() <= 50,
        "stdout must be at most cap bytes, got {} bytes",
        result.stdout.len()
    );
}

#[test]
fn test_output_not_truncated_under_cap() {
    let _case = CASE_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    let sandbox = make_sandbox(&dir);

    make_bin(&registry_dir, "fn-smallout", "echo hello");

    let ctx = ExecContext {
        output_cap_bytes: 1024,
        ..base_ctx()
    };

    let mut count = 0u32;
    let result = execute_capability(
        &registry_dir,
        "fn-smallout",
        &[],
        &ctx,
        &sandbox,
        &mut count,
    )
    .unwrap();

    assert!(
        !result.output_truncated,
        "output_truncated must be false for small output"
    );
    assert!(
        result.stdout.contains("hello"),
        "stdout must contain script output"
    );
}

// ── Test: per-wake call-count cap ─────────────────────────────────────────────

#[test]
fn test_per_wake_cap_enforced() {
    let _case = CASE_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    let sandbox = make_sandbox(&dir);

    make_bin(&registry_dir, "fn-cap", "echo ok");

    let ctx = ExecContext {
        calls_per_wake_cap: 2,
        ..base_ctx()
    };

    let mut count = 0u32;

    // First two calls: succeed
    execute_capability(&registry_dir, "fn-cap", &[], &ctx, &sandbox, &mut count).unwrap();
    execute_capability(&registry_dir, "fn-cap", &[], &ctx, &sandbox, &mut count).unwrap();
    assert_eq!(count, 2, "call_count must be incremented");

    // Third call: must fail due to cap
    let result = execute_capability(&registry_dir, "fn-cap", &[], &ctx, &sandbox, &mut count);
    assert!(result.is_err(), "third call must be rejected (cap=2)");
    let err = result.unwrap_err();
    assert!(
        err.contains("cap") || err.contains("limit") || err.contains("exceeded"),
        "error must mention the cap, got: {err}"
    );
}

#[test]
fn test_per_wake_cap_allows_up_to_cap() {
    let _case = CASE_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    let sandbox = make_sandbox(&dir);

    make_bin(&registry_dir, "fn-cap2", "echo ok");

    let ctx = ExecContext {
        calls_per_wake_cap: 3,
        ..base_ctx()
    };

    let mut count = 0u32;
    for _ in 0..3 {
        execute_capability(&registry_dir, "fn-cap2", &[], &ctx, &sandbox, &mut count).unwrap();
    }
    assert_eq!(count, 3);

    let result = execute_capability(&registry_dir, "fn-cap2", &[], &ctx, &sandbox, &mut count);
    assert!(result.is_err(), "4th call must fail with cap=3");
}

// ── Test: calls.jsonl row shape ───────────────────────────────────────────────

#[test]
fn test_calls_jsonl_row_shape() {
    let _case = CASE_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    let sandbox = make_sandbox(&dir);

    make_bin(&registry_dir, "fn-rowtest", "echo done");

    let ctx = ExecContext {
        caller: "agent-caller-x".to_string(),
        created_by: "agent-creator-y".to_string(),
        wake_n: 42,
        timeout_secs: 5,
        output_cap_bytes: 1024,
        calls_per_wake_cap: 10,
    };

    let mut count = 0u32;
    execute_capability(&registry_dir, "fn-rowtest", &[], &ctx, &sandbox, &mut count).unwrap();

    let calls_path = registry_dir.join("calls.jsonl");
    assert!(
        calls_path.exists(),
        "calls.jsonl must exist after execution"
    );

    let content = fs::read_to_string(&calls_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert!(
        !lines.is_empty(),
        "calls.jsonl must have at least one entry"
    );

    // Parse the last entry
    let row: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();

    assert!(
        row["ts"].is_string() && !row["ts"].as_str().unwrap().is_empty(),
        "ts must be present"
    );
    assert_eq!(
        row["fn_id"].as_str(),
        Some("fn-rowtest"),
        "fn_id must match"
    );
    assert_eq!(
        row["caller"].as_str(),
        Some("agent-caller-x"),
        "caller must be harness-set value"
    );
    assert_eq!(
        row["created_by"].as_str(),
        Some("agent-creator-y"),
        "created_by must be harness-set value"
    );
    assert_eq!(row["wake_n"].as_u64(), Some(42), "wake_n must match");
    assert!(
        row["exit_code"].is_number(),
        "exit_code must be present and numeric"
    );
}

#[test]
fn test_calls_jsonl_exit_code_zero_on_success() {
    let _case = CASE_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    let sandbox = make_sandbox(&dir);

    make_bin(&registry_dir, "fn-exit0", "exit 0");

    let mut count = 0u32;
    execute_capability(
        &registry_dir,
        "fn-exit0",
        &[],
        &base_ctx(),
        &sandbox,
        &mut count,
    )
    .unwrap();

    let content = fs::read_to_string(registry_dir.join("calls.jsonl")).unwrap();
    let row: serde_json::Value = serde_json::from_str(content.lines().last().unwrap()).unwrap();
    assert_eq!(
        row["exit_code"].as_i64(),
        Some(0),
        "successful script must record exit_code=0"
    );
}
