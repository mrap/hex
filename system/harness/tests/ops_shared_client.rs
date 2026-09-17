//! Integration tests for the process-wide shared iii client (KTD8): once
//! `hex harness serve` installs its long-lived client, `ops::call_builtin`
//! must reuse it instead of opening — and shutting down — one client per
//! call. See `docs/plans/2026-09-17-1412-fix-known-open-harness-failures-plan.md`,
//! unit U6.
//!
//! Own integration binary, not `tests/fd_limits.rs`: `ops`'s eventual shared
//! client is a process-wide `OnceLock`, so once one test installs a client,
//! every later test in that SAME PROCESS sees a client already installed.
//! Sharing a binary with `fd_limits.rs`'s per-call-path tests would make one
//! file's outcome depend on the other file's run order.
//!
//! Design decision — the sanctioned runner for this file is `cargo nextest`
//! (Verification Contract), which gives every test its own process, so the
//! `OnceLock` is naturally per-test there and none of the tests below depend
//! on run order under nextest. `install_via(url)` (builds a client on its own
//! url, hands it to `ops::install_shared_client`) exists anyway so this file
//! is not silently order-dependent under a plain `cargo test` run, where all
//! tests in a binary share one process and one `OnceLock`: there, only the
//! first call across the binary actually wins the slot, and every test here
//! is written to tolerate that (tests 1 and 2 assert the per-call path's fd,
//! telemetry, and per-call-client footprint — not "calls are routed through
//! THIS test's specific url" — so whichever client wins the slot does not
//! change their verdict).
#![cfg(unix)]

use std::net::TcpListener;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use hex::ops::{
    call_builtin_with_timeout_and_budget, install_shared_client, shared_client_installed,
    state_payload,
};
use hex::worker::runtime::connect_engine_client;

/// Serializes every test in this binary: they touch the process-wide shared
/// client slot, `III_URL`, `HEX_DIR`, and the telemetry store it points at.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Open-fd count of this process (`/dev/fd` on macOS and Linux). Copied from
/// `tests/fd_limits.rs` (not shared — see module doc for why).
fn open_fd_count() -> usize {
    std::fs::read_dir("/dev/fd").expect("read /dev/fd").count()
}

/// Set an env var for the guard's lifetime, restoring the previous value on
/// drop. Copied from `tests/fd_limits.rs`.
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

/// Engine address plus an isolated telemetry store for one test. Copied from
/// `tests/fd_limits.rs`: telemetry-asserting tests must isolate `HEX_DIR`
/// into a tempdir because other tests (in other binaries, same lane run)
/// share the sandboxed `HEX_DIR` the workspace `.cargo/config.toml` sets.
fn engine_env(url: String) -> (EnvVar, EnvVar, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hex_dir = EnvVar::set("HEX_DIR", tmp.path());
    let iii_url = EnvVar::set("III_URL", url);
    (hex_dir, iii_url, tmp)
}

/// A loopback port with nothing listening: connects are refused at once.
/// Copied from `tests/fd_limits.rs`.
fn refused_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr").port()
}

/// Install a client on `url` into the process-wide shared slot (see module
/// doc: only the first caller across the whole binary actually wins it).
fn install_via(url: &str) {
    let client = iii_sdk::register_worker(url, iii_sdk::InitOptions::default());
    install_shared_client(client);
}

/// Poll `open_fd_count()` until two reads 100ms apart agree, or `deadline`
/// passes. A single instantaneous read is not stable once a long-lived
/// client's reconnect loop is running: it opens and closes one socket every
/// 2s (SDK behavior, documented on `ops::call_builtin_with_timeout_and_budget`).
fn stable_fd_count(deadline: Instant) -> usize {
    let mut last = open_fd_count();
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let next = open_fd_count();
        if next == last || Instant::now() >= deadline {
            return next;
        }
        last = next;
    }
}

/// Accepts every connection on `listener` and holds it open without ever
/// reading a byte or answering the WebSocket upgrade — the incident shape,
/// generalized from `call_returns_within_budget_when_handshake_stalls` in
/// `fd_limits.rs` (which relies on the kernel backlog alone) to explicitly
/// hold MULTIPLE stalled connections open, since this file drives 3 calls
/// against the same listener plus a separate long-lived "installed" client
/// also dialing it.
fn stall_forever(listener: TcpListener) {
    std::thread::spawn(move || {
        // Held in this thread's own `Vec`, never read from: the connections
        // stay open and unanswered for as long as the thread runs (it loops
        // forever accepting), and close cleanly if the process ever tears
        // this thread down — unlike `mem::forget`, which would leak the fds
        // into this same process's table permanently and inflate every later
        // baseline read in this binary.
        let mut held = Vec::new();
        for stream in listener.incoming() {
            match stream {
                Ok(s) => held.push(s),
                Err(_) => break,
            }
        }
    });
}

