//! `Ctx` — the handle passed to a worker handler. Provides emit/state/run.
//!
//! `emit` delegates to `crate::ops::emit` in normal operation. During the
//! shutdown drain window the runtime hands handlers a Ctx whose `emit`
//! DIVERTS to the durable outbox instead of the engine — the at-most-once
//! shutdown-deferral rule (see hex-workers-as-rust-library decision). Because
//! a diverted emission was never delivered to the engine, replaying it on the
//! next init is a FIRST delivery → at-most-once preserved, no double-fire.

use anyhow::{anyhow, Error};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::outbox::{Emission, Outbox};

/// Runtime wiring a live Ctx carries so `emit` can divert during shutdown.
struct CtxRuntime {
    /// Flipped true once the harness begins draining. While true, `emit`
    /// diverts to the outbox instead of delivering to the engine.
    stopping: Arc<AtomicBool>,
    /// Durable on-disk outbox for shutdown-window emissions.
    outbox: Arc<Outbox>,
}

/// Handler-facing context. Created by the runtime per invocation.
///
/// A bare `Ctx::new()` (used in tests and non-runtime callers) always emits
/// to the engine. A `Ctx::with_runtime(...)` consults the shared `stopping`
/// flag and diverts to the outbox during the drain window.
pub struct Ctx {
    rt: Option<CtxRuntime>,
}

impl Ctx {
    /// Construct a plain Ctx with no shutdown-diversion wiring — `emit`
    /// always delivers to the engine.
    pub fn new() -> Self {
        Ctx { rt: None }
    }

    /// Construct a runtime-wired Ctx. `emit` delivers to the engine in normal
    /// operation, but diverts to `outbox` once `stopping` is set.
    pub fn with_runtime(stopping: Arc<AtomicBool>, outbox: Arc<Outbox>) -> Self {
        Ctx {
            rt: Some(CtxRuntime { stopping, outbox }),
        }
    }

    /// Emit an event. In normal operation this delegates to `ops::emit`, which
    /// writes the `{event,producer,ts,data}` envelope to iii state. During the
    /// shutdown drain window (runtime-wired Ctx with `stopping == true`) the
    /// emission is diverted to the durable outbox for replay on next init.
    pub fn emit(&self, event: &str, data: Value) -> Result<(), Error> {
        if let Some(rt) = &self.rt {
            if rt.stopping.load(Ordering::SeqCst) {
                return rt
                    .outbox
                    .append(&Emission {
                        event: event.to_string(),
                        data,
                    })
                    .map_err(|e| anyhow!("Ctx::emit: outbox append failed during drain: {e}"));
            }
        }
        crate::ops::emit(event, data, None).map_err(|e| anyhow!(e))
    }

    /// Handle for direct iii state access.
    pub fn state(&self) -> StateHandle {
        StateHandle
    }

    /// Run a shell command from within a handler.
    pub fn run(&self, argv: &[String]) -> Result<std::process::Output, Error> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| anyhow!("Ctx::run called with empty argv"))?;
        let out = std::process::Command::new(resolve_program(program))
            .args(args)
            .output()
            .map_err(|e| anyhow!("Ctx::run spawn failed for `{program}`: {e}"))?;
        // A non-zero exit is a FAILURE — surface it (with the command, exit code,
        // and a stderr/stdout tail) so the harness records WHY in telemetry. The
        // old behavior returned Ok regardless of exit code, which made every
        // failing cron log as `status=ok` with empty `detail` (regression after
        // the bake-in dropped iii_worker::run_command's exit-code check).
        if !out.status.success() {
            let code = out
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string());
            let tail = |bytes: &[u8]| {
                let s = String::from_utf8_lossy(bytes);
                let t = s.trim();
                head_tail(t, 600, 400)
            };
            let stderr_tail = tail(&out.stderr);
            let detail = if stderr_tail.is_empty() {
                tail(&out.stdout)
            } else {
                stderr_tail
            };
            return Err(anyhow!("`{program}` exited {code}: {detail}"));
        }
        Ok(out)
    }
}

impl Default for Ctx {
    fn default() -> Self {
        Self::new()
    }
}

/// First `head` + last `tail` chars with an ellipsis marker — error heads
/// carry file paths, tails carry exit reasons; keep both.
// `head_end`/`tail_start` are produced by `char_indices()` below, so both are
// valid char boundaries even when `flat` carries non-ASCII input.
#[allow(clippy::string_slice)]
fn head_tail(s: &str, head: usize, tail: usize) -> String {
    let flat = s.replace('\n', " ");
    if flat.len() <= head + tail {
        return flat;
    }
    // char-boundary-safe slicing
    let head_end = flat
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= head)
        .last()
        .unwrap_or(0);
    let tail_start = flat
        .char_indices()
        .map(|(i, _)| i)
        .find(|&i| i >= flat.len().saturating_sub(tail))
        .unwrap_or(flat.len());
    format!(
        "{} …[truncated]… {}",
        &flat[..head_end],
        &flat[tail_start..]
    )
}

/// Handle for direct iii state access from a worker handler. Stateless — each
/// call goes through the `ops` seam (the only iii caller). Scope/key are the
/// module's choice; values are arbitrary JSON.
pub struct StateHandle;

