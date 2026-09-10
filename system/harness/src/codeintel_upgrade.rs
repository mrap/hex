//! Independent, resumable signed code-intel step in `hex upgrade`.
//! Publication and service ownership stay in the shared installer.

use hex::app_identity::{self, CodeIntelProduct as Product, CodeIntelServiceChange, UpgradeMode};
use hex::managed_cargo_bridge::{
    self, Caller, CargoStatus, OutputMode, ReceiptLocation, Request, SourceState,
};
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const PRODUCTS: [Product; 2] = [Product::Cli, Product::Daemon];
const BUILD_INPUTS: [&str; 8] = [
    "Cargo.toml",
    "Cargo.lock",
    ".cargo",
    "system/code-intel",
    ":(exclude)system/code-intel/target",
    ":(exclude)system/code-intel/tests/fixtures",
    "system/scripts/macos-signing.py",
    "system/scripts/macos-app-install.py",
];

fn name(product: Product) -> &'static str {
    match product {
        Product::Cli => "cq",
        Product::Daemon => "scipd",
    }
}

#[derive(Debug, Default)]
pub(crate) struct Plan {
    revision: Option<String>,
    publish: [bool; 2],
    alias: [bool; 2],
    service: bool,
    recover_before_publish: bool,
}

impl Plan {
    pub(crate) fn needs_work(&self) -> bool {
        self.publish.iter().chain(&self.alias).any(|v| *v) || self.service
    }
}

trait Operations {
    fn mode(&self, product: Product) -> io::Result<UpgradeMode>;
    fn source(&self) -> io::Result<String>;
    fn alias(&self, product: Product, dry_run: bool) -> io::Result<bool>;
    fn service(&self, dry_run: bool) -> io::Result<CodeIntelServiceChange>;
    fn build(&self) -> io::Result<Build>;
    fn publish(&self, product: Product, build: &Build, revision: &str) -> io::Result<()>;
}

struct Build {
    output: PathBuf,
    version: String,
    // The bridge allocates this target and its receipt under a checked boundary.
    // Keep their locations for failure recovery and operator evidence.
    _evidence: Option<managed_cargo_bridge::Evidence>,
}

fn inspect_with(ops: &impl Operations) -> io::Result<Plan> {
    let modes = [ops.mode(Product::Cli)?, ops.mode(Product::Daemon)?];
    if modes.iter().all(|mode| *mode == UpgradeMode::Legacy) {
        return Ok(Plan::default());
    }
    if modes.contains(&UpgradeMode::Legacy) {
        return Err(io::Error::other("code-intel signing state is inconsistent"));
    }
    let revision = ops.source()?;
    let mut plan = Plan {
        revision: Some(revision.clone()),
        ..Plan::default()
    };
    for (index, (product, mode)) in PRODUCTS.into_iter().zip(modes).enumerate() {
        plan.publish[index] = mode != UpgradeMode::Signed(revision.clone());
        plan.alias[index] = if matches!(mode, UpgradeMode::Signed(_)) {
            ops.alias(product, true)?
        } else {
            true
        };
    }
    // Inspect an existing signed service even when its app will be updated.
    // Invalid service state must not hide behind a pending compilation.
    plan.service = if matches!(ops.mode(Product::Daemon)?, UpgradeMode::Signed(_)) {
        let service = ops.service(true)?;
        plan.recover_before_publish = service.recovery_pending && plan.publish[1];
        service.needed || plan.publish[1]
    } else {
        true
    };
    Ok(plan)
}

