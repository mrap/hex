//! Private-process bridge to the managed Cargo gate.
//!
//! Capture retains at most [`MAX_CAPTURED_BYTES`] raw bytes from the end of
//! each stream. UTF-8 display conversion can expand replacement characters.

use serde_json::Value;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use tempfile::{Builder as TempBuilder, TempDir};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

const GATE: &str = include_str!("scripts/managed-cargo-gate.py");
const ADAPTER: &str = include_str!("scripts/managed-target-check.py");
pub const MAX_CAPTURED_BYTES: usize = 64 * 1024;
const MAX_RECEIPT_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    ReleaseTests,
    CodeIntelBuild,
}

impl Caller {
    fn name(self) -> &'static str {
        match self {
            Self::ReleaseTests => "release-tests",
            Self::CodeIntelBuild => "code-intel-build",
        }
    }

    fn operation(self) -> &'static str {
        match self {
            Self::ReleaseTests => "test",
            Self::CodeIntelBuild => "build",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    Clean,
    Dirty,
    Unavailable,
}

impl SourceState {
    fn text(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Dirty => "dirty",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Capture,
    Inherit,
}

#[derive(Debug, Clone)]
pub struct ReceiptLocation {
    /// An existing, canonical, user-owned private directory.
    pub boundary: PathBuf,
    /// Plain components owned and created below `boundary` by this bridge.
    pub components: Vec<String>,
}

fn valid_location(boundary: &Path, components: &[String]) -> bool {
    boundary.is_absolute()
        && !components.is_empty()
        && !components.iter().any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || Path::new(component).components().count() != 1
        })
}

