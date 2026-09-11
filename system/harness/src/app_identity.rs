//! Shared macOS app-install callers. Signing and transaction rules live in Python.
//!
//! A missing policy is not permission to overwrite a previously signed app.
//! These checks coordinate trusted local installers, not hostile account owners.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const HEX_APP_ID: &str = "com.mrap.hex";
const POLICY_RELATIVE: &str = "Library/Application Support/Hex/build-signing/policy.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeIntelProduct {
    Cli,
    Daemon,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppProduct {
    Hex,
    CodeIntelCli,
    CodeIntelDaemon,
}

impl AppProduct {
    fn name(self) -> &'static str {
        match self {
            Self::Hex => "hex",
            Self::CodeIntelCli => "code-intel-cli",
            Self::CodeIntelDaemon => "code-intel-daemon",
        }
    }

    fn bundle_name(self) -> &'static str {
        match self {
            Self::Hex => "Hex.app",
            Self::CodeIntelCli => "CQ.app",
            Self::CodeIntelDaemon => "SCIPD.app",
        }
    }

    fn executable_name(self) -> &'static str {
        match self {
            Self::Hex => "hex",
            Self::CodeIntelCli => "cq",
            Self::CodeIntelDaemon => "scipd",
        }
    }

    fn bundle_identifier(self) -> &'static str {
        match self {
            Self::Hex => HEX_APP_ID,
            Self::CodeIntelCli => "com.mrap.hex.cq",
            Self::CodeIntelDaemon => "com.mrap.hex.scipd",
        }
    }

    fn helper_dir(self) -> &'static str {
        match self {
            Self::Hex => "libexec",
            Self::CodeIntelCli => "libexec/cq",
            Self::CodeIntelDaemon => "libexec/scipd",
        }
    }
}

impl From<CodeIntelProduct> for AppProduct {
    fn from(product: CodeIntelProduct) -> Self {
        match product {
            CodeIntelProduct::Cli => Self::CodeIntelCli,
            CodeIntelProduct::Daemon => Self::CodeIntelDaemon,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppPaths {
    product: AppProduct,
    pub bundle_identifier: &'static str,
    pub helper_dir: PathBuf,
    pub home: PathBuf,
    pub root: PathBuf,
    pub policy: PathBuf,
    pub app: PathBuf,
    pub executable: PathBuf,
    pub cli: PathBuf,
    pub state: PathBuf,
    pub lock: PathBuf,
    pub journal: PathBuf,
}

impl AppPaths {
    pub fn new(home: &Path, hex_dir: &Path) -> io::Result<Self> {
        Self::for_product(home, &hex_dir.join(".hex"), AppProduct::Hex)
    }

    fn for_product(home: &Path, root_path: &Path, product: AppProduct) -> io::Result<Self> {
        if !home.is_absolute() || !root_path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "app paths must be absolute",
            ));
        }
        let root = root_path.to_owned();
        let app = root.join(product.bundle_name());
        Ok(Self {
            product,
            bundle_identifier: product.bundle_identifier(),
            helper_dir: root.join(product.helper_dir()),
            home: home.to_owned(),
            policy: home.join(POLICY_RELATIVE),
            executable: app.join(format!("Contents/MacOS/{}", product.executable_name())),
            app,
            cli: root.join(format!("bin/{}", product.executable_name())),
            state: root.join(format!("{}.install-state.json", product.bundle_name())),
            lock: root.join(format!(".{}.app-install.lock", product.name())),
            journal: root.join(format!(".{}.app-install.journal.json", product.name())),
            root,
        })
    }

    pub fn code_intel(home: &Path, product: CodeIntelProduct) -> io::Result<Self> {
        Self::for_product(home, &home.join(".codeintel"), product.into())
    }