fn apply_with(ops: &impl Operations, previous: &Plan) -> io::Result<()> {
    let plan = inspect_with(ops)?;
    if plan.revision != previous.revision {
        return Err(io::Error::other(
            "code-intel source or signing state changed after preflight",
        ));
    }
    if plan.revision.is_none() {
        return Ok(());
    }
    let revision = plan
        .revision
        .as_deref()
        .ok_or_else(|| io::Error::other("missing managed source"))?;
    if plan.recover_before_publish {
        ops.service(false)?;
        println!("  [OK] Interrupted code-intel service reload completed before app replacement.");
    }
    let build = if plan.publish.iter().any(|v| *v) {
        Some(ops.build()?)
    } else {
        None
    };
    for (index, product) in PRODUCTS.into_iter().enumerate() {
        if plan.publish[index] {
            if ops.source()? != revision || ops.mode(product)? == UpgradeMode::Legacy {
                return Err(io::Error::other(
                    "code-intel inputs or signing state changed before publication",
                ));
            }
            ops.publish(
                product,
                build
                    .as_ref()
                    .ok_or_else(|| io::Error::other("missing build"))?,
                revision,
            )?;
            println!("  [OK] {} signed app installed.", name(product));
        }
        if plan.alias[index] || plan.publish[index] {
            if ops.source()? != revision {
                return Err(io::Error::other("source changed before alias repair"));
            }
            ops.alias(product, false)?;
            println!("  [OK] {} command path verified.", name(product));
        }
    }
    if plan.service {
        if ops.source()? != revision {
            return Err(io::Error::other("source changed before service repair"));
        }
        ops.service(false)?;
        println!("  [OK] Existing code-intel service reconciled.");
    }
    Ok(())
}

struct Native<'a> {
    hex_dir: &'a Path,
    source_dir: &'a Path,
    home: PathBuf,
}

impl<'a> Native<'a> {
    fn new(hex_dir: &'a Path, source_dir: &'a Path) -> io::Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("HOME is required for code-intel signing"))?;
        Ok(Self {
            hex_dir,
            source_dir,
            home,
        })
    }
}

fn git_output(source: &Path, args: &[&str]) -> io::Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "code-intel Git inspection failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output.stdout)
}

fn source_revision(source: &Path) -> io::Result<String> {
    if !source.join("system/code-intel/Cargo.toml").is_file() {
        return Err(io::Error::other(
            "managed code-intel requires its source manifest",
        ));
    }
    let mut args = vec![
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--ignored=matching",
        "--",
    ];
    args.extend(BUILD_INPUTS);
    if !git_output(source, &args)?.is_empty() {
        return Err(io::Error::other(
            "code-intel build inputs or signing helpers have uncommitted changes",
        ));
    }
    let revision = String::from_utf8(git_output(source, &["rev-parse", "--verify", "HEAD"])?)
        .map_err(io::Error::other)?;
    let revision = revision.trim();
    if !matches!(revision.len(), 40 | 64) || !revision.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(io::Error::other("invalid code-intel source revision"));
    }
    Ok(revision.to_owned())
}