impl ReceiptLocation {
    pub fn new(
        boundary: PathBuf,
        components: Vec<String>,
    ) -> std::result::Result<Self, BridgeError> {
        if !valid_location(&boundary, &components) {
            return Err(BridgeError::UnsafePath("invalid receipt location".into()));
        }
        Ok(Self {
            boundary,
            components,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub caller: Caller,
    pub source_revision: String,
    pub source_state: SourceState,
    pub working_repo: PathBuf,
    pub cargo_args: Vec<String>,
    pub receipt: ReceiptLocation,
    pub output: OutputMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputTail {
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoStatus {
    Exit(i32),
    Signal(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeError {
    UnsafePath(String),
    Spawn(String),
    Read(String),
    Join(String),
    Wait(String),
    Protocol(String),
    Gate(String),
}

#[derive(Debug, Clone)]
pub struct Evidence {
    pub invocation_dir: Option<PathBuf>,
    pub receipt: Option<PathBuf>,
    pub target: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Result {
    pub cargo: Option<CargoStatus>,
    pub stdout: OutputTail,
    pub stderr: OutputTail,
    pub evidence: Evidence,
    pub error: Option<BridgeError>,
}

#[derive(Clone, Copy)]
struct Identity {
    dev: u64,
    ino: u64,
}

fn empty_tail() -> OutputTail {
    OutputTail {
        text: String::new(),
        truncated: false,
    }
}

fn failure(error: BridgeError, evidence: Evidence) -> Result {
    failure_with_output(error, evidence, empty_tail(), empty_tail())
}

fn failure_with_output(
    error: BridgeError,
    evidence: Evidence,
    stdout: OutputTail,
    stderr: OutputTail,
) -> Result {
    Result {
        cargo: None,
        stdout,
        stderr,
        evidence,
        error: Some(error),
    }
}

fn invocation_evidence(invocation_dir: PathBuf) -> Evidence {
    Evidence {
        invocation_dir: Some(invocation_dir),
        receipt: None,
        target: None,
    }
}

fn safe_existing_dir_identity(path: &Path) -> std::result::Result<Identity, BridgeError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| BridgeError::UnsafePath(error.to_string()))?;
    #[cfg(unix)]
    {
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() } as u32
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(BridgeError::UnsafePath(format!(
                "unsafe private directory: {}",
                path.display()
            )));
        }
        Ok(Identity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(BridgeError::UnsafePath(format!(
                "unsafe private directory: {}",
                path.display()
            )));
        }
        Ok(Identity { dev: 0, ino: 0 })
    }
}

struct CheckedDir {
    path: PathBuf,
    identity: Identity,
    created_here: bool,
}

fn verify_checked_dir(check: &CheckedDir) -> std::result::Result<(), BridgeError> {
    let actual = if check.created_here {
        strict_private_dir_identity(&check.path)?
    } else {
        safe_existing_dir_identity(&check.path)?
    };
    if actual.dev != check.identity.dev || actual.ino != check.identity.ino {
        return Err(BridgeError::Protocol(format!(
            "receipt directory identity changed: {}",
            check.path.display()
        )));
    }
    Ok(())
}

fn strict_private_dir_identity(path: &Path) -> std::result::Result<Identity, BridgeError> {
    let identity = safe_existing_dir_identity(path)?;
    #[cfg(unix)]
    {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| BridgeError::UnsafePath(error.to_string()))?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(BridgeError::UnsafePath(format!(
                "new private directory is not 0700: {}",
                path.display()
            )));
        }
    }
    Ok(identity)
}

fn verify_private_dir(path: &Path, expected: Identity) -> std::result::Result<(), BridgeError> {
    let actual = strict_private_dir_identity(path)?;
    if actual.dev != expected.dev || actual.ino != expected.ino {
        return Err(BridgeError::Protocol(
            "invocation directory identity changed".into(),
        ));
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> std::result::Result<Identity, BridgeError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|error| BridgeError::UnsafePath(error.to_string()))?;
    strict_private_dir_identity(path)
}

fn private_tree(
    location: &ReceiptLocation,
) -> std::result::Result<(PathBuf, Vec<CheckedDir>), BridgeError> {
    if !valid_location(&location.boundary, &location.components) {
        return Err(BridgeError::UnsafePath("invalid receipt location".into()));
    }
    let canonical = fs::canonicalize(&location.boundary)
        .map_err(|error| BridgeError::UnsafePath(error.to_string()))?;
    if canonical != location.boundary {
        return Err(BridgeError::UnsafePath(
            "receipt boundary must already be canonical".into(),
        ));
    }
    let mut checks = vec![CheckedDir {
        path: canonical.clone(),
        identity: safe_existing_dir_identity(&canonical)?,
        created_here: false,
    }];
    let mut current = canonical;
    for component in &location.components {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(_) => checks.push(CheckedDir {
                path: current.clone(),
                identity: safe_existing_dir_identity(&current)?,
                created_here: false,
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => checks.push(CheckedDir {
                path: current.clone(),
                identity: create_private_dir(&current)?,
                created_here: true,
            }),
            Err(error) => return Err(BridgeError::UnsafePath(error.to_string())),
        }
    }
    Ok((current, checks))
}

fn read_tail(mut reader: impl Read) -> io::Result<OutputTail> {
    let mut retained = Vec::new();
    let mut chunk = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        if read >= MAX_CAPTURED_BYTES {
            retained.clear();
            retained.extend_from_slice(&chunk[read - MAX_CAPTURED_BYTES..read]);
            truncated = true;
        } else {
            let total = retained.len() + read;
            if total > MAX_CAPTURED_BYTES {
                retained.drain(..total - MAX_CAPTURED_BYTES);
                truncated = true;
            }
            retained.extend_from_slice(&chunk[..read]);
        }
    }
    Ok(OutputTail {
        text: String::from_utf8_lossy(&retained).into_owned(),
        truncated,
    })
}

fn write_helpers() -> std::result::Result<(TempDir, PathBuf), BridgeError> {
    let directory = TempBuilder::new()
        .prefix("hex-managed-cargo-")
        .tempdir()
        .map_err(|error| BridgeError::Spawn(error.to_string()))?;
    let gate = directory.path().join("managed-cargo-gate.py");
    fs::write(&gate, GATE)
        .and_then(|_| fs::write(directory.path().join("managed-target-check.py"), ADAPTER))
        .map_err(|error| BridgeError::Spawn(error.to_string()))?;
    Ok((directory, gate))
}

fn private_regular_identity(metadata: &fs::Metadata) -> std::result::Result<Identity, BridgeError> {
    #[cfg(unix)]
    {
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() } as u32
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(BridgeError::Protocol(
                "receipt is not a private regular file".into(),
            ));
        }
        Ok(Identity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        if !metadata.is_file() {
            return Err(BridgeError::Protocol(
                "receipt is not a private regular file".into(),
            ));
        }
        Ok(Identity { dev: 0, ino: 0 })
    }
}

fn read_private_receipt(path: &Path) -> std::result::Result<Value, BridgeError> {
    let before_path =
        fs::symlink_metadata(path).map_err(|error| BridgeError::Protocol(error.to_string()))?;
    if before_path.file_type().is_symlink() {
        return Err(BridgeError::Protocol("receipt is a symlink".into()));
    }
    let before = private_regular_identity(&before_path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let mut file = options
        .open(path)
        .map_err(|error| BridgeError::Protocol(error.to_string()))?;
    let opened = file
        .metadata()
        .map_err(|error| BridgeError::Protocol(error.to_string()))?;
    let opened_identity = private_regular_identity(&opened)?;
    if opened_identity.dev != before.dev
        || opened_identity.ino != before.ino
        || opened.len() > MAX_RECEIPT_BYTES
    {
        return Err(BridgeError::Protocol(
            "receipt identity or size changed before read".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.by_ref()
        .take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| BridgeError::Protocol(error.to_string()))?;
    let after = file
        .metadata()
        .map_err(|error| BridgeError::Protocol(error.to_string()))?;
    let after_identity = private_regular_identity(&after)?;
    let after_path =
        fs::symlink_metadata(path).map_err(|error| BridgeError::Protocol(error.to_string()))?;
    let final_identity = private_regular_identity(&after_path)?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES
        || after_identity.dev != opened_identity.dev
        || after_identity.ino != opened_identity.ino
        || after.len() != opened.len()
        || final_identity.dev != opened_identity.dev
        || final_identity.ino != opened_identity.ino
    {
        return Err(BridgeError::Protocol(
            "receipt identity or size changed while read".into(),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| BridgeError::Protocol(format!("malformed receipt: {error}")))
}

fn read_receipt(
    invocation_dir: &Path,
    request: &Request,
) -> std::result::Result<(PathBuf, PathBuf, CargoStatus), BridgeError> {
    let mut entries =
        fs::read_dir(invocation_dir).map_err(|error| BridgeError::Protocol(error.to_string()))?;
    let first = entries
        .next()
        .transpose()
        .map_err(|error| BridgeError::Protocol(error.to_string()))?
        .ok_or_else(|| BridgeError::Protocol("managed gate left no receipt".into()))?;
    if entries
        .next()
        .transpose()
        .map_err(|error| BridgeError::Protocol(error.to_string()))?
        .is_some()
    {
        return Err(BridgeError::Protocol(
            "managed gate left multiple receipt entries".into(),
        ));
    }
    let receipt_path = first.path();
    let receipt = read_private_receipt(&receipt_path)?;
    if receipt["schema_version"] != "foundation.managed-cargo-gate.v1"
        || receipt["source_revision"] != request.source_revision
        || receipt["source_state"] != request.source_state.text()
        || receipt["operation"] != request.caller.operation()
        || receipt["managed_target"]["caller_identity"] != request.caller.name()
        || receipt["target_created"] != true
        || receipt["outcome"]["state"] != "completed"
    {
        return Err(BridgeError::Protocol(
            "receipt context is not the requested completed invocation".into(),
        ));
    }
    let target = receipt["managed_target"]["resolved_target"]
        .as_str()
        .filter(|value| Path::new(value).is_absolute())
        .ok_or_else(|| BridgeError::Protocol("receipt target is not absolute".into()))?;
    let code = receipt["outcome"]["cargo_exit_code"]
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            BridgeError::Gate("gate did not record an integer completed Cargo status".into())
        })?;
    let status =
        if code < 0 {
            CargoStatus::Signal(code.checked_neg().ok_or_else(|| {
                BridgeError::Protocol("receipt signal is outside i32 range".into())
            })?)
        } else {
            CargoStatus::Exit(code)
        };
    Ok((receipt_path, PathBuf::from(target), status))
}

fn terminate_and_reap(child: &mut Child, primary: BridgeError) -> BridgeError {
    let kill_error = child.kill().err();
    let reap_error = child.wait().err();
    let mut detail = format!("{primary:?}");
    match kill_error {
        Some(error) => detail.push_str(&format!("; termination failed: {error}")),
        None => detail.push_str("; termination attempted"),
    }
    match reap_error {
        Some(error) => detail.push_str(&format!("; final reap failed: {error}")),
        None => detail.push_str("; final reap completed"),
    }
    BridgeError::Read(detail)
}

fn wait_or_reap(child: &mut Child) -> std::result::Result<ExitStatus, BridgeError> {
    match child.wait() {
        Ok(status) => Ok(status),
        Err(wait_error) => {
            let kill_error = child.kill().err();
            let reap_error = child.wait().err();
            let mut detail = format!("owned child wait failed: {wait_error}");
            match kill_error {
                Some(error) => detail.push_str(&format!("; termination failed: {error}")),
                None => detail.push_str("; termination attempted"),
            }
            match reap_error {
                Some(error) => detail.push_str(&format!("; final reap failed: {error}")),
                None => detail.push_str("; final reap completed"),
            }
            Err(BridgeError::Wait(detail))
        }
    }
}

fn transport_matches(status: &ExitStatus, cargo: &CargoStatus) -> bool {
    let cargo_code = match cargo {
        CargoStatus::Exit(code) => *code,
        CargoStatus::Signal(signal) => -*signal,
    };
    status.code() == Some(cargo_code.rem_euclid(256))
}

fn run_with_environment(
    request: &Request,
    python: &Path,
    environment: Option<&[(OsString, OsString)]>,
) -> Result {
    let (receipt_parent, receipt_checks) = match private_tree(&request.receipt) {
        Ok(tree) => tree,
        Err(error) => {
            return failure(
                error,
                Evidence {
                    invocation_dir: None,
                    receipt: None,
                    target: None,
                },
            )
        }
    };
    let invocation_dir = receipt_parent.join(format!("managed-cargo-{}", Uuid::new_v4().simple()));
    let invocation_identity = match create_private_dir(&invocation_dir) {
        Ok(identity) => identity,
        Err(error) => {
            return failure(
                error,
                Evidence {
                    invocation_dir: None,
                    receipt: None,
                    target: None,
                },
            )
        }
    };
    let (_helper_dir, gate) = match write_helpers() {
        Ok(helper) => helper,
        Err(error) => return failure(error, invocation_evidence(invocation_dir)),
    };
    if !request.working_repo.is_dir() {
        return failure(
            BridgeError::UnsafePath("working repository is not a directory".into()),
            invocation_evidence(invocation_dir),
        );
    }

    let mut command = Command::new(python);
    if let Some(environment) = environment {
        command
            .env_clear()
            .envs(environment.iter().map(|(key, value)| (key, value)));
    }
    command
        .args(["-I", "-B"])
        .arg(gate)
        .args([
            "--caller",
            request.caller.name(),
            "--source-revision",
            &request.source_revision,
            "--source-state",
            request.source_state.text(),
            "--receipt-dir",
        ])
        .arg(&invocation_dir)
        .arg(request.caller.operation())
        .args(["--fresh-target", "--quiet-summary"])
        .args(&request.cargo_args)
        .current_dir(&request.working_repo)
        .stdin(Stdio::null());
    if request.output == OutputMode::Capture {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return failure(
                BridgeError::Spawn(error.to_string()),
                invocation_evidence(invocation_dir),
            )
        }
    };
    let (stdout, stderr, status) = if request.output == OutputMode::Capture {
        let stdout = match child.stdout.take() {
            Some(stream) => stream,
            None => {
                return failure(
                    terminate_and_reap(&mut child, BridgeError::Read("missing stdout pipe".into())),
                    invocation_evidence(invocation_dir),
                );
            }
        };
        let stderr = match child.stderr.take() {
            Some(stream) => stream,
            None => {
                return failure(
                    terminate_and_reap(&mut child, BridgeError::Read("missing stderr pipe".into())),
                    invocation_evidence(invocation_dir),
                );
            }
        };
        let stdout_reader = thread::spawn(move || read_tail(stdout));
        let stderr_reader = thread::spawn(move || read_tail(stderr));
        let waited = wait_or_reap(&mut child);
        let stdout = stdout_reader
            .join()
            .map_err(|_| BridgeError::Join("stdout reader panicked".into()))
            .and_then(|result| result.map_err(|error| BridgeError::Read(error.to_string())));
        let stderr = stderr_reader
            .join()
            .map_err(|_| BridgeError::Join("stderr reader panicked".into()))
            .and_then(|result| result.map_err(|error| BridgeError::Read(error.to_string())));
        match (stdout, stderr, waited) {
            (Ok(stdout), Ok(stderr), Ok(status)) => (stdout, stderr, status),
            (stdout, stderr, waited) => {
                let retained_stdout = stdout.as_ref().ok().cloned().unwrap_or_else(empty_tail);
                let retained_stderr = stderr.as_ref().ok().cloned().unwrap_or_else(empty_tail);
                let mut errors = Vec::new();
                if let Err(error) = stdout {
                    errors.push(format!("stdout: {error:?}"));
                }
                if let Err(error) = stderr {
                    errors.push(format!("stderr: {error:?}"));
                }
                if let Err(error) = waited {
                    errors.push(format!("wait: {error:?}"));
                }
                return failure_with_output(
                    BridgeError::Read(format!("capture completion failed: {}", errors.join("; "))),
                    invocation_evidence(invocation_dir),
                    retained_stdout,
                    retained_stderr,
                );
            }
        }
    } else {
        match wait_or_reap(&mut child) {
            Ok(status) => (empty_tail(), empty_tail(), status),
            Err(error) => return failure(error, invocation_evidence(invocation_dir)),
        }
    };

    for check in &receipt_checks {
        if let Err(error) = verify_checked_dir(check) {
            return failure_with_output(error, invocation_evidence(invocation_dir), stdout, stderr);
        }
    }
    if let Err(error) = verify_private_dir(&invocation_dir, invocation_identity) {
        return failure_with_output(error, invocation_evidence(invocation_dir), stdout, stderr);
    }
    let receipt_result = read_receipt(&invocation_dir, request);
    for check in &receipt_checks {
        if let Err(error) = verify_checked_dir(check) {
            return failure_with_output(error, invocation_evidence(invocation_dir), stdout, stderr);
        }
    }
    if let Err(error) = verify_private_dir(&invocation_dir, invocation_identity) {
        return failure_with_output(error, invocation_evidence(invocation_dir), stdout, stderr);
    }
    match receipt_result {
        Ok((receipt, target, cargo)) if transport_matches(&status, &cargo) => Result {
            cargo: Some(cargo),
            stdout,
            stderr,
            evidence: Evidence {
                invocation_dir: Some(invocation_dir),
                receipt: Some(receipt),
                target: Some(target),
            },
            error: None,
        },
        Ok((receipt, target, _)) => failure_with_output(
            BridgeError::Protocol("Python transport does not match validated Cargo status".into()),
            Evidence {
                invocation_dir: Some(invocation_dir),
                receipt: Some(receipt),
                target: Some(target),
            },
            stdout,
            stderr,
        ),
        Err(error) => {
            failure_with_output(error, invocation_evidence(invocation_dir), stdout, stderr)
        }
    }
}

fn run_with_python(request: &Request, python: &Path) -> Result {
    run_with_environment(request, python, None)
}

pub fn run(request: &Request) -> Result {
    run_with_python(request, Path::new("python3"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::sync::{Mutex, OnceLock};

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn private(path: &Path) {
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn fixture_python(environment: &[(OsString, OsString)]) -> PathBuf {
        let path = environment
            .iter()
            .find_map(|(key, value)| (key == "PATH").then_some(value))
            .expect("fixture must define a closed PATH");
        for directory in std::env::split_paths(path) {
            let candidate = directory.join("python3");
            let Ok(metadata) = fs::metadata(&candidate) else {
                continue;
            };
            #[cfg(unix)]
            let executable = metadata.is_file() && metadata.permissions().mode() & 0o111 != 0;
            #[cfg(not(unix))]
            let executable = metadata.is_file();
            if executable {
                return candidate;
            }
        }
        panic!("fixture PATH has no executable python3");
    }

    fn run_test(request: &Request, environment: &[(OsString, OsString)]) -> Result {
        run_with_environment(request, &fixture_python(environment), Some(environment))
    }

    fn actual(
        mode: &str,
        output: OutputMode,
    ) -> (TempDir, Request, PathBuf, Vec<(OsString, OsString)>) {
        let directory = TempBuilder::new()
            .prefix("managed-bridge-test-")
            .tempdir()
            .unwrap();
        let repo = directory.path().join("repo");
        let boundary = directory.path().join("boundary");
        let home = directory.path().join("home");
        let bin = directory.path().join("bin");
        let managed = directory.path().join("managed");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&boundary).unwrap();
        fs::create_dir_all(home.join(".boi/v2")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&managed).unwrap();
        private(&boundary);
        private(&bin);
        let repo = fs::canonicalize(repo).unwrap();
        let boundary = fs::canonicalize(boundary).unwrap();
        let managed = fs::canonicalize(managed).unwrap();
        fs::write(
            home.join(".boi/v2/daemon.toml"),
            format!(
                "cargo_target_dir = \"{}\"\n[managed_target_policy]\nrevision = \"test\"\nallowed_roots = [\"{}\"]\ndenied_roots = [\"{}\"]\n",
                managed.display(), managed.display(), directory.path().join("denied").display()
            ),
        ).unwrap();
        let log = directory.path().join("fake-cargo.log");
        let helper_log = directory.path().join("helper.log");
        fs::write(
            bin.join("cargo"),
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$FAKE_LOG\"\nprintf 'cwd=%s\\n' \"$PWD\" >> \"$FAKE_LOG\"\nprintf 'target=%s\\n' \"$CARGO_TARGET_DIR\" >> \"$FAKE_LOG\"\nprintf 'build=%s\\n' \"$CARGO_BUILD_BUILD_DIR\" >> \"$FAKE_LOG\"\ncase \"$FAKE_MODE\" in\n  signal) kill -TERM $$ ;;\n  large) /usr/bin/awk 'BEGIN {printf \"out-head\"; for (i=0; i<70000; i++) printf \"x\"; printf \"out-tail\"}' ; /usr/bin/awk 'BEGIN {printf \"err-head\" > \"/dev/stderr\"; for (i=0; i<70000; i++) printf \"y\" > \"/dev/stderr\"; printf \"err-tail\" > \"/dev/stderr\"}' ;;\n  *) printf out; printf err >&2 ;;\nesac\ncase \"$FAKE_MODE\" in negative) exit 7 ;; *) exit 0 ;; esac\n",
        ).unwrap();
        private(&bin.join("cargo"));
        let environment = vec![
            (OsString::from("HOME"), home.into_os_string()),
            (
                OsString::from("PATH"),
                OsString::from(format!("{}:/opt/homebrew/bin:/usr/bin:/bin", bin.display())),
            ),
            (
                OsString::from("CARGO_TARGET_DIR"),
                managed.clone().into_os_string(),
            ),
            (
                OsString::from("CARGO_BUILD_BUILD_DIR"),
                managed.into_os_string(),
            ),
            (OsString::from("FAKE_LOG"), log.clone().into_os_string()),
            (
                OsString::from("FAKE_HELPER_LOG"),
                helper_log.into_os_string(),
            ),
            (OsString::from("FAKE_MODE"), OsString::from(mode)),
        ];
        let request = Request {
            caller: Caller::ReleaseTests,
            source_revision: "bridge-test-revision".into(),
            source_state: SourceState::Clean,
            working_repo: repo,
            cargo_args: vec!["--workspace".into(), "--locked".into(), "--offline".into()],
            receipt: ReceiptLocation::new(boundary, vec!["receipts".into()]).unwrap(),
            output,
        };
        (directory, request, log, environment)
    }

    #[test]
    fn embedded_helper_bytes_are_the_accepted_exact_files() {
        assert_eq!(
            format!("{:x}", Sha256::digest(GATE.as_bytes())),
            "8b4a24287fde00766c0c6545edaa0e8542b116f6d10fee0bad45f8b773eaa4c8"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(ADAPTER.as_bytes())),
            "0e4bf03b775cf718347c9f5b211f404ed1228596805ef9c3e6c588c8258ae87c"
        );
    }

    #[test]
    fn embedded_gate_uses_operation_flags_repo_and_rebound_outputs() {
        let _lock = lock();
        let (_directory, request, log, environment) = actual("success", OutputMode::Capture);
        let result = run_test(&request, &environment);
        assert!(result.error.is_none(), "{result:?}");
        assert_eq!(result.cargo, Some(CargoStatus::Exit(0)));
        assert_eq!(result.stdout.text, "out");
        assert_eq!(result.stderr.text, "err");
        let evidence = result.evidence;
        assert!(
            evidence.invocation_dir.is_some()
                && evidence.receipt.is_some()
                && evidence.target.is_some()
        );
        let log = fs::read_to_string(log).unwrap();
        assert!(log.starts_with("test\n--workspace\n--locked\n--offline\n"));
        assert!(log.contains(&format!("cwd={}", request.working_repo.display())));
        let target = evidence.target.unwrap();
        assert!(target.starts_with(
            environment
                .iter()
                .find(|(key, _)| key == "CARGO_TARGET_DIR")
                .unwrap()
                .1
                .as_os_str()
        ));
        assert!(log.contains(&format!("target={}", target.display())));
        assert!(log.contains(&format!("build={}", target.display())));
    }

    #[test]
    fn embedded_gate_preserves_negative_exit_and_signal_from_receipt() {
        let _lock = lock();
        let (_directory, request, log, environment) = actual("negative", OutputMode::Capture);
        let negative = run_test(&request, &environment);
        assert!(negative.error.is_none(), "{negative:?}");
        assert_eq!(negative.cargo, Some(CargoStatus::Exit(7)));
        assert!(log.exists());
        let (_directory, request, _log, environment) = actual("signal", OutputMode::Capture);
        let signal = run_test(&request, &environment);
        assert!(signal.error.is_none(), "{signal:?}");
        assert_eq!(signal.cargo, Some(CargoStatus::Signal(15)));
    }

    #[test]
    fn embedded_gate_capture_keeps_distinct_tail_and_inherit_returns_empty_tails() {
        let _lock = lock();
        let (_directory, request, _log, environment) = actual("large", OutputMode::Capture);
        let captured = run_test(&request, &environment);
        assert!(captured.error.is_none(), "{captured:?}");
        assert!(captured.stdout.truncated && captured.stderr.truncated);
        assert!(
            captured.stdout.text.ends_with("out-tail")
                && !captured.stdout.text.contains("out-head")
        );
        assert!(
            captured.stderr.text.ends_with("err-tail")
                && !captured.stderr.text.contains("err-head")
        );
        let (_directory, request, log, environment) = actual("success", OutputMode::Inherit);
        let inherited = run_test(&request, &environment);
        assert!(inherited.error.is_none(), "{inherited:?}");
        assert_eq!(inherited.stdout, empty_tail());
        assert_eq!(inherited.stderr, empty_tail());
        assert!(log.exists());
    }

    #[test]
    fn inherited_streams_reach_the_enclosing_process() {
        if std::env::var_os("BRIDGE_INHERIT_PROBE").is_some() {
            let (_directory, request, _log, environment) = actual("success", OutputMode::Inherit);
            let result = run_test(&request, &environment);
            assert!(result.error.is_none(), "{result:?}");
            return;
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "managed_cargo_bridge::tests::inherited_streams_reach_the_enclosing_process",
                "--nocapture",
            ])
            .env("BRIDGE_INHERIT_PROBE", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("out"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("err"));
    }

    #[test]
    fn existing_0755_boundary_works_and_unsafe_or_missing_paths_fail_before_cargo() {
        let _lock = lock();
        let (directory, mut request, log, environment) = actual("success", OutputMode::Capture);
        #[cfg(unix)]
        fs::set_permissions(&request.receipt.boundary, fs::Permissions::from_mode(0o755)).unwrap();
        let success = run_test(&request, &environment);
        assert!(success.error.is_none(), "{success:?}");
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&request.receipt.boundary)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        request.receipt.components = vec!["..".into()];
        let traversal = run_test(&request, &environment);
        assert!(matches!(traversal.error, Some(BridgeError::UnsafePath(_))));
        request.receipt.components = vec!["link".into()];
        #[cfg(unix)]
        std::os::unix::fs::symlink(directory.path(), request.receipt.boundary.join("link"))
            .unwrap();
        let symlink = run_test(&request, &environment);
        assert!(matches!(symlink.error, Some(BridgeError::UnsafePath(_))));
        assert!(log.exists());
    }

    #[test]
    fn tail_keeps_last_bytes_and_reader_failure_is_loud() {
        let input = [
            b"head".as_slice(),
            &vec![b'x'; MAX_CAPTURED_BYTES],
            b"tail".as_slice(),
        ]
        .concat();
        let tail = read_tail(input.as_slice()).unwrap();
        assert!(tail.truncated && tail.text.ends_with("tail") && !tail.text.contains("head"));
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("broken reader"))
            }
        }
        assert!(read_tail(Broken).is_err());
    }