/// Regression target for KTD8: with a shared client installed and running in
/// the background against one refused port, and `III_URL` pointing at a
/// SECOND, different refused port, a `call_builtin` call must go through the
/// installed shared client, not open its own client from `III_URL`.
///
/// The discriminator is the error message, not timing: `call_builtin_with_
/// timeout_and_budget` formats `"{function_id} failed (url={url}): {e}"`
/// where `url` is read straight from `III_URL` on every per-call-path
/// invocation. So today the error names the `III_URL` port unconditionally;
/// once the shared path is wired in (this unit's remaining task), `III_URL`
/// is never read on that path and the error must not name it. `iii::shutdown`
/// only fires from `record_shutdown_failure` — a per-call shutdown that
/// OVERRAN its budget, not merely "a per-call shutdown ran" — and fd count
/// are kept as envelope guards, not the primary signal (a refused-port
/// shutdown already completes well inside its budget today, so neither one
/// discriminates before/after on its own here).
#[test]
fn shared_client_refused_port_calls_add_no_fds() {
    let _g = serial();
    let shared_port = refused_port();
    let per_call_port = refused_port();
    let _env = engine_env(format!("ws://127.0.0.1:{per_call_port}"));
    install_via(&format!("ws://127.0.0.1:{shared_port}"));

    let baseline = stable_fd_count(Instant::now() + Duration::from_secs(5));

    const CALLS: usize = 3;
    let mut last_err = String::new();
    for _ in 0..CALLS {
        last_err = call_builtin_with_timeout_and_budget(
            "state::get",
            state_payload("events", "nope", None),
            Some(200),
            Duration::from_secs(5),
        )
        .expect_err("unreachable engine must fail loud");
    }

    let per_call_marker = per_call_port.to_string();
    assert!(
        !last_err.contains(&per_call_marker),
        "call resolved its own client from III_URL (port {per_call_port}) instead of reusing \
         the shared client installed on port {shared_port}: {last_err}"
    );

    let rows = hex::telemetry::recent(50).expect("telemetry recent");
    assert!(
        !rows.iter().any(|r| r.event == "iii::shutdown"),
        "unexpected iii::shutdown row(s): a per-call shutdown overran its budget: {rows:?}"
    );

    // The shared client's own reconnect loop opens/closes one socket every
    // 2s, so allow +1 for one caught mid-connect; the calls themselves must
    // add nothing that survives the poll.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut after = open_fd_count();
    while after > baseline + 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        after = open_fd_count();
    }
    assert!(
        after <= baseline + 1,
        "fd count did not return to the post-install baseline: {baseline} before, {after} after {CALLS} calls"
    );
}

/// Regression target for KTD8, incident shape: with a shared client
/// installed against a listener that accepts TCP and never answers the
/// WebSocket handshake, `call_builtin` calls must still fail loud within the
/// trigger timeout, must not leak fds, and must not run a per-call
/// `shutdown()`. Fails today for both reasons: the per-call client's
/// `shutdown_within` blocks for its full budget against a stalled handshake
/// (recording an `iii::shutdown` row each time) and its socket is never
/// released within the test.
#[test]
fn shared_client_stalled_handshake_calls_add_no_fds() {
    let _g = serial();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    stall_forever(listener);
    let url = format!("ws://127.0.0.1:{port}");
    let _env = engine_env(url.clone());
    install_via(&url);

    let baseline = stable_fd_count(Instant::now() + Duration::from_secs(5));

    const CALLS: usize = 3;
    let budget = Duration::from_millis(500);
    for i in 0..CALLS {
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
            panic!("call {i} still blocked after {bound:?} (trigger timeout + shutdown budget + slack)")
        });
        assert!(r.is_err(), "stalled engine must fail loud, got {r:?}");
    }

    // Checked before the fd poll (which can run up to 10s) so both pieces of
    // red evidence show up even though the fd assertion is very likely to
    // fail first: each per-call `shutdown_within` blocks for its full budget
    // against a handshake that never completes, and `record_shutdown_failure`
    // fires — a per-call shutdown that OVERRAN its budget, not merely "a
    // per-call shutdown ran" — once per call.
    let rows = hex::telemetry::recent(50).expect("telemetry recent");
    assert!(
        !rows.iter().any(|r| r.event == "iii::shutdown"),
        "unexpected iii::shutdown row(s): a per-call shutdown overran its budget: {rows:?}"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut after = open_fd_count();
    while after > baseline + 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        after = open_fd_count();
    }
    assert!(
        after <= baseline + 1,
        "fd count did not return to the post-install baseline: {baseline} before, {after} after {CALLS} calls"
    );
}

/// Pins the install contract itself: the seam `serve` will call
/// (`connect_engine_client`) must be the one that installs the shared
/// client. Fails today: the stub registers a client but does not install it.
#[test]
fn connect_engine_client_installs_the_shared_client() {
    let _g = serial();
    let port = refused_port();
    let url = format!("ws://127.0.0.1:{port}");
    let _client = connect_engine_client(&url);
    assert!(
        shared_client_installed(),
        "connect_engine_client must install the shared client"
    );
}

/// A second install must not panic, and the flag must stay true (the first
/// installed client wins; see module doc). Fails today: the stub's
/// `shared_client_installed` is hard-coded `false`, regardless of any
/// install call. The stub install cannot yet print the production WARN line
/// either (there is nothing to compare a second install against); this test
/// only pins the panic-free / flag-stays-true contract, which a test process
/// can observe.
#[test]
fn second_install_is_a_loud_no_op() {
    let _g = serial();
    install_via(&format!("ws://127.0.0.1:{}", refused_port()));
    // Must not panic.
    install_via(&format!("ws://127.0.0.1:{}", refused_port()));
    assert!(
        shared_client_installed(),
        "shared_client_installed must stay true after a second install attempt"
    );
}