    /// Conservative admission only. The shared helper validates actual state.
    /// Dangling links and incomplete journals count as managed evidence.
    pub fn managed_evidence(&self) -> io::Result<bool> {
        for path in [&self.policy, &self.app, &self.state, &self.journal] {
            if entry_present(path)? {
                return Ok(true);
            }
        }
        if self.product == AppProduct::Hex {
            let alias = self.root.join("bin/hex-agent");
            match fs::symlink_metadata(&alias) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let target = fs::read_link(&alias)?;
                    // The old compatibility alias points to the raw CLI. A direct
                    // app alias is signed evidence even when the app is now absent.
                    if target != Path::new("hex") && target != self.cli {
                        return Ok(true);
                    }
                }
                Ok(metadata) if metadata.is_file() => {}
                Ok(_) => return Err(io::Error::other("unexpected Hex agent alias file type")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        match fs::symlink_metadata(&self.cli) {
            Ok(metadata) if metadata.file_type().is_symlink() => Ok(true),
            Ok(metadata) if metadata.is_file() => Ok(false),
            Ok(_) => Err(io::Error::other("unexpected Hex CLI file type")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

fn entry_present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

#[derive(Debug)]
struct AppIdentityError(String);
impl std::fmt::Display for AppIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "signed-app check: {}", self.0)
    }
}
impl std::error::Error for AppIdentityError {}

const OUTPUT_LIMIT: u64 = 64 * 1024;

async fn bounded_read(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, AppIdentityError> {
    let mut bytes = Vec::new();
    reader
        .take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| AppIdentityError(format!("verifier output read failed: {e}")))?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err(AppIdentityError("verifier output exceeds limit".into()));
    }
    Ok(bytes)
}

// The child has a new group and is intentionally not reaped before output is
// drained. Keeping its PID allocated prevents group-ID reuse during cleanup.
#[allow(unsafe_code)]
fn kill_unreaped_group(pid: u32) -> Result<(), AppIdentityError> {
    let pid = i32::try_from(pid)
        .map_err(|_| AppIdentityError("verifier PID exceeds platform range".into()))?;
    if pid <= 1 {
        return Err(AppIdentityError("invalid verifier process group".into()));
    }
    // SAFETY: this is the group ID assigned to our still-unreaped child.
    if unsafe { libc::kill(-pid, libc::SIGKILL) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(AppIdentityError(format!(
                "verifier group cleanup failed: {error}"
            )));
        }
    }
    Ok(())
}

struct VerifierChild {
    child: tokio::process::Child,
    pid: u32,
    armed: bool,
}

impl Drop for VerifierChild {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = kill_unreaped_group(self.pid) {
                eprintln!("signed-app cleanup on cancellation failed: {error}");
            }
        }
        // Child's kill_on_drop also requests direct-child cleanup/reaping.
    }
}

async fn run_verifier(
    program: &Path,
    args: &[&std::ffi::OsStr],
    home: &Path,
    lock_fd: Option<std::os::fd::RawFd>,
    timeout: Duration,
) -> Result<Vec<u8>, AppIdentityError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    if let Some(fd) = lock_fd {
        // Only the forked child inherits this cooperative product lock.
        // The parent descriptor keeps CLOEXEC and remains owned by its guard.
        #[allow(unsafe_code)]
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = command
        .spawn()
        .map_err(|e| AppIdentityError(format!("verifier launch failed: {e}")))?;
    let pid = child
        .id()
        .ok_or_else(|| AppIdentityError("verifier has no PID".into()))?;
    let mut process = VerifierChild {
        child,
        pid,
        armed: true,
    };
    let stdout = process
        .child
        .stdout
        .take()
        .ok_or_else(|| AppIdentityError("verifier stdout unavailable".into()))?;
    let stderr = process
        .child
        .stderr
        .take()
        .ok_or_else(|| AppIdentityError("verifier stderr unavailable".into()))?;
    let deadline = tokio::time::Instant::now() + timeout;
    let output = tokio::time::timeout_at(deadline, async {
        let (stdout, stderr) = tokio::try_join!(bounded_read(stdout), bounded_read(stderr))?;
        Ok::<_, AppIdentityError>((stdout, stderr))
    })
    .await;
    let output = match output {
        Ok(Ok(output)) => output,
        other => {
            let cleanup = kill_unreaped_group(pid);
            let reap = tokio::time::timeout(Duration::from_secs(5), process.child.wait()).await;
            if matches!(reap, Ok(Ok(_))) {
                process.armed = false;
            }
            cleanup?;
            if !matches!(reap, Ok(Ok(_))) {
                return Err(AppIdentityError(
                    "verifier cleanup could not reap child".into(),
                ));
            }
            return Err(match other {
                Ok(Err(error)) => error,
                _ => AppIdentityError("verifier timed out".into()),
            });
        }
    };
    let status = match tokio::time::timeout_at(deadline, process.child.wait()).await {
        Ok(Ok(status)) => {
            process.armed = false;
            status
        }
        other => {
            let cleanup = kill_unreaped_group(pid);
            let reap = tokio::time::timeout(Duration::from_secs(5), process.child.wait()).await;
            if matches!(reap, Ok(Ok(_))) {
                process.armed = false;
            }
            cleanup?;
            if !matches!(reap, Ok(Ok(_))) {
                return Err(AppIdentityError(
                    "verifier cleanup could not reap child".into(),
                ));
            }
            return Err(AppIdentityError(match other {
                Ok(Err(error)) => format!("verifier wait failed: {error}"),
                _ => "verifier timed out".into(),
            }));
        }
    };
    if !status.success() {
        // The helper only receives public paths/policy. Bound its diagnostic;
        // no inherited provider environment reaches this child.
        return Err(AppIdentityError(format!(
            "verifier failed ({status}): {}",
            String::from_utf8_lossy(&output.1)
        )));
    }
    if !output.1.is_empty() {
        return Err(AppIdentityError(
            "verifier returned unexpected stderr".into(),
        ));
    }
    Ok(output.0)
}