fn cargo_version(source: &Path) -> io::Result<String> {
    let value: toml::Value = fs::read_to_string(source.join("system/code-intel/Cargo.toml"))?
        .parse()
        .map_err(io::Error::other)?;
    value
        .get("package")
        .and_then(|v| v.get("version"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("code-intel package version is missing"))
}

impl Operations for Native<'_> {
    fn mode(&self, product: Product) -> io::Result<UpgradeMode> {
        app_identity::prepare_codeintel_upgrade(
            product,
            &self.home.join(".codeintel"),
            self.source_dir,
        )
    }
    fn source(&self) -> io::Result<String> {
        source_revision(self.source_dir)
    }
    fn alias(&self, product: Product, dry_run: bool) -> io::Result<bool> {
        app_identity::reconcile_codeintel_alias(product, self.hex_dir, self.source_dir, dry_run)
    }
    fn service(&self, dry_run: bool) -> io::Result<CodeIntelServiceChange> {
        app_identity::reconcile_codeintel_service(self.source_dir, dry_run)
    }
    fn build(&self) -> io::Result<Build> {
        let version = cargo_version(self.source_dir)?;
        let revision = source_revision(self.source_dir)?;
        let host_output = Command::new("rustc").arg("-vV").output()?;
        if !host_output.status.success() {
            return Err(io::Error::other("rustc host inspection failed"));
        }
        let host_text = String::from_utf8(host_output.stdout).map_err(io::Error::other)?;
        let host = host_text
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .filter(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            })
            .ok_or_else(|| io::Error::other("rustc host triple is invalid"))?;
        let receipt = ReceiptLocation::new(
            fs::canonicalize(self.hex_dir)?,
            vec![".hex".into(), "managed-cargo-receipts".into()],
        )
        .map_err(|error| {
            io::Error::other(format!("invalid code-intel receipt location: {error:?}"))
        })?;
        let result = managed_cargo_bridge::run(&Request {
            caller: Caller::CodeIntelBuild,
            source_revision: revision,
            source_state: SourceState::Clean,
            working_repo: fs::canonicalize(self.source_dir)?,
            cargo_args: vec![
                "--locked".into(),
                "--release".into(),
                "--package".into(),
                "scipd".into(),
                "--bin".into(),
                "cq".into(),
                "--bin".into(),
                "scipd".into(),
                "--target".into(),
                host.into(),
            ],
            receipt,
            output: OutputMode::Inherit,
        });
        if let Some(error) = &result.error {
            return Err(io::Error::other(format!(
                "code-intel managed Cargo bridge failed: {error:?}; retained evidence: {}",
                managed_evidence(&result)
            )));
        }
        match result.cargo {
            Some(CargoStatus::Exit(0)) => {}
            Some(CargoStatus::Exit(code)) => {
                return Err(io::Error::other(format!(
                    "code-intel Cargo build failed (exit {code}); retained evidence: {}",
                    managed_evidence(&result)
                )))
            }
            Some(CargoStatus::Signal(signal)) => {
                return Err(io::Error::other(format!(
                    "code-intel Cargo build terminated by signal {signal}; retained evidence: {}",
                    managed_evidence(&result)
                )))
            }
            None => {
                return Err(io::Error::other(format!(
                "code-intel managed Cargo bridge returned no Cargo status; retained evidence: {}",
                managed_evidence(&result)
            )))
            }
        }
        let target = result.evidence.target.clone().ok_or_else(|| {
            io::Error::other(format!(
                "code-intel managed Cargo bridge omitted target evidence; retained evidence: {}",
                managed_evidence(&result)
            ))
        })?;
        let output = target.join(host).join("release");
        for product in PRODUCTS {
            let candidate = output.join(name(product));
            let metadata = fs::symlink_metadata(&candidate).map_err(|error| {
                io::Error::other(format!(
                    "code-intel build did not produce the exact executable at {}: {error}; retained evidence: {}",
                    candidate.display(),
                    managed_evidence(&result)
                ))
            })?;
            if !metadata.file_type().is_file() || metadata.len() == 0 || !executable(&metadata) {
                return Err(io::Error::other(format!(
                    "code-intel build did not produce the exact executable; retained evidence: {}",
                    managed_evidence(&result)
                )));
            }
        }
        Ok(Build {
            output,
            version,
            _evidence: Some(result.evidence),
        })
    }
    fn publish(&self, product: Product, build: &Build, revision: &str) -> io::Result<()> {
        app_identity::install_codeintel_build(
            product,
            &self.home.join(".codeintel"),
            self.source_dir,
            &build.output.join(name(product)),
            &build.version,
            revision,
        )
    }
}

