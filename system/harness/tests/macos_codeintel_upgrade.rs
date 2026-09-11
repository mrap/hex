//! Actual Hex CLI orchestration with private HOME and protocol fixtures.
//! Crypto, shared publication and launchctl are tested separately. No live use.
#![cfg(target_os = "macos")]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    source: PathBuf,
    hex: PathBuf,
    bin: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let source = temp.path().join("source");
        let hex = temp.path().join("hex");
        let bin = temp.path().join("bin");
        let managed = temp.path().join("managed-target");
        write(&source.join("templates/AGENTS.md"), "fixture");
        write(
            &source.join("system/code-intel/Cargo.toml"),
            "[package]\nname='scipd'\nversion='0.1.0'\n",
        );
        write(
            &source.join("system/scripts/macos-signing.py"),
            "# Protocol fixture only\n",
        );
        write(
            &source.join("system/scripts/macos-app-install.py"),
            r#"import json,sys,pathlib
home=pathlib.Path.home(); args=sys.argv[1:]; command,product=args[:2]
assert product in ('code-intel-cli','code-intel-daemon'), args
root=home/'.codeintel'; assert pathlib.Path(args[args.index('--root')+1])==root
with (home/'calls').open('a') as f:f.write(command+('-dry' if '--dry-run' in args else '')+' '+product+'\n')
state=json.loads((home/'fixture.json').read_text()); revision=state['revision']; name='cq' if product=='code-intel-cli' else 'scipd'
result={'schema_version':1,'product':product,'mode':'signed-current'}
if command=='preflight':
    result.update(policy_available=True,managed=True,mode='signed-current' if state.get(name) else 'configured-legacy',source_revision=state.get(name))
elif command=='install':
    assert args[args.index('--version')+1]=='0.1.0'
    assert args[args.index('--source-revision')+1]==revision
    assert args[args.index('--helper-source-revision')+1]==revision
    artifact=pathlib.Path(args[args.index('--source')+1]); assert artifact.read_text()==name
    if state.get('fail')==name: raise SystemExit('fixture install failure '+name)
    state[name]=revision; result['source_revision']=revision
elif command=='compatibility-alias':
    workspace=pathlib.Path(args[args.index('--hex-workspace')+1]); alias=workspace/'.hex/bin'/name
    dry='--dry-run' in args; needed=not state.get('alias_'+name,False)
    result.update(source_revision=revision,generation='fixture-generation',alias_path=str(alias),target_path=str(root/'bin'/name),action=('would-create' if dry else 'created') if needed else 'current',changed=needed and not dry,published=needed and not dry,archive_path=None)
    if not dry: state['alias_'+name]=True
elif command=='service-reconcile':
    dry='--dry-run' in args; needed=state.get('service',True)
    pending=state.get('pending')
    if pending and pending!=state.get('scipd'):raise SystemExit('fixture pending generation mismatch')
    if not dry and state.get('fail')=='service':
        state['pending']=state.get('scipd');(home/'fixture.json').write_text(json.dumps(state));raise SystemExit('fixture service failure')
    result.update(service_recovery_pending=bool(pending),service_action=('would-restart' if dry else 'recovered') if needed else 'loaded',service_needs_change=needed,published=needed and not dry,plist_path=str(home/'Library/LaunchAgents/com.hex.scipd.plist'),executable_path=str(root/'SCIPD.app/Contents/MacOS/scipd'))
    if not dry: state['service']=False;state.pop('pending',None)
else: raise AssertionError(args)
(home/'fixture.json').write_text(json.dumps(state));print(json.dumps(result))
"#,
        );
        for file in ["macos-signing.py", "macos-app-install.py"] {
            write(
                &hex.join(".hex/scripts").join(file),
                fs::read(source.join("system/scripts").join(file)).unwrap(),
            );
        }
        write(&hex.join("CLAUDE.md"), "fixture");
        fs::create_dir_all(hex.join(".hex/bin")).unwrap();
        write(
            &home.join("Library/Application Support/Hex/build-signing/policy.json"),
            "{}",
        );
        fs::create_dir_all(home.join(".boi/v2")).unwrap();
        fs::create_dir_all(&managed).unwrap();
        write(
            &home.join(".boi/v2/daemon.toml"),
            format!(
                "cargo_target_dir = \"{}\"\n[managed_target_policy]\nrevision = \"fixture\"\nallowed_roots = [\"{}\"]\ndenied_roots = [\"{}\"]\n",
                managed.display(), managed.display(), temp.path().join("denied").display()
            ),
        );
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
                .current_dir(&source)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        let revision = Command::new("git")
            .current_dir(&source)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let revision = String::from_utf8(revision.stdout).unwrap();
        write(
            &home.join("fixture.json"),
            serde_json::to_vec(&serde_json::json!({"revision": revision.trim()})).unwrap(),
        );
        write(
            &bin.join("rustc"),
            "#!/bin/sh\nprintf 'host: aarch64-apple-darwin\\n'\n",
        );
        write(
            &bin.join("cargo"),
            r#"#!/usr/bin/python3
import pathlib,sys,os,json
home=pathlib.Path.home();state=json.loads((home/'fixture.json').read_text());args=sys.argv[1:]
with (home/'calls').open('a') as f:f.write('cargo\n')
assert args==['build','--locked','--release','--package','scipd','--bin','cq','--bin','scipd','--target','aarch64-apple-darwin'],args
target=pathlib.Path(os.environ['CARGO_TARGET_DIR']);host=args[args.index('--target')+1]
assert target.is_absolute(); assert host=='aarch64-apple-darwin'; assert os.environ['CARGO_BUILD_BUILD_DIR']==str(target)
if state.get('fail')=='build':raise SystemExit(9)
out=target/('wrong-target' if state.get('fail')=='wrong-target' else host)/'release';out.mkdir(parents=True)
for name in ('cq','scipd'):
    path=out/name; path.write_text(name)
    path.chmod(0o755)
if state.get('fail')=='empty':(out/'cq').write_text('')
if state.get('fail')=='nonexec':(out/'cq').chmod(0o644)
if state.get('fail')=='symlink':
    (out/'cq').unlink(); (out/'cq').symlink_to(out/'scipd')
"#,
        );
        for name in ["cargo", "rustc"] {
            fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            _temp: temp,
            home,
            source,
            hex,
            bin,
        }
    }
    fn run(&self) -> Output {
        self.command().output().unwrap()
    }
    fn run_with_inherited_build_dir(&self) -> Output {
        self.command()
            .env(
                "CARGO_BUILD_BUILD_DIR",
                self.home.join("wrong-inherited-build-dir"),
            )
            .output()
            .unwrap()
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hex"));
        command
            .args(["upgrade", "--local"])
            .arg(&self.source)
            .env_clear()
            .env("HOME", &self.home)
            .env("HEX_DIR", &self.hex)
            .env(
                "PATH",
                format!("{}:/opt/homebrew/bin:/usr/bin:/bin", self.bin.display()),
            )
            .env("CARGO_BUILD_TARGET", "wrong-inherited-target");
        command
    }
    fn state(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.home.join("fixture.json")).unwrap()).unwrap()
    }
    fn fail(&self, failure: &str) {
        let mut state = self.state();
        state["fail"] = failure.into();
        write(
            &self.home.join("fixture.json"),
            serde_json::to_vec(&state).unwrap(),
        );
        write(&self.home.join("calls"), "");
    }
    fn calls(&self) -> String {
        fs::read_to_string(self.home.join("calls")).unwrap()
    }
}

