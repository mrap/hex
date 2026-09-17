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
use std::time::{Duration, Instant};

use hex::ops::{call_builtin_with_timeout, call_builtin_with_timeout_and_budget, state_payload};

#[path = "support/mod.rs"]
mod support;
use support::*;

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

/// Regression for the 2026-09-17 EMFILE storm: `call_builtin` used to leak
/// the SDK's `iii-connection` thread (plus its kqueue and the loopback
/// socket) on every call because nothing ever called `III::shutdown()`.
/// Inside `hex harness serve` that reached the 256-fd soft limit in ~100 min
/// and broke every worker that spawns `hex`. Fails 4 -> 18 fds over 3 calls
/// without the shutdown.
///
/// Drives the unreachable-engine path (a port with no listener) with a short
/// invocation timeout: each call must fail LOUD and must not hold on to file
/// descriptors once it returns.
#[test]
fn call_builtin_releases_fds_after_each_call_when_engine_unreachable() {
    let _g = serial();
    let _env = engine_env(format!("ws://127.0.0.1:{}", refused_port()));

    let before = open_fd_count();
    const CALLS: usize = 3;
    for _ in 0..CALLS {
        let r = call_builtin_with_timeout(
            "state::get",
            state_payload("events", "nope", None),
            Some(200),
        );
        assert!(r.is_err(), "unreachable engine must fail loud, got {r:?}");
    }

    // The connection thread exits within ~2s of shutdown() even while
    // connects keep failing; poll for the fds to come back.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut after = open_fd_count();
    while after > before + 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        after = open_fd_count();
    }

    // A leak is >= 5 fds per call (socket, kqueue, runtime wakers); allow a
    // small tolerance for the runtime's own bookkeeping.
    assert!(
        after <= before + 2,
        "fd leak: {before} open before, {after} after {CALLS} calls (>= {} would be a per-call leak)",
        before + CALLS * 5
    );
}

fn nofile_limits() -> libc::rlimit {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a valid, writable rlimit struct for the duration of the call.
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) }, 0);
    lim
}

/// Restores the soft `RLIMIT_NOFILE` on drop, including during a panic, so a
/// failing assertion never leaves the rest of this binary at 256.
struct NofileSoftGuard(libc::rlim_t);

impl Drop for NofileSoftGuard {
    fn drop(&mut self) {
        let mut lim = nofile_limits();
        lim.rlim_cur = self.0;
        // SAFETY: only rlim_cur changes and it never exceeds rlim_max.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } != 0 {
            eprintln!(
                "fd_limits: could not restore soft NOFILE to {}: {}",
                self.0,
                std::io::Error::last_os_error()
            );
        }
    }
}

fn set_nofile_soft(soft: libc::rlim_t) {
    let mut lim = nofile_limits();
    lim.rlim_cur = soft;
    // SAFETY: only rlim_cur changes and it never exceeds rlim_max.
    assert_eq!(
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) },
        0,
        "setrlimit soft={soft}: {}",
        std::io::Error::last_os_error()
    );
}

/// 2026-09-17 EMFILE storm: the harness ran under launchd's 256-fd soft
/// limit. This test starts from that exact state (soft lowered to 256 first,
/// so it is not vacuous on hosts whose ambient limit is already higher, e.g.
/// Linux CI at 1024), then proves `raise_nofile_soft_limit` lifts it to the
/// clamp target, is idempotent, and that more than 256 files open at once.
#[test]
fn raise_nofile_soft_limit_lifts_launchd_default_to_clamp() {
    let _g = serial();
    let _restore = NofileSoftGuard(nofile_limits().rlim_cur);
    set_nofile_soft(256);
    assert_eq!(
        nofile_limits().rlim_cur,
        256,
        "precondition: soft lowered to 256"
    );

    let (soft, hard) = hex::worker::runtime::raise_nofile_soft_limit().expect("raise rlimit");
    let expected = if cfg!(target_os = "macos") {
        std::cmp::min(hard, hex::worker::runtime::NOFILE_CLAMP_MACOS)
    } else {
        hard
    };
    assert_eq!(soft, expected, "soft must equal the clamp target");
    assert!(soft > 256, "soft limit still at launchd default: {soft}");
    let again = hex::worker::runtime::raise_nofile_soft_limit().expect("second call");
    assert_eq!(again, (soft, hard), "must be idempotent");

    // Proof by running code, not by reading the struct back: open more than
    // 256 fds at once.
    let files: Vec<_> = (0..300)
        .map(|_| std::fs::File::open("/dev/null").expect("open /dev/null"))
        .collect();
    assert_eq!(files.len(), 300);
    drop(files);
}
