//! Process-global resource tests for the harness: file-descriptor release
//! after each `ops::call_builtin`, and the `RLIMIT_NOFILE` raise at
//! `hex harness serve` startup.
//!
//! These live in their own integration binary (own process, own fd table)
//! because the `--lib` suite runs ~900 tests in parallel threads, several of
//! which hold sockets or child-process pipes; a process-wide `/dev/fd` count
//! taken there is not attributable to one test. Every test here takes
//! `SERIAL` first so the two resource probes never overlap each other either.
//!
//! Incident: 2026-09-17 EMFILE storm (`failures-storm-...Too-many-open-files`),
//! fixed in v0.53.2; hardened per code review run 20260917-110953-bcc19fd9.
#![cfg(unix)]

use std::net::TcpListener;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use hex::ops::{call_builtin_with_timeout_and_budget, state_payload};

/// Serializes every test in this binary: they read or change process-wide
/// state (fd table, rlimits, `III_URL`).
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Set an env var for the guard's lifetime, restoring the previous value on
/// drop (`III_URL` for the engine address, `HEX_DIR` so telemetry rows land
/// in a tempdir instead of the real instance store).
struct EnvVar {
    name: &'static str,
    prev: Option<String>,
}

impl EnvVar {
    fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = std::env::var(name).ok();
        std::env::set_var(name, value);
        EnvVar { name, prev }
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => std::env::set_var(self.name, v),
            None => std::env::remove_var(self.name),
        }
    }
}

/// Engine address plus an isolated telemetry store for one test.
fn engine_env(url: String) -> (EnvVar, EnvVar, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hex_dir = EnvVar::set("HEX_DIR", tmp.path());
    let iii_url = EnvVar::set("III_URL", url);
    (hex_dir, iii_url, tmp)
}

/// Regression for the review finding on the v0.53.2 fix: `shutdown()` joins
/// the SDK connection thread, and the SDK's `connect_async` has no handshake
/// timeout. A listener that accepts TCP (kernel backlog) but never answers
/// the HTTP upgrade must not wedge the caller: the call returns within the
/// trigger timeout plus the shutdown-join budget, loudly, and the stuck
/// thread is detached.
#[test]
fn call_returns_within_budget_when_handshake_stalls() {
    let _g = serial();
    // Bound but never accept: the kernel completes the TCP handshake, the
    // client sends its upgrade request, and then waits forever.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let _env = engine_env(format!("ws://127.0.0.1:{port}"));

    let budget = Duration::from_millis(500);
    // Run the call on its own thread so a regression to an unbounded join
    // fails this assertion instead of hanging the test binary.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = call_builtin_with_timeout_and_budget(
            "state::get",
            state_payload("events", "nope", None),
            Some(200),
            budget,
        );
        let _ = tx.send(r);
    });
    let bound = Duration::from_millis(200) + budget + Duration::from_secs(1);
    let r = rx.recv_timeout(bound).unwrap_or_else(|_| {
        panic!("call still blocked after {bound:?} (trigger timeout + shutdown budget + slack)")
    });
    assert!(r.is_err(), "stalled engine must fail loud, got {r:?}");
    // The listener stays open through the assertions so the handshake really
    // was stalled, not refused, for the whole call.
    drop(listener);
}