impl StateHandle {
    /// Read `scope/key`; `Ok(None)` if absent.
    pub fn get(&self, scope: &str, key: &str) -> Result<Option<Value>, Error> {
        crate::ops::state_get(scope, key).map_err(|e| anyhow!(e))
    }
    /// Write `value` at `scope/key`.
    pub fn set(&self, scope: &str, key: &str, value: Value) -> Result<(), Error> {
        crate::ops::state_set(scope, key, &value).map_err(|e| anyhow!(e))
    }
    /// Delete `scope/key`.
    pub fn delete(&self, scope: &str, key: &str) -> Result<(), Error> {
        crate::ops::state_delete(scope, key).map_err(|e| anyhow!(e))
    }
}

/// A bare `hex` resolves to the running harness binary, not to PATH. Workers are compiled
/// into `hex` itself, and the service PATH is rendered by whoever last ran
/// `hex harness start|ensure` (2026-09-15: a launchd-context re-render dropped `.hex/bin`
/// and six workers failed for 6.5 h). Any other program name passes through unchanged.
fn resolve_program(program: &str) -> std::ffi::OsString {
    if program == "hex" {
        if let Ok(exe) = std::env::current_exe() {
            return exe.into_os_string();
        }
    }
    std::ffi::OsString::from(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Error;
    use serde_json::json;
    use serde_json::Value;
    use tempfile::tempdir;

    /// A bare `hex` must resolve to the running binary, never to PATH (regression 2026-09-15:
    /// the service PATH lost `.hex/bin` and every `Ctx::run(["hex", ...])` failed to spawn).
    #[test]
    fn bare_hex_resolves_to_current_exe_not_path() {
        let resolved = resolve_program("hex");
        assert_eq!(resolved, std::env::current_exe().unwrap().into_os_string());
        assert_eq!(resolve_program("git"), std::ffi::OsString::from("git"));
    }

    /// StateHandle exposes get/set/delete returning anyhow::Result. This test
    /// only checks the API compiles and is shaped as expected; live behavior is
    /// covered by ops::state_roundtrip_live.
    #[test]
    fn state_handle_has_get_set_delete_api() {
        let ctx = Ctx::new();
        let _h = ctx.state();
        fn _assert_api() {
            let _: fn(&StateHandle, &str, &str) -> Result<Option<Value>, Error> = StateHandle::get;
            let _: fn(&StateHandle, &str, &str, Value) -> Result<(), Error> = StateHandle::set;
            let _: fn(&StateHandle, &str, &str) -> Result<(), Error> = StateHandle::delete;
        }
        _assert_api();
    }

    /// A runtime-wired Ctx, while stopping, must divert emit to the outbox
    /// (NOT attempt a live engine connection). Asserted by reading the outbox
    /// file back — the emission lands on disk.
    #[test]
    fn emit_diverts_to_outbox_while_stopping() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let outbox = Arc::new(Outbox::new(&path));
        let stopping = Arc::new(AtomicBool::new(true));
        let ctx = Ctx::with_runtime(stopping, outbox.clone());

        ctx.emit("landings.updated", json!({ "spec_id": "S1" }))
            .expect("diverted emit must succeed");

        // The emission must be durably on disk, replayable on next init.
        let mut replayed = Vec::new();
        let n = outbox
            .replay(|e| {
                replayed.push(e);
                Ok(())
            })
            .unwrap();
        assert_eq!(n, 1, "exactly one diverted emission must be queued");
        assert_eq!(replayed[0].event, "landings.updated");
        assert_eq!(replayed[0].data["spec_id"], "S1");
    }

    /// A command that exits non-zero must return Err (so the harness records a
    /// failure in telemetry), and the error must carry the stderr tail so the
    /// `detail` column says WHY — not a silent `ok`.
    #[test]
    fn run_errors_with_stderr_on_nonzero_exit() {
        let ctx = Ctx::new();
        let argv: Vec<String> = vec![
            "sh".into(),
            "-c".into(),
            "echo boom-from-stderr >&2; exit 7".into(),
        ];
        let err = ctx.run(&argv).expect_err("non-zero exit must be an Err");
        let msg = err.to_string();
        assert!(
            msg.contains("exited 7"),
            "error must carry exit code; got: {msg}"
        );
        assert!(
            msg.contains("boom-from-stderr"),
            "error must carry the stderr tail; got: {msg}"
        );
    }

    /// A successful command still returns Ok(Output).
    #[test]
    fn run_ok_on_success() {
        let ctx = Ctx::new();
        let out = ctx
            .run(&["sh".into(), "-c".into(), "exit 0".into()])
            .expect("exit 0 must be Ok");
        assert!(out.status.success());
    }

    #[test]
    fn head_tail_keeps_both_ends() {
        let s = format!("/path/to/the/error/file.txt: {}END", "x".repeat(2000));
        let out = head_tail(&s, 600, 400);
        assert!(out.starts_with("/path/to/the/error/file.txt:"));
        assert!(out.ends_with("END"));
        assert!(out.contains("…[truncated]…"));
    }

    #[test]
    fn head_tail_short_passthrough() {
        assert_eq!(head_tail("short", 600, 400), "short");
    }
}