/// A dedicated short-lived runtime also works from an existing Tokio caller.
/// The join keeps borrowed lock ownership alive through the entire operation.
fn run_python(
    args: &[std::ffi::OsString],
    home: &Path,
    lock_fd: Option<std::os::fd::RawFd>,
    timeout: Duration,
) -> io::Result<Vec<u8>> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let argv = args
                    .iter()
                    .map(std::ffi::OsString::as_os_str)
                    .collect::<Vec<_>>();
                runtime
                    .block_on(run_verifier(
                        Path::new("/usr/bin/python3"),
                        &argv,
                        home,
                        lock_fd,
                        timeout,
                    ))
                    .map_err(io::Error::other)
            })
            .join()
            .map_err(|_| io::Error::other("app helper thread panicked"))?
    })
}

const BOOTSTRAP: &str = r#"import hashlib, os, stat, sys
root, helper_dir, signing_hash, install_hash = sys.argv[1:5]
contents = {}
for name, expected in [('macos-signing.py', signing_hash), ('macos-app-install.py', install_hash)]:
    path = os.path.join(helper_dir, name)
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as stream:
        mode = os.fstat(stream.fileno()).st_mode
        if not stat.S_ISREG(mode) or mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX):
            raise ValueError('helper must be a regular file without special mode bits')
        source = stream.read(1048577)
    if len(source) > 1048576 or hashlib.sha256(source).hexdigest() != expected:
        raise ValueError('installed helper provenance mismatch: ' + name)
    contents[name] = source
path = os.path.join(helper_dir, 'macos-app-install.py')
sys.argv = [path] + sys.argv[5:]
sys.path.insert(0, os.path.dirname(path))
globals()['__file__'] = path
exec(compile(contents['macos-app-install.py'], path, 'exec'), globals())
"#;