#[test]
fn actual_cli_resumes_companions_without_versions_or_hex_build() {
    let fixture = Fixture::new();
    fixture.fail("scipd");
    let first = fixture.run();
    assert!(
        !first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stdout)
    );
    assert_eq!(fixture.state()["cq"], fixture.state()["revision"]);
    assert!(fixture.state().get("scipd").is_none());
    assert!(!fixture.hex.join(".hex/upgrade.json").exists());
    fixture.fail("service");
    let second = fixture.run();
    assert!(!second.status.success());
    assert!(!fixture.calls().contains("install code-intel-cli"));
    assert!(fixture.calls().contains("install code-intel-daemon"));
    assert!(!fixture.hex.join(".hex/upgrade.json").exists());
    fixture.fail("");
    let third = fixture.run();
    assert!(
        third.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&third.stdout),
        String::from_utf8_lossy(&third.stderr)
    );
    assert!(!fixture.calls().contains("cargo"));
    assert!(!fixture.calls().contains("install code-intel"));
    assert!(fixture.hex.join(".hex/upgrade.json").exists());
    fixture.fail("");
    let fourth = fixture.run();
    assert!(fourth.status.success());
    assert!(String::from_utf8_lossy(&fourth.stdout).contains("Nothing to do"));
    assert!(!fixture.calls().contains("cargo"));
}

#[test]
fn actual_cli_build_failures_never_publish_or_record_success() {
    for failure in ["build", "wrong-target", "empty", "nonexec", "symlink"] {
        let fixture = Fixture::new();
        fixture.fail(failure);
        let result = fixture.run();
        assert!(!result.status.success(), "{failure}");
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            output.contains("retained evidence: invocation="),
            "{failure}: {output}"
        );
        assert!(output.contains("receipt="), "{failure}: {output}");
        assert!(output.contains("target="), "{failure}: {output}");
        assert!(!fixture.calls().contains("install code-intel"));
        assert!(!fixture.hex.join(".hex/upgrade.json").exists());
    }
}

#[test]
fn actual_cli_rejects_inherited_build_selection_before_fake_cargo() {
    let fixture = Fixture::new();
    let result = fixture.run_with_inherited_build_dir();
    assert!(!result.status.success());
    assert!(!fixture.calls().contains("cargo"));
    assert!(!fixture.calls().contains("install code-intel"));
    assert!(!fixture.hex.join(".hex/upgrade.json").exists());
}

#[test]
fn actual_cli_finishes_pending_reload_before_new_source_publication() {
    let fixture = Fixture::new();
    fixture.fail("service");
    assert!(!fixture.run().status.success());
    let old = fixture.state()["scipd"].clone();
    assert_eq!(fixture.state()["pending"], old);
    write(
        &fixture.source.join("release-note.txt"),
        "new selected source",
    );
    for args in [
        vec!["add", "release-note.txt"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "next source",
        ],
    ] {
        assert!(Command::new("git")
            .current_dir(&fixture.source)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    let revision = Command::new("git")
        .current_dir(&fixture.source)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let revision = String::from_utf8(revision.stdout).unwrap();
    let mut state = fixture.state();
    state["revision"] = revision.trim().into();
    write(
        &fixture.home.join("fixture.json"),
        serde_json::to_vec(&state).unwrap(),
    );
    fixture.fail("");
    let result = fixture.run();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let calls = fixture.calls();
    assert!(
        calls.find("service-reconcile code-intel-daemon").unwrap()
            < calls.find("install code-intel-daemon").unwrap()
    );
    assert_eq!(fixture.state()["scipd"], fixture.state()["revision"]);
    assert_ne!(fixture.state()["scipd"], old);
    assert!(fixture.state().get("pending").is_none());
}
