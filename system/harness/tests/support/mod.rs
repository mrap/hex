//! Shared test helpers for the harness's process-global integration test
//! binaries (`fd_limits.rs`, `ops_shared_client.rs`, `telemetry_store.rs`).
//! Included via `#[path = "support/mod.rs"] mod support;` in each binary
//! rather than declared as its own test target, so each binary compiles its
//! own private copy of this module — in particular, `SERIAL` stays a
//! per-binary static, not one shared across binaries.

use std::net::TcpListener;
use std::sync::{Mutex, MutexGuard};

/// Serializes every test in the binary that includes this module: they touch
/// process-wide state (fd table, rlimits, env vars like `III_URL`/`HEX_DIR`,
/// and, in `ops_shared_client.rs`, the shared-client slot) that two tests
/// must never probe or mutate concurrently in the same process.
pub static SERIAL: Mutex<()> = Mutex::new(());

pub fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Open-fd count of this process (`/dev/fd` on macOS and Linux).
pub fn open_fd_count() -> usize {
    std::fs::read_dir("/dev/fd").expect("read /dev/fd").count()
}

/// Set an env var for the guard's lifetime, restoring the previous value on
/// drop (`III_URL` for the engine address, `HEX_DIR` so telemetry rows land
/// in a tempdir instead of the real instance store).
pub struct EnvVar {
    name: &'static str,
    prev: Option<String>,
}

impl EnvVar {
    pub fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
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
pub fn engine_env(url: String) -> (EnvVar, EnvVar, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hex_dir = EnvVar::set("HEX_DIR", tmp.path());
    let iii_url = EnvVar::set("III_URL", url);
    (hex_dir, iii_url, tmp)
}

/// A loopback port with nothing listening: connects are refused at once.
pub fn refused_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr").port()
}