#[derive(serde::Deserialize)]
struct HelperState {
    helpers: Helpers,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Helpers {
    #[serde(rename = "macos-signing.py")]
    signing: HelperRecord,
    #[serde(rename = "macos-app-install.py")]
    install: HelperRecord,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperRecord {
    sha256: String,
    source_revision: String,
}

fn read_regular(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(format!(
            "not a regular file: {}",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other(format!(
            "file too large: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

fn valid_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64) && revision.bytes().all(|b| b.is_ascii_hexdigit())
}

fn check_helper(path: &Path, record: &HelperRecord) -> io::Result<()> {
    use sha2::{Digest, Sha256};
    if record.sha256.len() != 64
        || !record.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || !valid_revision(&record.source_revision)
    {
        return Err(io::Error::other("invalid installed helper provenance"));
    }
    let actual = format!("{:x}", Sha256::digest(read_regular(path, 1024 * 1024)?));
    if actual != record.sha256.to_ascii_lowercase() {
        return Err(io::Error::other(format!(
            "installed helper hash mismatch: {}",
            path.display()
        )));
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct OwnerResult {
    schema_version: u32,
    product: String,
    mode: String,
    executable_path: PathBuf,
    bundle_identifier: String,
    generation: String,
}

/// The guard is held across the caller's service operation.
pub struct VerifiedOwner {
    executable: PathBuf,
    signed: bool,
    _lock: Option<fs::File>,
}

impl VerifiedOwner {
    pub fn executable(&self) -> &Path {
        &self.executable
    }
    pub fn is_signed(&self) -> bool {
        self.signed
    }

    #[cfg(test)]
    pub(crate) fn fixture(executable: PathBuf, signed: bool) -> Self {
        Self {
            executable,
            signed,
            _lock: None,
        }
    }
}

fn verified_owner_at(paths: AppPaths) -> io::Result<VerifiedOwner> {
    use fs2::FileExt;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    if !paths.managed_evidence()? {
        return Ok(VerifiedOwner {
            executable: paths.cli,
            signed: false,
            _lock: None,
        });
    }
    // Service operations do not create installation directories or repair state.
    let mut directories = vec![paths.root.clone(), paths.root.join("bin")];
    let mut helper_parent = paths.root.clone();
    for component in paths
        .helper_dir
        .strip_prefix(&paths.root)
        .map_err(io::Error::other)?
        .components()
    {
        helper_parent.push(component);
        directories.push(helper_parent.clone());
    }
    for dir in directories {
        if !fs::symlink_metadata(dir)?.is_dir() {
            return Err(io::Error::other(
                "signed app parent is not a real directory",
            ));
        }
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&paths.lock)?;
    if !lock.metadata()?.is_file() {
        return Err(io::Error::other("invalid app install lock"));
    }
    lock.try_lock_exclusive()
        .map_err(|e| io::Error::new(e.kind(), format!("app install busy or unavailable: {e}")))?;
    if entry_present(&paths.journal)? {
        return Err(io::Error::other(
            "incomplete app install blocks service changes",
        ));
    }
    if !entry_present(&paths.policy)? {
        return Err(io::Error::other(
            "central signing policy missing; service registration is unchanged",
        ));
    }
    let provenance: HelperState = serde_json::from_slice(&read_regular(&paths.state, 128 * 1024)?)
        .map_err(io::Error::other)?;
    let helper = paths.helper_dir.join("macos-app-install.py");
    check_helper(&helper, &provenance.helpers.install)?;
    check_helper(
        &paths.helper_dir.join("macos-signing.py"),
        &provenance.helpers.signing,
    )?;
    let args = vec![
        "-I".into(),
        "-B".into(),
        "-c".into(),
        BOOTSTRAP.into(),
        paths.root.clone().into_os_string(),
        paths.helper_dir.clone().into_os_string(),
        provenance.helpers.signing.sha256.into(),
        provenance.helpers.install.sha256.into(),
        "service-owner".into(),
        paths.product.name().into(),
        "--root".into(),
        paths.root.clone().into_os_string(),
        "--lock-fd".into(),
        lock.as_raw_fd().to_string().into(),
    ];
    let bytes = run_python(
        &args,
        &paths.home,
        Some(lock.as_raw_fd()),
        Duration::from_secs(120),
    )?;
    let owner: OwnerResult = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if owner.schema_version != 1
        || owner.product != paths.product.name()
        || owner.mode != "signed-current"
        || owner.executable_path != paths.executable
        || owner.bundle_identifier != paths.bundle_identifier
        || owner.generation.is_empty()
    {
        return Err(io::Error::other(
            "shared helper returned an unexpected service owner",
        ));
    }
    Ok(VerifiedOwner {
        executable: paths.executable,
        signed: true,
        _lock: Some(lock),
    })
}

pub fn verified_owner(hex_dir: &Path) -> io::Result<VerifiedOwner> {
    if !cfg!(target_os = "macos") {
        return Ok(VerifiedOwner {
            executable: hex_dir.join(".hex/bin/hex"),
            signed: false,
            _lock: None,
        });
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("HOME is required for the shared signing policy"))?;
    verified_owner_at(AppPaths::new(&home, hex_dir)?)
}

pub fn verified_codeintel_owner(
    product: CodeIntelProduct,
    codeintel_dir: &Path,
) -> io::Result<VerifiedOwner> {
    if !cfg!(target_os = "macos") {
        return Ok(VerifiedOwner {
            executable: codeintel_dir.join(match product {
                CodeIntelProduct::Cli => "bin/cq",
                CodeIntelProduct::Daemon => "bin/scipd",
            }),
            signed: false,
            _lock: None,
        });
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("HOME is required for the shared signing policy"))?;
    verified_owner_at(AppPaths::for_product(&home, codeintel_dir, product.into())?)
}

#[derive(Debug, PartialEq, Eq)]
pub enum UpgradeMode {
    Legacy,
    Migrate,
    Signed(String),
}

fn current_paths(hex_dir: &Path) -> io::Result<AppPaths> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("HOME is required for the shared signing policy"))?;
    AppPaths::new(&home, hex_dir)
}

#[derive(serde::Deserialize)]
struct ModeResult {
    mode: String,
    source_revision: Option<String>,
    policy_available: Option<bool>,
    managed: Option<bool>,
}

fn source_command<T: serde::de::DeserializeOwned>(
    paths: &AppPaths,
    source_dir: &Path,
    command: &str,
    extra: &[std::ffi::OsString],
) -> io::Result<T> {
    let helper = source_dir.join("system/scripts/macos-app-install.py");
    // Selected Foundation source is the updater's trust input. Do not substitute
    // an older installed helper or an executable found through PATH.
    read_regular(&helper, 1024 * 1024)?;
    read_regular(
        &source_dir.join("system/scripts/macos-signing.py"),
        1024 * 1024,
    )?;
    let mut args = vec![
        "-I".into(),
        "-B".into(),
        helper.into_os_string(),
        command.into(),
        paths.product.name().into(),
        "--root".into(),
        paths.root.clone().into_os_string(),
    ];
    args.extend_from_slice(extra);
    let output = run_python(&args, &paths.home, None, Duration::from_secs(300))?;
    let result: serde_json::Value = serde_json::from_slice(&output).map_err(io::Error::other)?;
    if result.get("schema_version").and_then(|v| v.as_u64()) != Some(1)
        || result.get("product").and_then(|v| v.as_str()) != Some(paths.product.name())
    {
        return Err(io::Error::other("unexpected app-installer result"));
    }
    serde_json::from_value(result).map_err(io::Error::other)
}

pub fn prepare_upgrade(hex_dir: &Path, source_dir: &Path) -> io::Result<UpgradeMode> {
    if !cfg!(target_os = "macos") {
        return Ok(UpgradeMode::Legacy);
    }
    let paths = current_paths(hex_dir)?;
    prepare_upgrade_at(&paths, source_dir)
}

fn prepare_upgrade_at(paths: &AppPaths, source_dir: &Path) -> io::Result<UpgradeMode> {
    if !paths.managed_evidence()? {
        return Ok(UpgradeMode::Legacy);
    }
    let result: ModeResult = source_command(paths, source_dir, "preflight", &[])?;
    if result.policy_available != Some(true) || result.managed != Some(true) {
        return Err(io::Error::other(
            "managed preflight did not verify the signing policy",
        ));
    }
    match result.mode.as_str() {
        "configured-legacy" | "empty" => {
            if !entry_present(&paths.policy)? {
                return Err(io::Error::other(
                    "signing policy disappeared during preflight",
                ));
            }
            Ok(UpgradeMode::Migrate)
        }
        "signed-current" => {
            let revision = result
                .source_revision
                .filter(|s| valid_revision(s))
                .ok_or_else(|| {
                    io::Error::other("verified installation lacks product source revision")
                })?;
            Ok(UpgradeMode::Signed(revision))
        }
        mode => Err(io::Error::other(format!(
            "app state blocks upgrade: {mode}"
        ))),
    }
}

pub fn install_build(
    hex_dir: &Path,
    source_dir: &Path,
    executable: &Path,
    version: &str,
    revision: &str,
) -> io::Result<()> {
    let paths = current_paths(hex_dir)?;
    let extra = vec![
        "--source".into(),
        executable.as_os_str().to_owned(),
        "--version".into(),
        version.into(),
        "--source-revision".into(),
        revision.into(),
        "--helper-source-revision".into(),
        revision.into(),
    ];
    let result: ModeResult = source_command(&paths, source_dir, "install", &extra)?;
    if result.mode != "signed-current" || result.source_revision.as_deref() != Some(revision) {
        return Err(io::Error::other(
            "app installer did not commit the requested signed source",
        ));
    }
    Ok(())
}

pub fn prepare_codeintel_upgrade(
    product: CodeIntelProduct,
    codeintel_dir: &Path,
    source_dir: &Path,
) -> io::Result<UpgradeMode> {
    if !cfg!(target_os = "macos") {
        return Ok(UpgradeMode::Legacy);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("HOME is required for the shared signing policy"))?;
    let paths = AppPaths::for_product(&home, codeintel_dir, product.into())?;
    prepare_upgrade_at(&paths, source_dir)
}

pub fn install_codeintel_build(
    product: CodeIntelProduct,
    codeintel_dir: &Path,
    source_dir: &Path,
    executable: &Path,
    version: &str,
    revision: &str,
) -> io::Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("HOME is required for the shared signing policy"))?;
    let paths = AppPaths::for_product(&home, codeintel_dir, product.into())?;
    let extra = vec![
        "--source".into(),
        executable.as_os_str().to_owned(),
        "--version".into(),
        version.into(),
        "--source-revision".into(),
        revision.into(),
        "--helper-source-revision".into(),
        revision.into(),
    ];
    let result: ModeResult = source_command(&paths, source_dir, "install", &extra)?;
    if result.mode != "signed-current" || result.source_revision.as_deref() != Some(revision) {
        return Err(io::Error::other(
            "app installer did not commit the requested signed source",
        ));
    }
    Ok(())
}

/// Inspect or repair only the fixed Hex compatibility command for this product.
/// The shared helper owns verification, locking, backup and publication.
pub fn reconcile_codeintel_alias(
    product: CodeIntelProduct,
    hex_dir: &Path,
    source_dir: &Path,
    dry_run: bool,
) -> io::Result<bool> {
    #[derive(serde::Deserialize)]
    struct AliasResult {
        source_revision: String,
        generation: String,
        alias_path: PathBuf,
        target_path: PathBuf,
        action: String,
        changed: bool,
        published: bool,
        archive_path: Option<PathBuf>,
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("HOME is required"))?;
    let paths = AppPaths::code_intel(&home, product)?;
    let mut extra = vec!["--hex-workspace".into(), hex_dir.as_os_str().to_owned()];
    if dry_run {
        extra.push("--dry-run".into());
    }
    let result: AliasResult = source_command(&paths, source_dir, "compatibility-alias", &extra)?;
    let pending = matches!(result.action.as_str(), "would-create" | "would-migrate");
    let changed = matches!(result.action.as_str(), "created" | "migrated");
    let valid_action = if dry_run {
        (pending || result.action == "current") && !result.changed && !result.published
    } else {
        (changed || result.action == "current")
            && result.changed == changed
            && result.published == changed
    };
    if !valid_action
        || !valid_revision(&result.source_revision)
        || result.generation.is_empty()
        || result.alias_path
            != hex_dir
                .join(".hex/bin")
                .join(paths.product.executable_name())
        || result.target_path != paths.cli
        || (result.action == "migrated" && result.archive_path.is_none())
    {
        return Err(io::Error::other("invalid compatibility-alias result"));
    }
    Ok(pending || changed)
}

#[derive(Debug, Default)]
pub struct CodeIntelServiceChange {
    pub needed: bool,
    /// A validated interrupted reload must finish before replacing its app.
    pub recovery_pending: bool,
}

/// Reconcile only the already declared code-intel service. No new service is enabled.
pub fn reconcile_codeintel_service(
    source_dir: &Path,
    dry_run: bool,
) -> io::Result<CodeIntelServiceChange> {
    #[derive(serde::Deserialize)]
    struct ServiceResult {
        mode: String,
        service_action: String,
        service_needs_change: bool,
        service_recovery_pending: bool,
        published: bool,
        plist_path: PathBuf,
        executable_path: PathBuf,
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("HOME is required"))?;
    let paths = AppPaths::code_intel(&home, CodeIntelProduct::Daemon)?;
    let extra = if dry_run {
        vec!["--dry-run".into()]
    } else {
        vec![]
    };
    let result: ServiceResult = source_command(&paths, source_dir, "service-reconcile", &extra)?;
    let pending = matches!(
        result.service_action.as_str(),
        "would-restart" | "would-update-stopped"
    );
    let changed = matches!(
        result.service_action.as_str(),
        "restarted" | "recovered" | "updated-stopped"
    );
    let current = matches!(
        result.service_action.as_str(),
        "loaded" | "stopped" | "absent"
    );
    let valid_action = if dry_run {
        (pending || current) && result.service_needs_change == pending && !result.published
    } else {
        (changed || current)
            && result.service_needs_change == changed
            && result.published == changed
    };
    if !valid_action
        || result.mode != "signed-current"
        || result.plist_path != home.join("Library/LaunchAgents/com.hex.scipd.plist")
        || result.executable_path != paths.executable
    {
        return Err(io::Error::other("invalid code-intel service result"));
    }
    Ok(CodeIntelServiceChange {
        needed: result.service_needs_change,
        recovery_pending: result.service_recovery_pending,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn hex_workspace_name_does_not_change_its_runtime_root() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join(".hex");
        assert_eq!(
            AppPaths::new(temp.path(), &workspace).unwrap().root,
            workspace.join(".hex")
        );
    }

    #[test]
    fn codeintel_helper_parent_alias_is_rejected_before_execution() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::code_intel(temp.path(), CodeIntelProduct::Cli).unwrap();
        fixture(&paths, "print(json.dumps(result))");
        fs::rename(paths.root.join("libexec"), paths.root.join("helper-store")).unwrap();
        symlink("helper-store", paths.root.join("libexec")).unwrap();
        assert!(verified_owner_at(paths.clone()).is_err());
        assert!(!paths.root.join("called").exists());
    }

    #[test]
    fn raw_legacy_is_distinct_from_every_signed_evidence_path() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(temp.path(), &temp.path().join("hex")).unwrap();
        assert!(!paths.managed_evidence().unwrap());
        fs::create_dir_all(paths.cli.parent().unwrap()).unwrap();
        fs::write(&paths.cli, b"legacy raw").unwrap();
        assert!(!paths.managed_evidence().unwrap());
        for path in [&paths.policy, &paths.app, &paths.state, &paths.journal] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            symlink("missing", path).unwrap();
            assert!(paths.managed_evidence().unwrap(), "{}", path.display());
            fs::remove_file(path).unwrap();
        }
        fs::remove_file(&paths.cli).unwrap();
        symlink(&paths.executable, &paths.cli).unwrap();
        assert!(paths.managed_evidence().unwrap());
    }

    #[test]
    fn orphan_signed_alias_cannot_downgrade_to_raw_legacy() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(temp.path(), &temp.path().join("hex")).unwrap();
        fs::create_dir_all(paths.cli.parent().unwrap()).unwrap();
        let alias = paths.root.join("bin/hex-agent");
        for raw_cli in [false, true] {
            if raw_cli {
                fs::write(&paths.cli, b"raw legacy").unwrap();
            }
            for target in [Path::new("hex"), paths.cli.as_path()] {
                symlink(target, &alias).unwrap();
                assert!(!paths.managed_evidence().unwrap());
                fs::remove_file(&alias).unwrap();
            }
            for target in [
                Path::new("../Hex.app/Contents/MacOS/hex"),
                paths.executable.as_path(),
            ] {
                symlink(target, &alias).unwrap();
                assert!(paths.managed_evidence().unwrap());
                assert!(prepare_upgrade_at(&paths, &temp.path().join("source")).is_err());
                fs::remove_file(&alias).unwrap();
            }
        }
    }

