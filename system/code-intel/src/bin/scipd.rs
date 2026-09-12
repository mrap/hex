//! `scipd` — the live code-intel daemon (SPEC-A2 §2).
//!
//! Launchd-supervised (com.hex.scipd, KeepAlive). Owns the capped pool of
//! live rust-analyzer instances and serves cq over `<home>/scipd.sock`.
//! Wires together (SPEC-A2 §2/§4):
//! - the UDS accept loop ([`Daemon`]) dispatching into the live pool,
//! - a reaper thread sweeping the pool every 30s (idle TTL, vanish reap,
//!   memory watchdog),
//! - clean SIGTERM/SIGINT shutdown: drain the accept loop, then
//!   `pool.shutdown_all()` — no orphaned rust-analyzer children, ever.

use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use scipd_core::config::ScipdConfig;
use scipd_core::daemon::{BindError, Daemon, SWEEP_INTERVAL};
use scipd_core::live::{LiveInstance, Pool};
use scipd_core::workspace::codeintel_home;

fn fail(code: &str, message: String, hint: &str) -> ExitCode {
    // Loud structured stderr, same code/message/hint triple as cq errors.
    eprintln!(
        "{}",
        serde_json::json!({"error": {"code": code, "message": message, "hint": hint}})
    );
    ExitCode::FAILURE
}

/// An early, before-startup flag that short-circuits normal daemon startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EarlyFlag {
    Version,
    Help,
}

/// Pure: does `args` (argv, excluding argv0) request an early exit? Checked
/// at the very top of `main`, before `codeintel_home()` or `Daemon::bind`,
/// so `--version`/`--help` work even when the daemon config/home can't be
/// resolved — and so `scipd --version` never falls through to daemon
/// startup and hits `BindError::AlreadyRunning` (exit 1) when a daemon is
/// already serving. The installer's post-install self-check runs `<cli>
/// --version` and rolled back the scipd install on exactly that exit-1.
/// No clap parser here (unlike `cq`, which gets `--version` for free via
/// `#[command(version)]`) — scipd takes no other CLI flags, so a manual
/// scan is the whole story.
fn early_flag(args: &[String]) -> Option<EarlyFlag> {
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Some(EarlyFlag::Version);
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Some(EarlyFlag::Help);
    }
    None
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match early_flag(&args) {
        Some(EarlyFlag::Version) => {
            println!("scipd {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(EarlyFlag::Help) => {
            println!("scipd — live code-intel daemon. Usage: scipd [--version|-V] [--help|-h]");
            return ExitCode::SUCCESS;
        }
        None => {}
    }

    let home = match codeintel_home() {
        Ok(home) => home,
        Err(e) => {
            return fail(
                e.code_str(),
                e.to_string(),
                "set $CODEINTEL_HOME or $HOME so scipd can find its home",
            );
        }
    };
    if let Err(e) = std::fs::create_dir_all(&home) {
        return fail(
            "DAEMON_START_FAILED",
            format!("creating {}: {e}", home.display()),
            "check permissions on the codeintel home directory",
        );
    }

    // Malformed config is fatal and loud — never default-on-parse-failure
    // (SPEC-A2 §4, Standing Order S6).
    let config = match ScipdConfig::load(&home) {
        Ok(config) => config,
        Err(e) => {
            return fail(
                "BAD_CONFIG",
                e,
                "fix or remove scipd.toml; missing file means defaults, malformed never does",
            );
        }
    };

    // The live pool: one rust-analyzer per worktree, quiescent-fallback
    // window from config (SPEC-A2 §4 caveat).
    let warm_fallback = Duration::from_secs(config.warm_fallback_secs);
    let pool: Arc<Pool<LiveInstance>> = Arc::new(Pool::new(config, move |root| {
        LiveInstance::spawn_with_options(
            scipd_core::live::instance::RUST_ANALYZER_BIN,
            root,
            warm_fallback,
        )
    }));

    let daemon = match Daemon::bind(&home, Arc::clone(&pool)) {
        Ok(daemon) => daemon,
        Err(e @ BindError::AlreadyRunning { .. }) => {
            return fail(
                "ALREADY_RUNNING",
                e.to_string(),
                "exactly one scipd per codeintel home; stop the other instance first",
            );
        }
        Err(e) => {
            return fail(
                "DAEMON_START_FAILED",
                e.to_string(),
                "check the codeintel home is writable and the socket path is free",
            );
        }
    };

    // SIGTERM (launchd stop) and SIGINT (interactive) drain the accept loop.
    let flag = daemon.shutdown_flag();
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        if let Err(e) = signal_hook::flag::register(sig, flag.clone()) {
            return fail(
                "DAEMON_START_FAILED",
                format!("registering signal {sig}: {e}"),
                "this should never fail; report it",
            );
        }
    }

    // Reaper/watchdog thread: one pool policy pass every SWEEP_INTERVAL
    // (SPEC-A2 §4), polling the shutdown flag so SIGTERM stays responsive.
    let reaper = {
        let pool = Arc::clone(&pool);
        let flag = flag.clone();
        std::thread::Builder::new()
            .name("scipd-reaper".into())
            .spawn(move || {
                let mut next_sweep = Instant::now() + SWEEP_INTERVAL;
                while !flag.load(Ordering::SeqCst) {
                    if Instant::now() >= next_sweep {
                        pool.sweep();
                        next_sweep = Instant::now() + SWEEP_INTERVAL;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
    };
    let reaper = match reaper {
        Ok(handle) => handle,
        Err(e) => {
            return fail(
                "DAEMON_START_FAILED",
                format!("spawning reaper thread: {e}"),
                "this should never fail; report it",
            );
        }
    };

    let run_result = daemon.run();

    // Accept loop drained (SIGTERM/SIGINT or crash): stop the reaper, then
    // shut every live instance down — no orphan rust-analyzer children.
    flag.store(true, Ordering::SeqCst);
    if reaper.join().is_err() {
        eprintln!("scipd: reaper thread panicked during shutdown");
    }
    pool.shutdown_all();

    match run_result {
        Ok(()) => {
            eprintln!("scipd: clean shutdown");
            ExitCode::SUCCESS
        }
        Err(e) => fail(
            "DAEMON_CRASHED",
            format!("accept loop failed: {e}"),
            "launchd KeepAlive will restart scipd; check ~/.codeintel/logs",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_version_flags() {
        assert_eq!(
            early_flag(&["--version".to_string()]),
            Some(EarlyFlag::Version)
        );
        assert_eq!(early_flag(&["-V".to_string()]), Some(EarlyFlag::Version));
    }

    #[test]
    fn recognizes_help_flags() {
        assert_eq!(early_flag(&["--help".to_string()]), Some(EarlyFlag::Help));
        assert_eq!(early_flag(&["-h".to_string()]), Some(EarlyFlag::Help));
    }

    #[test]
    fn other_args_are_not_early_flags() {
        assert_eq!(early_flag(&[]), None);
        assert_eq!(early_flag(&["--foo".to_string()]), None);
        assert_eq!(
            early_flag(&["serve".to_string(), "--bar".to_string()]),
            None
        );
    }

    #[test]
    fn version_takes_priority_when_both_present() {
        assert_eq!(
            early_flag(&["--help".to_string(), "--version".to_string()]),
            Some(EarlyFlag::Version)
        );
    }
}