fn executable(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

fn managed_evidence(result: &managed_cargo_bridge::Result) -> String {
    format!(
        "invocation={} receipt={} target={}",
        result
            .evidence
            .invocation_dir
            .as_deref()
            .map_or("none".into(), |path| path.display().to_string()),
        result
            .evidence
            .receipt
            .as_deref()
            .map_or("none".into(), |path| path.display().to_string()),
        result
            .evidence
            .target
            .as_deref()
            .map_or("none".into(), |path| path.display().to_string()),
    )
}

pub(crate) fn inspect(hex_dir: &Path, source_dir: &Path) -> io::Result<Plan> {
    if !cfg!(target_os = "macos") {
        return Ok(Plan::default());
    }
    inspect_with(&Native::new(hex_dir, source_dir)?)
}

pub(crate) fn apply(hex_dir: &Path, source_dir: &Path, plan: &Plan) -> io::Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    apply_with(&Native::new(hex_dir, source_dir)?, plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        revisions: RefCell<[Option<String>; 2]>,
        aliases: RefCell<[bool; 2]>,
        service_stale: RefCell<bool>,
        calls: RefCell<Vec<String>>,
        fail: RefCell<Option<String>>,
        source: RefCell<String>,
    }
    fn index(product: Product) -> usize {
        usize::from(product == Product::Daemon)
    }
    impl Fake {
        fn new(revisions: [Option<&str>; 2]) -> Self {
            Self {
                revisions: RefCell::new(revisions.map(|r| r.map(str::to_owned))),
                aliases: RefCell::new([false; 2]),
                service_stale: RefCell::new(false),
                calls: RefCell::new(vec![]),
                fail: RefCell::new(None),
                source: RefCell::new("new".into()),
            }
        }
        fn call(&self, value: &str) -> io::Result<()> {
            self.calls.borrow_mut().push(value.into());
            if self.fail.borrow().as_deref() == Some(value) {
                return Err(io::Error::other(value));
            }
            Ok(())
        }
    }
    impl Operations for Fake {
        fn mode(&self, p: Product) -> io::Result<UpgradeMode> {
            Ok(self.revisions.borrow()[index(p)]
                .clone()
                .map(UpgradeMode::Signed)
                .unwrap_or(UpgradeMode::Migrate))
        }
        fn source(&self) -> io::Result<String> {
            self.call("source")?;
            Ok(self.source.borrow().clone())
        }
        fn alias(&self, p: Product, dry: bool) -> io::Result<bool> {
            if !dry {
                self.call(&format!("alias-{}", name(p)))?;
            }
            let stale = self.aliases.borrow()[index(p)];
            if !dry {
                self.aliases.borrow_mut()[index(p)] = false;
            }
            Ok(stale)
        }
        fn service(&self, dry: bool) -> io::Result<CodeIntelServiceChange> {
            if !dry {
                self.call("service")?;
            }
            let stale = *self.service_stale.borrow();
            if !dry {
                *self.service_stale.borrow_mut() = false;
            }
            Ok(CodeIntelServiceChange {
                needed: stale,
                recovery_pending: false,
            })
        }
        fn build(&self) -> io::Result<Build> {
            self.call("build")?;
            Ok(Build {
                output: PathBuf::new(),
                version: "0.1.0".into(),
                _evidence: None,
            })
        }
        fn publish(&self, p: Product, _build: &Build, revision: &str) -> io::Result<()> {
            self.call(&format!("publish-{}", name(p)))?;
            self.revisions.borrow_mut()[index(p)] = Some(revision.into());
            Ok(())
        }
    }

    #[test]
    fn companion_only_retry_builds_once_and_preserves_completed_product() {
        let ops = Fake::new([None, None]);
        *ops.fail.borrow_mut() = Some("publish-scipd".into());
        assert!(apply_with(&ops, &inspect_with(&ops).unwrap()).is_err());
        assert_eq!(ops.revisions.borrow()[0].as_deref(), Some("new"));
        assert!(ops.revisions.borrow()[1].is_none());
        ops.calls.borrow_mut().clear();
        *ops.fail.borrow_mut() = None;
        apply_with(&ops, &inspect_with(&ops).unwrap()).unwrap();
        assert_eq!(
            ops.calls.borrow().iter().filter(|s| *s == "build").count(),
            1
        );
        assert!(!ops.calls.borrow().iter().any(|s| s == "publish-cq"));
        assert!(ops.calls.borrow().iter().any(|s| s == "publish-scipd"));
        ops.calls.borrow_mut().clear();
        let plan = inspect_with(&ops).unwrap();
        assert!(!plan.needs_work());
        apply_with(&ops, &plan).unwrap();
        assert!(ops.calls.borrow().iter().all(|s| s == "source"));
    }

    #[test]
    fn service_and_alias_only_repairs_do_not_compile_or_publish() {
        for service in [false, true] {
            let ops = Fake::new([Some("new"), Some("new")]);
            *ops.service_stale.borrow_mut() = service;
            ops.aliases.borrow_mut()[0] = !service;
            let plan = inspect_with(&ops).unwrap();
            assert!(plan.needs_work());
            apply_with(&ops, &plan).unwrap();
            let calls = ops.calls.borrow();
            assert!(!calls
                .iter()
                .any(|s| s == "build" || s.starts_with("publish")));
            assert!(calls
                .iter()
                .any(|s| s == if service { "service" } else { "alias-cq" }));
        }
    }

    #[test]
    fn failed_build_preserves_both_installed_revisions() {
        let ops = Fake::new([Some("old"), Some("old")]);
        *ops.fail.borrow_mut() = Some("build".into());
        assert!(apply_with(&ops, &inspect_with(&ops).unwrap()).is_err());
        assert_eq!(
            *ops.revisions.borrow(),
            [Some("old".into()), Some("old".into())]
        );
        assert!(!ops
            .calls
            .borrow()
            .iter()
            .any(|s| s.starts_with("publish") || s == "service"));
    }

    #[test]
    fn service_failure_retains_published_apps_and_retries_without_build() {
        let ops = Fake::new([None, None]);
        *ops.service_stale.borrow_mut() = true;
        *ops.fail.borrow_mut() = Some("service".into());
        assert!(apply_with(&ops, &inspect_with(&ops).unwrap()).is_err());
        assert_eq!(
            *ops.revisions.borrow(),
            [Some("new".into()), Some("new".into())]
        );
        *ops.fail.borrow_mut() = None;
        ops.calls.borrow_mut().clear();
        apply_with(&ops, &inspect_with(&ops).unwrap()).unwrap();
        assert!(!ops
            .calls
            .borrow()
            .iter()
            .any(|s| s == "build" || s.starts_with("publish")));
    }

    #[test]
    fn changed_source_after_preflight_fails_before_build() {
        let ops = Fake::new([None, None]);
        let plan = inspect_with(&ops).unwrap();
        *ops.source.borrow_mut() = "moved".into();
        assert!(apply_with(&ops, &plan).is_err());
        assert!(!ops.calls.borrow().iter().any(|s| s == "build"));
    }

    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("system/code-intel/src")).unwrap();
        fs::create_dir_all(dir.path().join("system/scripts")).unwrap();
        fs::write(
            dir.path().join("system/code-intel/Cargo.toml"),
            "[package]\nname='scipd'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(dir.path().join("system/code-intel/src/lib.rs"), "").unwrap();
        fs::write(
            dir.path().join("system/scripts/macos-signing.py"),
            "original",
        )
        .unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-qm",
                "fixture",
            ],
        ] {
            assert!(Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        dir
    }

    #[test]
    fn source_guard_rejects_untracked_build_script_and_changed_helper() {
        for path in [
            "system/code-intel/build.rs",
            "system/scripts/macos-signing.py",
            ".cargo/config.toml",
            "Cargo.lock",
        ] {
            let dir = repository();
            assert_eq!(cargo_version(dir.path()).unwrap(), "0.1.0");
            assert!(source_revision(dir.path()).is_ok());
            let file = dir.path().join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "changed").unwrap();
            assert!(source_revision(dir.path()).is_err(), "{path}");
        }
    }
}