    fn controlled_helper(directory: &TempDir) -> PathBuf {
        let helper = directory.path().join("controlled-helper");
        fs::write(&helper, r#"#!/bin/sh
printf '%s\n' "$@" > "$FAKE_HELPER_LOG"
receipt=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --receipt-dir) receipt=$2; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s' "$FAKE_RECEIPT" > "$receipt/receipt.json"
if [ "$FAKE_RECEIPT_MODE" = unsafe ]; then chmod 644 "$receipt/receipt.json"; else chmod 600 "$receipt/receipt.json"; fi
exit "$FAKE_EXIT"
"#).unwrap();
        private(&helper);
        helper
    }

    fn terminal_receipt(request: &Request, target: &Path, cargo_exit: i32) -> Value {
        serde_json::json!({"schema_version":"foundation.managed-cargo-gate.v1","source_revision":request.source_revision,"source_state":request.source_state.text(),"operation":request.caller.operation(),"managed_target":{"caller_identity":request.caller.name(),"resolved_target":target.to_string_lossy()},"target_created":true,"outcome":{"state":"completed","cargo_exit_code":cargo_exit}})
    }

    fn controlled_environment_raw(
        base: &[(OsString, OsString)],
        receipt: OsString,
        mode: &str,
        exit: &str,
    ) -> Vec<(OsString, OsString)> {
        let mut environment = base.to_vec();
        environment.extend([
            (OsString::from("FAKE_RECEIPT"), receipt),
            (OsString::from("FAKE_RECEIPT_MODE"), OsString::from(mode)),
            (OsString::from("FAKE_EXIT"), OsString::from(exit)),
        ]);
        environment
    }