    #[test]
    fn revision_format_matches_shared_contract() {
        for length in [39, 40, 41, 63, 64, 65] {
            assert_eq!(
                valid_revision(&"a".repeat(length)),
                matches!(length, 40 | 64)
            );
        }
        assert!(!valid_revision(&"z".repeat(40)));
    }

    #[test]
    fn invalid_cli_type_is_not_legacy() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(temp.path(), &temp.path().join("hex")).unwrap();
        fs::create_dir_all(&paths.cli).unwrap();
        assert!(paths.managed_evidence().is_err());
        assert!(AppPaths::new(Path::new("relative"), temp.path()).is_err());
    }

    fn fixture(paths: &AppPaths, behavior: &str) {
        fixture_for(paths, paths.product.name(), behavior)
    }

    fn fixture_for(paths: &AppPaths, product: &str, behavior: &str) {
        use sha2::{Digest, Sha256};
        fs::create_dir_all(&paths.helper_dir).unwrap();
        fs::create_dir_all(paths.cli.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.policy.parent().unwrap()).unwrap();
        fs::write(&paths.policy, b"fixture public policy").unwrap();
        fs::write(&paths.lock, b"").unwrap();
        let script = format!(
            r#"import sys, os, json, fcntl
from pathlib import Path
root=Path(sys.argv[sys.argv.index('--root')+1])
fd=int(sys.argv[sys.argv.index('--lock-fd')+1])
assert sys.argv[1:3] == ['service-owner','{product}']
assert os.fstat(fd).st_ino == os.stat(root/'{lock}').st_ino
assert Path(__file__).parent == Path('{helper_dir}')
with open(root/'{lock}','rb') as other:
    try: fcntl.flock(other.fileno(),fcntl.LOCK_EX|fcntl.LOCK_NB)
    except BlockingIOError: pass
    else: raise AssertionError('parent lock not held')
(root/'called').write_text('yes')
result={{'schema_version':1,'product':'{product}','mode':'signed-current','executable_path':str(Path('{executable}')),'bundle_identifier':'{bundle_identifier}','generation':'generation-a'}}
{behavior}
"#,
            product = product,
            lock = paths.lock.file_name().unwrap().to_str().unwrap(),
            helper_dir = paths.helper_dir.display(),
            executable = paths.executable.display(),
            bundle_identifier = paths.bundle_identifier,
        );
        let mut helpers = serde_json::Map::new();
        for (name, content) in [
            ("macos-app-install.py", script.as_bytes()),
            ("macos-signing.py", b"# fixture only\n".as_slice()),
        ] {
            fs::write(paths.helper_dir.join(name), content).unwrap();
            helpers.insert(name.into(), serde_json::json!({"sha256":format!("{:x}",Sha256::digest(content)),"source_revision":"a".repeat(40)}));
        }
        fs::write(
            &paths.state,
            serde_json::to_vec(&serde_json::json!({"helpers":helpers})).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn actual_helper_admission_holds_lock_through_owner_lifetime() {
        const CHILD: &str = "HEX_APP_OWNER_LOCK_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Another parallel test can fork while the owner holds its flock.
            // CLOEXEC closes the inherited descriptor at exec, not at fork.
            // Isolate this immediate-release assertion from unrelated forks;
            // the real helper and both lock assertions still run below.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app_identity::tests::actual_helper_admission_holds_lock_through_owner_lifetime",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "isolated lock test failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
            return;
        }
        use fs2::FileExt;
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(temp.path(), &temp.path().join("hex")).unwrap();
        fixture(&paths, "print(json.dumps(result))");
        let owner = verified_owner_at(paths.clone()).unwrap();
        assert!(owner.is_signed());
        assert_eq!(owner.executable(), paths.executable);
        let another = fs::File::open(&paths.lock).unwrap();
        assert!(another.try_lock_exclusive().is_err());
        drop(owner);
        another.try_lock_exclusive().unwrap();
    }

    #[test]
    fn codeintel_products_have_distinct_fixed_paths_and_admit_real_helper_shape() {
        for product in [CodeIntelProduct::Cli, CodeIntelProduct::Daemon] {
            let temp = tempfile::tempdir().unwrap();
            let paths = AppPaths::code_intel(temp.path(), product).unwrap();
            fixture_for(&paths, paths.product.name(), "print(json.dumps(result))");
            let owner = verified_owner_at(paths.clone()).unwrap();
            assert!(owner.is_signed());
            assert_eq!(owner.executable(), paths.executable);
            assert!(paths.helper_dir.ends_with(match product {
                CodeIntelProduct::Cli => "libexec/cq",
                CodeIntelProduct::Daemon => "libexec/scipd",
            }));
            assert_eq!(
                paths.bundle_identifier,
                match product {
                    CodeIntelProduct::Cli => "com.mrap.hex.cq",
                    CodeIntelProduct::Daemon => "com.mrap.hex.scipd",
                }
            );
        }
    }

    #[test]
    fn codeintel_admission_rejects_wrong_identity_product_helper_dir_and_changed_helper() {
        for defect in ["identity", "product", "helper-dir", "changed-helper"] {
            let temp = tempfile::tempdir().unwrap();
            let paths = AppPaths::code_intel(temp.path(), CodeIntelProduct::Cli).unwrap();
            let behavior = if defect == "identity" {
                "result['bundle_identifier']='com.mrap.hex';print(json.dumps(result))"
            } else {
                "print(json.dumps(result))"
            };
            fixture_for(
                &paths,
                if defect == "product" {
                    "hex"
                } else {
                    paths.product.name()
                },
                behavior,
            );
            if defect == "helper-dir" {
                fs::rename(&paths.helper_dir, paths.root.join("libexec/wrong")).unwrap();
            } else if defect == "changed-helper" {
                fs::write(paths.helper_dir.join("macos-signing.py"), b"changed").unwrap();
            }
            assert!(verified_owner_at(paths).is_err(), "{defect}");
        }
    }

    #[test]
    fn bad_provenance_policy_or_journal_never_executes_helper() {
        for defect in ["hash", "policy", "journal", "state-symlink"] {
            let temp = tempfile::tempdir().unwrap();
            let paths = AppPaths::new(temp.path(), &temp.path().join("hex")).unwrap();
            fixture(&paths, "print(json.dumps(result))");
            match defect {
                "hash" => {
                    fs::write(paths.root.join("libexec/macos-signing.py"), b"changed").unwrap()
                }
                "policy" => fs::remove_file(&paths.policy).unwrap(),
                "journal" => fs::write(&paths.journal, b"incomplete").unwrap(),
                _ => {
                    fs::remove_file(&paths.state).unwrap();
                    symlink("missing", &paths.state).unwrap();
                }
            }
            assert!(verified_owner_at(paths.clone()).is_err(), "{defect}");
            assert!(!paths.root.join("called").exists(), "{defect}");
        }
    }

    #[test]
    fn actual_helper_wrong_duplicate_or_failed_result_cannot_admit() {
        for behavior in [
            "result['product']='boi';print(json.dumps(result))",
            "print('{\"schema_version\":1,\"schema_version\":1}')",
            "sys.exit(7)",
            "print(json.dumps(result));print('failure',file=sys.stderr)",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let paths = AppPaths::new(temp.path(), &temp.path().join("hex")).unwrap();
            fixture(&paths, behavior);
            assert!(verified_owner_at(paths).is_err(), "{behavior}");
        }
    }

    #[test]
    fn bounded_runner_rejects_overflow_and_a_sleeping_child() {
        let temp = tempfile::tempdir().unwrap();
        for code in [
            "import os; os.write(1,b'x'*100000)",
            "import os; os.write(2,b'x'*100000)",
            "import time; time.sleep(10)",
        ] {
            let args = ["-I".into(), "-B".into(), "-c".into(), code.into()];
            let started = std::time::Instant::now();
            assert!(run_python(&args, temp.path(), None, Duration::from_millis(200)).is_err());
            assert!(started.elapsed() < Duration::from_secs(3));
        }
    }

    #[test]
    fn managed_preflight_never_accepts_raw_or_wrong_source_result() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(temp.path(), &temp.path().join("hex")).unwrap();
        fs::create_dir_all(paths.policy.parent().unwrap()).unwrap();
        fs::write(&paths.policy, b"fixture public policy").unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("system/scripts")).unwrap();
        let helper = source.join("system/scripts/macos-app-install.py");
        fs::write(source.join("system/scripts/macos-signing.py"), b"# fixture").unwrap();
        for response in [
            serde_json::json!({"schema_version":1,"product":"hex","mode":"legacy-raw","managed":true,"policy_available":true}),
            serde_json::json!({"schema_version":1,"product":"boi","mode":"signed-current","source_revision":"a".repeat(40),"managed":true,"policy_available":true}),
            serde_json::json!({"schema_version":1,"product":"hex","mode":"signed-current","source_revision":"release:v1","managed":true,"policy_available":true}),
            serde_json::json!({"schema_version":1,"product":"hex","mode":"signed-current","source_revision":"a".repeat(40)}),
            serde_json::json!({"schema_version":1,"product":"hex","mode":"empty","managed":"true","policy_available":true}),
            serde_json::json!({"schema_version":1,"product":"hex","mode":"empty","managed":true,"policy_available":false}),
        ] {
            fs::write(&helper, format!("print({:?})\n", response.to_string())).unwrap();
            assert!(prepare_upgrade_at(&paths, &source).is_err(), "{response}");
        }
        for (mode, expected) in [
            ("empty", UpgradeMode::Migrate),
            ("configured-legacy", UpgradeMode::Migrate),
            ("signed-current", UpgradeMode::Signed("a".repeat(40))),
        ] {
            let response = serde_json::json!({"schema_version":1,"product":"hex","mode":mode,"source_revision":"a".repeat(40),"managed":true,"policy_available":true});
            fs::write(&helper, format!("print({:?})\n", response.to_string())).unwrap();
            assert_eq!(prepare_upgrade_at(&paths, &source).unwrap(), expected);
        }
        fs::remove_file(&helper).unwrap();
        assert!(prepare_upgrade_at(&paths, &source).is_err());
        fs::remove_file(&paths.policy).unwrap();
        assert_eq!(
            prepare_upgrade_at(&paths, &source).unwrap(),
            UpgradeMode::Legacy
        );
    }
}
