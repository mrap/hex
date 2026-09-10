//! Test-only exact-child support for binary tests with process-wide effects.
//!
//! This module is compiled only for the harness binary test target. It keeps
//! synthetic child state out of the full libtest parent and retains bounded
//! diagnostic output when a child fails or exceeds its execution deadline.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

pub const EXECUTION_TIMEOUT: Duration = Duration::from_secs(15);
const CLEANUP_GRACE: Duration = Duration::from_millis(250);
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(300);
const ADMISSION_POLL: Duration = Duration::from_millis(10);
pub const OUTPUT_TAIL_BYTES: usize = 4096;

static ADMISSION: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    None,
    BeforeStdoutReader,
    BeforeStderrReader,
    BothReadersFailAfterRead,
}

#[derive(Debug)]
pub struct OutputTail {
    pub total_bytes: usize,
    pub tail: Vec<u8>,
    pub truncated: bool,
    pub read_error: Option<String>,
}

#[derive(Debug)]
pub struct ChildOutput {
    pub status: ExitStatus,
    pub stdout: OutputTail,
    pub stderr: OutputTail,
}

#[derive(Debug, Default)]
struct JoinedPipes {
    stdout: Option<OutputTail>,
    stderr: Option<OutputTail>,
    errors: Vec<String>,
}

pub fn in_child(test_name: &str) -> bool {
    std::env::var("HEX_TEST_CHILD_SELECTOR").ok().as_deref() == Some(test_name)
}

pub fn stage(name: &str) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "HEX_TEST_CHILD_STAGE:{name}")
        .map_err(|error| format!("test-child stage {name} write failed: {error}"))?;
    stdout
        .flush()
        .map_err(|error| format!("test-child stage {name} flush failed: {error}"))
}

fn admission_lock() -> &'static Mutex<()> {
    ADMISSION.get_or_init(|| Mutex::new(()))
}

pub fn acquire_admission(timeout: Duration) -> Result<MutexGuard<'static, ()>, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match admission_lock().try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(_)) => {
                return Err("test-child admission guard is poisoned".to_string());
            }
            Err(TryLockError::WouldBlock) if Instant::now() >= deadline => {
                return Err(format!(
                    "test-child admission exceeded {} seconds before child spawn",
                    timeout.as_secs()
                ));
            }
            Err(TryLockError::WouldBlock) => thread::sleep(ADMISSION_POLL),
        }
    }
}

pub fn acquire_default_admission() -> Result<MutexGuard<'static, ()>, String> {
    acquire_admission(ADMISSION_TIMEOUT)
}

fn append_tail(output: &mut OutputTail, bytes: &[u8]) {
    output.total_bytes += bytes.len();
    output.tail.extend_from_slice(bytes);
    if output.tail.len() > OUTPUT_TAIL_BYTES {
        let excess = output.tail.len() - OUTPUT_TAIL_BYTES;
        output.tail.drain(..excess);
        output.truncated = true;
    }
}

fn read_pipe(mut pipe: impl Read + Send + 'static, fault: Fault, name: &'static str) -> OutputTail {
    let mut output = OutputTail {
        total_bytes: 0,
        tail: Vec::new(),
        truncated: false,
        read_error: None,
    };
    let mut buffer = [0_u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => append_tail(&mut output, &buffer[..count]),
            Err(error) => {
                output.read_error = Some(error.to_string());
                return output;
            }
        }
    }
    if fault == Fault::BothReadersFailAfterRead {
        output.read_error = Some(format!("controlled {name} reader failure"));
    }
    output
}

fn join_one(name: &str, reader: Option<thread::JoinHandle<OutputTail>>, joined: &mut JoinedPipes) {
    let output = match reader {
        Some(reader) => match reader.join() {
            Ok(output) => Some(output),
            Err(_) => {
                joined.errors.push(format!("{name} reader panicked"));
                None
            }
        },
        None => {
            joined.errors.push(format!("{name} reader was not started"));
            None
        }
    };
    if let Some(output) = output {
        if let Some(error) = &output.read_error {
            joined.errors.push(format!(
                "{name} reader failed: {error}; {}",
                tail_summary(&output)
            ));
        }
        match name {
            "stdout" => joined.stdout = Some(output),
            "stderr" => joined.stderr = Some(output),
            _ => unreachable!("only stdout and stderr readers are joined"),
        }
    }
}

fn join_all(
    stdout: Option<thread::JoinHandle<OutputTail>>,
    stderr: Option<thread::JoinHandle<OutputTail>>,
) -> JoinedPipes {
    let mut joined = JoinedPipes::default();
    // Always join both. A failing reader must not discard the other stream.
    join_one("stdout", stdout, &mut joined);
    join_one("stderr", stderr, &mut joined);
    joined
}