    fn controlled_environment(
        base: &[(OsString, OsString)],
        receipt: Value,
        mode: &str,
        exit: &str,
    ) -> Vec<(OsString, OsString)> {
        controlled_environment_raw(base, OsString::from(receipt.to_string()), mode, exit)
    }

    #[test]
    fn controlled_helper_positive_then_specific_receipt_faults() {
        let _lock = lock();
        let (directory, request, _log, environment) = actual("success", OutputMode::Capture);
        let helper = controlled_helper(&directory);
        let target = fs::canonicalize(directory.path().join("managed")).unwrap();
        let valid = terminal_receipt(&request, &target, 0);
        let positive = run_with_environment(
            &request,
            &helper,
            Some(&controlled_environment(
                &environment,
                valid.clone(),
                "private",
                "0",
            )),
        );
        assert!(positive.error.is_none(), "{positive:?}");
        let helper_log = environment
            .iter()
            .find(|(key, _)| key == "FAKE_HELPER_LOG")
            .unwrap()
            .1
            .clone();
        let argv = fs::read_to_string(helper_log).unwrap();
        let argv: Vec<_> = argv.lines().collect();
        assert_eq!(argv[0..2], ["-I", "-B"]);
        assert_eq!(
            argv[3..],
            [
                "--caller",
                "release-tests",
                "--source-revision",
                "bridge-test-revision",
                "--source-state",
                "clean",
                "--receipt-dir",
                argv[10],
                "test",
                "--fresh-target",
                "--quiet-summary",
                "--workspace",
                "--locked",
                "--offline"
            ]
        );
        for (fault, expected) in [
            ("source", "requested completed"),
            ("operation", "requested completed"),
            ("caller", "requested completed"),
            ("created", "requested completed"),
            ("started", "requested completed"),
        ] {
            let mut receipt = valid.clone();
            match fault {
                "source" => receipt["source_revision"] = Value::String("wrong".into()),
                "operation" => receipt["operation"] = Value::String("build".into()),
                "caller" => {
                    receipt["managed_target"]["caller_identity"] = Value::String("wrong".into())
                }
                "created" => receipt["target_created"] = Value::Bool(false),
                "started" => receipt["outcome"]["state"] = Value::String("started".into()),
                _ => unreachable!(),
            }
            let result = run_with_environment(
                &request,
                &helper,
                Some(&controlled_environment(
                    &environment,
                    receipt,
                    "private",
                    "0",
                )),
            );
            assert!(
                matches!(&result.error, Some(BridgeError::Protocol(detail)) if detail.contains(expected)),
                "{fault}: {result:?}"
            );
        }
        let unsafe_receipt = run_with_environment(
            &request,
            &helper,
            Some(&controlled_environment(
                &environment,
                valid.clone(),
                "unsafe",
                "0",
            )),
        );
        assert!(matches!(
            unsafe_receipt.error,
            Some(BridgeError::Protocol(_))
        ));
        let mismatch = run_with_environment(
            &request,
            &helper,
            Some(&controlled_environment(
                &environment,
                terminal_receipt(&request, &target, 7),
                "private",
                "0",
            )),
        );
        assert!(matches!(mismatch.error, Some(BridgeError::Protocol(_))));
        assert!(mismatch.evidence.receipt.is_some() && mismatch.evidence.target.is_some());
    }

    #[test]
    fn controlled_receipts_reject_schema_types_and_i32_min_without_panicking() {
        let _lock = lock();
        let (directory, request, _log, environment) = actual("success", OutputMode::Capture);
        let helper = controlled_helper(&directory);
        let target = fs::canonicalize(directory.path().join("managed")).unwrap();
        let valid = terminal_receipt(&request, &target, 0);
        let malformed = run_with_environment(
            &request,
            &helper,
            Some(&controlled_environment_raw(
                &environment,
                OsString::from("not-json"),
                "private",
                "0",
            )),
        );
        assert!(
            matches!(&malformed.error, Some(BridgeError::Protocol(detail)) if detail.contains("malformed receipt"))
        );
        let mut wrong_schema = valid.clone();
        wrong_schema["schema_version"] = Value::String("wrong".into());
        let wrong_schema = run_with_environment(
            &request,
            &helper,
            Some(&controlled_environment(
                &environment,
                wrong_schema,
                "private",
                "0",
            )),
        );
        assert!(
            matches!(&wrong_schema.error, Some(BridgeError::Protocol(detail)) if detail.contains("requested completed"))
        );
        let mut missing_type = valid.clone();
        missing_type["outcome"]["cargo_exit_code"] = Value::String("seven".into());
        let missing_type = run_with_environment(
            &request,
            &helper,
            Some(&controlled_environment(
                &environment,
                missing_type,
                "private",
                "0",
            )),
        );
        assert!(matches!(missing_type.error, Some(BridgeError::Gate(_))));
        let mut minimum = valid;
        minimum["outcome"]["cargo_exit_code"] = Value::Number(i64::from(i32::MIN).into());
        let minimum = run_with_environment(
            &request,
            &helper,
            Some(&controlled_environment(
                &environment,
                minimum,
                "private",
                "0",
            )),
        );
        assert!(
            matches!(&minimum.error, Some(BridgeError::Protocol(detail)) if detail.contains("outside i32 range"))
        );
    }

    #[test]
    fn missing_python_is_a_typed_spawn_failure() {
        let _lock = lock();
        let (_directory, request, _log, environment) = actual("success", OutputMode::Capture);
        let result = run_with_environment(
            &request,
            Path::new("/definitely/missing/python3"),
            Some(&environment),
        );
        assert!(matches!(result.error, Some(BridgeError::Spawn(_))));
        assert!(result.evidence.invocation_dir.is_some());
    }
}