fn tail_summary(output: &OutputTail) -> String {
    format!(
        "bytes={} truncated={} tail={:?}",
        output.total_bytes,
        output.truncated,
        String::from_utf8_lossy(&output.tail)
    )
}

fn ledger_joined(ledger: &mut Vec<String>, joined: &JoinedPipes) {
    match &joined.stdout {
        Some(stdout) => ledger.push(format!("stdout reader: {}", tail_summary(stdout))),
        None => ledger.push("stdout reader: no-output".to_string()),
    }
    match &joined.stderr {
        Some(stderr) => ledger.push(format!("stderr reader: {}", tail_summary(stderr))),
        None => ledger.push("stderr reader: no-output".to_string()),
    }
    ledger.extend(joined.errors.iter().cloned());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupSignal {
    Sent,
    Esrch,
    Failed,
    NotAttempted,
}

fn signal_owned_group(pid: u32, signal: i32, ledger: &mut Vec<String>) -> GroupSignal {
    let Ok(group) = i32::try_from(pid) else {
        ledger.push(format!(
            "group signal {signal}: not-attempted pid-not-representable"
        ));
        return GroupSignal::NotAttempted;
    };
    // process_group(0) creates a child-led group. Rechecking getpgid protects
    // against a stale or reused PID before a negative-PID group signal.
    let actual_group = unsafe { libc::getpgid(group) };
    if actual_group == -1 {
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => {
                ledger.push(format!("group signal {signal}: ESRCH before group signal"));
                return GroupSignal::Esrch;
            }
            Some(libc::EPERM) => {
                ledger.push(format!(
                    "group signal {signal}: EPERM resolving child group"
                ));
                return GroupSignal::Failed;
            }
            _ => {
                ledger.push(format!(
                    "group signal {signal}: error resolving child group: {error}"
                ));
                return GroupSignal::Failed;
            }
        }
    }
    if actual_group != group {
        ledger.push(format!(
            "group signal {signal}: not-attempted unowned-pgid={actual_group} expected={group}"
        ));
        return GroupSignal::NotAttempted;
    }
    if unsafe { libc::kill(-group, signal) } == 0 {
        ledger.push(format!("group signal {signal}: sent owned-pgid={group}"));
        return GroupSignal::Sent;
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => {
            ledger.push(format!("group signal {signal}: ESRCH"));
            GroupSignal::Esrch
        }
        Some(libc::EPERM) => {
            ledger.push(format!("group signal {signal}: EPERM"));
            GroupSignal::Failed
        }
        _ => {
            ledger.push(format!("group signal {signal}: error {error}"));
            GroupSignal::Failed
        }
    }
}

fn direct_kill_after_group_failure(child: &mut Child, reason: &str, ledger: &mut Vec<String>) {
    match child.try_wait() {
        Ok(Some(status)) => ledger.push(format!(
            "direct child kill: not-attempted {reason}; root child already exited {status}"
        )),
        Ok(None) => match child.kill() {
            Ok(()) => ledger.push(format!("direct child kill: sent after {reason}")),
            Err(error) => ledger.push(format!("direct child kill: error after {reason}: {error}")),
        },
        Err(error) => ledger.push(format!(
            "direct child kill: not-attempted try-wait error: {error}"
        )),
    }
}

fn cleanup(
    child: &mut Child,
    pid: u32,
    stdout: Option<thread::JoinHandle<OutputTail>>,
    stderr: Option<thread::JoinHandle<OutputTail>>,
    primary: String,
) -> String {
    let cleanup_started = Instant::now();
    let mut ledger = vec![
        primary,
        "cleanup: final root-child wait and reader joins are intentionally unbounded".to_string(),
        "cleanup: root child exit does not prove descendant or process-group cleanup".to_string(),
    ];
    let term = signal_owned_group(pid, libc::SIGTERM, &mut ledger);
    if matches!(term, GroupSignal::Failed | GroupSignal::NotAttempted) {
        direct_kill_after_group_failure(child, "group TERM failure", &mut ledger);
    } else {
        ledger.push("direct child kill: not-attempted group TERM sent-or-absent".to_string());
    }

    let grace_deadline = Instant::now() + CLEANUP_GRACE;
    let mut exited = false;
    let mut grace_wait_error = false;
    while Instant::now() < grace_deadline {
        match child.try_wait() {
            Ok(Some(status)) => {
                exited = true;
                ledger.push(format!("grace wait: root child exited {status}"));
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                grace_wait_error = true;
                ledger.push(format!("grace wait: error {error}"));
                break;
            }
        }
    }
    if exited {
        ledger.push("group signal 9: not-attempted root child exited during grace".to_string());
        ledger.push("direct child kill: not-attempted root child exited during grace".to_string());
    } else {
        if grace_wait_error {
            ledger.push("grace wait: state-unknown-after-error before group KILL".to_string());
        } else {
            ledger.push("grace wait: expired/still-running before group KILL".to_string());
        }
        let kill = signal_owned_group(pid, libc::SIGKILL, &mut ledger);
        if matches!(kill, GroupSignal::Failed | GroupSignal::NotAttempted) {
            direct_kill_after_group_failure(child, "group KILL failure", &mut ledger);
        } else {
            ledger.push("direct child kill: not-attempted group KILL sent-or-absent".to_string());
        }
    }
    match child.wait() {
        Ok(status) => ledger.push(format!(
            "direct child wait: success {status}; owned root child reaped"
        )),
        Err(error) => ledger.push(format!("direct child wait: error {error}")),
    }
    let joined = join_all(stdout, stderr);
    ledger_joined(&mut ledger, &joined);
    ledger.push(format!(
        "cleanup elapsed_ms={}",
        cleanup_started.elapsed().as_millis()
    ));
    ledger.join("; ")
}

pub fn run_exact(
    test_name: &str,
    home: &Path,
    hex_dir: &Path,
    fault: Fault,
) -> Result<ChildOutput, String> {
    let _admission = acquire_default_admission()?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = std::process::Command::new(executable);
    command
        .args(["--exact", test_name, "--nocapture"])
        .env_clear()
        .env("HOME", home)
        .env("HEX_DIR", hex_dir)
        .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin")
        .env("HEX_TEST_CHILD_SELECTOR", test_name)
        .env("HEX_DISTILL_FORCE_EXTRACT_FAIL", "deferred")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("test child failed to start: {error}"))?;
    let pid = child.id();
    let stdout_pipe = match child.stdout.take() {
        Some(pipe) => pipe,
        None => {
            return Err(cleanup(
                &mut child,
                pid,
                None,
                None,
                "test child has no stdout pipe".to_string(),
            ));
        }
    };
    if fault == Fault::BeforeStdoutReader {
        drop(stdout_pipe);
        return Err(cleanup(
            &mut child,
            pid,
            None,
            None,
            "controlled stdout pipe setup failure".to_string(),
        ));
    }
    let stdout = thread::spawn(move || read_pipe(stdout_pipe, fault, "stdout"));
    if fault == Fault::BeforeStderrReader {
        return Err(cleanup(
            &mut child,
            pid,
            Some(stdout),
            None,
            "controlled stderr pipe setup failure".to_string(),
        ));
    }
    let stderr_pipe = match child.stderr.take() {
        Some(pipe) => pipe,
        None => {
            return Err(cleanup(
                &mut child,
                pid,
                Some(stdout),
                None,
                "test child has no stderr pipe".to_string(),
            ));
        }
    };
    let stderr = thread::spawn(move || read_pipe(stderr_pipe, fault, "stderr"));
    let deadline = Instant::now() + EXECUTION_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                return Err(cleanup(
                    &mut child,
                    pid,
                    Some(stdout),
                    Some(stderr),
                    format!("test child {test_name} exceeded 15 seconds"),
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                return Err(cleanup(
                    &mut child,
                    pid,
                    Some(stdout),
                    Some(stderr),
                    format!("test child {test_name} wait failed: {error}"),
                ));
            }
        }
    };
    let joined = join_all(Some(stdout), Some(stderr));
    if !joined.errors.is_empty() {
        let mut ledger = vec![format!("test child {test_name} finished {status}")];
        ledger_joined(&mut ledger, &joined);
        return Err(ledger.join("; "));
    }
    let stdout = joined
        .stdout
        .expect("successful stdout reader must retain output");
    let stderr = joined
        .stderr
        .expect("successful stderr reader must retain output");
    Ok(ChildOutput {
        status,
        stdout,
        stderr,
    })
}

pub fn require_one_pass(test_name: &str, output: ChildOutput) -> Result<(), String> {
    let stdout = String::from_utf8_lossy(&output.stdout.tail);
    let expected_test = format!("test {test_name} ... ok");
    if !output.status.success()
        || !stdout.contains("running 1 test")
        || !stdout.contains(&expected_test)
        || !stdout.contains("test result: ok. 1 passed")
    {
        return Err(format!(
            "test child {test_name} did not prove one exact passing test (status {}); stdout {}; stderr {}",
            output.status,
            tail_summary(&output.stdout),
            tail_summary(&output.stderr)
        ));
    }
    println!(
        "HEX_TEST_CHILD_SUCCESS test={test_name} status={} stdout={} stderr={}",
        output.status,
        tail_summary(&output.stdout),
        tail_summary(&output.stderr)
    );
    Ok(())
}
