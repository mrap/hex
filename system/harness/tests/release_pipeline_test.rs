//! Integration tests for the GitFlow release pipeline (oss-releaser spec,
//! scope item 9) — drives the BUILT BINARY (`env!("CARGO_BIN_EXE_hex")`)
//! against isolated temp git repos: a main+develop pair with a local BARE
//! `origin` over a file path. No network, no real remotes, ever.
//!
//! Ports the legacy shell suites, then goes further:
//! - `tests/test_release_gates.sh` tests 1–8: semver validity, version-gate
//!   outcomes (block unchanged/regression, accept patch/major bumps), and
//!   the next-patch suggestion.
//! - `tests/test_release_tag_step.sh` (all scenarios): remote-aware tag
//!   handling — push when absent on origin (the OBS-017 fix, exercised by
//!   every success-path cut), idempotent green when origin already has the tag
//!   at the same commit, loud refusal (never overwrite) when it diverges.
//! - Ceremony scenarios: success-path cut on an injected toy profile,
//!   clean-tree refusal, lock-held refusal, missing-develop bootstrap
//!   instruction, back-merge conflict abort, develop-moved race abort, and
//!   dry-run exit codes.
//! - The pre-push hook shim (the scope-item-8 shape, installed via
//!   `core.hooksPath`): main blocked without HEX_RELEASE_PIPELINE=1, allowed
//!   with it, develop/tag pushes pass through, and a missing binary warns
//!   but never bricks the push.
//!
//! Gate injection seam: profiles load from `$HEX_DIR/.hex/config/releases.toml`,
//! so each test points HEX_DIR at its own tempdir with a toy profile whose
//! gates are trivial commands — fast, hermetic, and no Docker E2E.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_hex")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// ---------------------------------------------------------------------------
// Fixtures — toy GitFlow repo + bare origin + injected toy profile.
// ---------------------------------------------------------------------------

struct Fixture {
    _td: tempfile::TempDir,
    repo: PathBuf,
    origin: PathBuf,
    hex_dir: PathBuf,
}

/// A temp GitFlow repo (`main` + `develop`, one conventional commit,
/// `version.txt` @ 0.1.0) pushed to a local bare origin, plus a HEX_DIR whose
/// `releases.toml` injects a toy profile (match_dir = "repo") with the given
/// gate lines — the documented test seam for trivial gate batteries.
fn fixture_with_gates(gates: &str) -> Fixture {
    let td = tempfile::tempdir().expect("tempdir");
    let root = td.path();

    let origin = root.join("origin.git");
    std::fs::create_dir(&origin).unwrap();
    git(&origin, &["init", "-q", "--bare"]);

    let repo = root.join("repo"); // the toy profile matches `match_dir = "repo"`
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["config", "tag.gpgsign", "false"]);
    // Hermetic commits/pushes: neutralize any user/global hooks.
    let nohooks = root.join("nohooks");
    std::fs::create_dir_all(&nohooks).unwrap();
    git(
        &repo,
        &["config", "core.hooksPath", nohooks.to_str().unwrap()],
    );
    std::fs::write(repo.join("version.txt"), "0.1.0\n").unwrap();
    git(&repo, &["add", "-A"]);
    commit(&repo, "feat: initial");
    git(&repo, &["branch", "develop"]);
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "origin", "main", "develop"]);

    let hex_dir = root.join("hexdir");
    let cfg = hex_dir.join(".hex/config");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("releases.toml"),
        format!(
            "[[profiles]]\n\
             name = \"toy\"\n\
             match_dir = \"repo\"\n\
             gates = [\n{gates}\n]\n\
             version_files = [{{ path = \"version.txt\" }}]\n"
        ),
    )
    .unwrap();

    Fixture {
        _td: td,
        repo,
        origin,
        hex_dir,
    }
}

fn fixture() -> Fixture {
    fixture_with_gates("  { name = \"noop\", command = \"true\" },")
}

/// `hex release cut <extra>` from inside the repo, with the fixture's HEX_DIR
/// (profile injection + isolated telemetry) and a clean pipeline env.
fn run_cut(fix: &Fixture, extra: &[&str]) -> Output {
    Command::new(bin())
        .args(["release", "cut"])
        .args(extra)
        .current_dir(&fix.repo)
        .env("HEX_DIR", &fix.hex_dir)
        .env_remove("HEX_RELEASE_PIPELINE")
        .output()
        .expect("run hex release cut")
}

fn git_out(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git must be runnable in tests");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        root.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    text(&out.stdout).trim().to_string()
}

fn git(root: &Path, args: &[&str]) {
    let _ = git_out(root, args);
}

fn commit(root: &Path, subject: &str) {
    git(root, &["commit", "-q", "-m", subject]);
}

fn ref_exists(root: &Path, refname: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", refname])
        .current_dir(root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Install a post-receive hook on the bare origin that creates `tag` the
/// moment `refs/heads/main` lands, pointing at `$new` or `$old` — the seam
/// for simulating a tag appearing on origin mid-release (idempotent skip vs
/// divergent refusal at the push step).
fn install_origin_tagger(origin: &Path, tag: &str, which: &str) {
    let hooks = origin.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    // Pin hooksPath: a user-global core.hooksPath would otherwise shadow the
    // bare repo's own hooks dir and the tagger would never fire.
    git(
        origin,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    let hook = hooks.join("post-receive");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\n\
             while read old new ref; do\n\
               if [ \"$ref\" = \"refs/heads/main\" ]; then git tag {tag} \"{which}\"; fi\n\
             done\n\
             exit 0\n"
        ),
    )
    .unwrap();
    make_executable(&hook);
}

/// The pre-push exec shim — the REAL shipped artifact, not a copy. This test
/// previously embedded its own duplicate of the shim text, so the shipped
/// `.githooks/pre-push` could drift freely while the test stayed green
/// against its private copy (oss-releaser review nonblocker, 2026-06-11).
/// `include_str!` makes the shipped file the single source of truth: every
/// behavioral assertion below now exercises exactly what users run.
const PRE_PUSH_SHIM: &str = include_str!("../../../.githooks/pre-push");

/// Install the shim as the repo's pre-push hook via `core.hooksPath`.
fn install_prepush_shim(fix: &Fixture) {
    assert!(
        PRE_PUSH_SHIM.lines().count() <= 10,
        "the pre-push shim must stay <= 10 lines (spec scope item 8)"
    );
    let hooks = fix.repo.parent().unwrap().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-push");
    std::fs::write(&hook, PRE_PUSH_SHIM).unwrap();
    make_executable(&hook);
    git(
        &fix.repo,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
}

/// `git push origin <refspec>` from the repo with the shim active and PATH
/// arranged so the shim's `command -v hex` resolves to the freshly built
/// test binary. `pipeline` toggles HEX_RELEASE_PIPELINE=1.
fn push_with_shim(repo: &Path, refspec: &str, pipeline: bool) -> Output {
    let bin_dir = Path::new(bin()).parent().expect("binary has a parent dir");
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("git");
    cmd.args(["push", "origin", refspec])
        .current_dir(repo)
        .env("PATH", path);
    if pipeline {
        cmd.env("HEX_RELEASE_PIPELINE", "1");
    } else {
        cmd.env_remove("HEX_RELEASE_PIPELINE");
    }
    cmd.output().expect("run git push")
}

// ---------------------------------------------------------------------------
// Semver validity + version gate (legacy test_release_gates.sh tests 1–8).
// ---------------------------------------------------------------------------

#[test]
fn cut_refuses_invalid_semver_versions() {
    let f = fixture();
    for v in ["banana", "1.0", "1", "v1.0.0", "1.0.0-beta", "1.0.0.1"] {
        let out = run_cut(&f, &["--version", v]);
        assert!(!out.status.success(), "--version {v} must be refused");
        let err = text(&out.stderr);
        assert!(err.contains("not semver"), "--version {v}: {err}");
    }
    // The refusals mutated nothing.
    assert_eq!(git_out(&f.repo, &["tag"]), "");
    assert!(!f.repo.join(".git/hex-release.lock").exists());
}

#[test]
fn version_gate_blocks_unchanged_version_and_suggests_next_patch() {
    // Legacy tests 4 + 8: version == latest tag blocks, naming the next patch.
    let f = fixture();
    git(&f.repo, &["tag", "v1.0.0"]);
    let out = run_cut(&f, &["--version", "1.0.0"]);
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(err.contains("not greater"), "{err}");
    assert!(
        err.contains("1.0.1"),
        "next-patch suggestion missing: {err}"
    );

    // The suggestion is numeric, not string math: 1.2.9 → 1.2.10.
    let f2 = fixture();
    git(&f2.repo, &["tag", "v1.2.9"]);
    let out = run_cut(&f2, &["--version", "1.2.9"]);
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(err.contains("1.2.10"), "{err}");
}

#[test]
fn version_gate_blocks_regression() {
    // Legacy test 6: 1.0.0 → 0.9.0 blocks; nothing reaches origin.
    let f = fixture();
    git(&f.repo, &["tag", "v1.0.0"]);
    let out = run_cut(&f, &["--version", "0.9.0"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("not greater"));
    assert_eq!(git_out(&f.origin, &["tag"]), "");
}

// ---------------------------------------------------------------------------
// Success-path cuts on the injected toy profile (legacy tests 5 + 7, plus the
// tag-step "absent on origin → pushed" scenario).
// ---------------------------------------------------------------------------

#[test]
fn cut_success_path_patch_bump_lands_on_origin() {
    let f = fixture();
    git(&f.repo, &["tag", "v1.0.0"]);
    let out = run_cut(&f, &["--level", "patch"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stdout).contains("Release complete"));

    // Tag sits on the main merge commit, locally AND on origin (OBS-017:
    // a locally existing tag must still reach the remote).
    let main_sha = git_out(&f.repo, &["rev-parse", "refs/heads/main"]);
    assert_eq!(
        git_out(&f.repo, &["rev-parse", "v1.0.1^{commit}"]),
        main_sha
    );
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "refs/heads/main"]),
        main_sha
    );
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "v1.0.1^{commit}"]),
        main_sha
    );
    // develop pushed too, carrying the back-merged bump.
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "refs/heads/develop"]),
        git_out(&f.repo, &["rev-parse", "refs/heads/develop"])
    );
    assert_eq!(git_out(&f.repo, &["show", "main:version.txt"]), "1.0.1");
    assert_eq!(git_out(&f.repo, &["show", "develop:version.txt"]), "1.0.1");
    // Release branch and lock cleaned up.
    assert_eq!(git_out(&f.repo, &["branch", "--list", "release/*"]), "");
    assert!(!f.repo.join(".git/hex-release.lock").exists());
}

#[test]
fn cut_success_path_major_bump_lands_on_origin() {
    // Legacy test 7: 1.0.0 → 2.0.0 is accepted and released.
    let f = fixture();
    git(&f.repo, &["tag", "v1.0.0"]);
    let out = run_cut(&f, &["--level", "major"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let main_sha = git_out(&f.repo, &["rev-parse", "refs/heads/main"]);
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "v2.0.0^{commit}"]),
        main_sha
    );
    assert_eq!(git_out(&f.repo, &["show", "main:version.txt"]), "2.0.0");
}

#[test]
fn dry_run_exits_zero_when_green_and_one_when_blocked() {
    // Green battery: exit 0, nothing mutated.
    let f = fixture();
    let out = run_cut(&f, &["--dry-run"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stdout).contains("Dry run complete"));
    assert_eq!(git_out(&f.repo, &["tag"]), "");

    // Blocked battery: exit 1 even with --dry-run — never maskable.
    let blocked = fixture_with_gates("  { name = \"boom\", command = \"exit 1\" },");
    let out = run_cut(&blocked, &["--dry-run"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("BLOCKED"));
    assert_eq!(git_out(&blocked.repo, &["tag"]), "");
}

// ---------------------------------------------------------------------------
// Precondition refusals.
// ---------------------------------------------------------------------------

#[test]
fn cut_refuses_dirty_tree() {
    let f = fixture();
    std::fs::write(f.repo.join("stray.txt"), "x").unwrap();
    let out = run_cut(&f, &["--version", "0.2.0"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("not clean"));
    assert_eq!(git_out(&f.repo, &["tag"]), "");
}

#[test]
fn cut_refuses_missing_develop_with_bootstrap_instruction() {
    let f = fixture();
    git(&f.repo, &["branch", "-D", "develop"]);
    let out = run_cut(&f, &["--version", "0.2.0"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("git branch develop main && git push origin develop"),
        "must print the exact bootstrap instruction; got: {}",
        text(&out.stderr)
    );
}

#[test]
fn cut_refuses_when_lock_held_naming_the_holder() {
    let f = fixture();
    std::fs::write(
        f.repo.join(".git/hex-release.lock"),
        "pid=4242 started=earlier\n",
    )
    .unwrap();
    let out = run_cut(&f, &["--version", "0.2.0"]);
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(err.contains("already in flight"), "{err}");
    assert!(err.contains("pid=4242"), "{err}");
    // The holder's lock survives the refused attempt.
    assert!(f.repo.join(".git/hex-release.lock").exists());
}

// ---------------------------------------------------------------------------
// Remote tag scenarios (legacy test_release_tag_step.sh).
// ---------------------------------------------------------------------------

#[test]
fn cut_refuses_tag_that_already_exists_on_origin() {
    let f = fixture();
    git(&f.origin, &["tag", "v0.2.0", "main"]);
    let out = run_cut(&f, &["--version", "0.2.0"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("already exists on origin"));
    // Never created locally, never overwritten remotely.
    assert_eq!(git_out(&f.repo, &["tag"]), "");
}

#[test]
fn origin_side_tag_created_during_the_atomic_push_cannot_race_it() {
    // Under the atomic push (main, tag, develop land in one transaction) a
    // post-receive hook that tags main only fires AFTER our tag is already on
    // origin, so its `git tag` no-ops. The ceremony succeeds and origin holds
    // the ceremony's tag at the release merge. (Before atomic pushes this
    // fixture exercised the "tag already on origin" idempotent skip, which is
    // structurally unreachable now.)
    let f = fixture();
    install_origin_tagger(&f.origin, "v0.2.0", "$new");
    let out = run_cut(&f, &["--version", "0.2.0"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let main_sha = git_out(&f.repo, &["rev-parse", "refs/heads/main"]);
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "v0.2.0^{commit}"]),
        main_sha
    );
}

#[test]
fn divergent_remote_tag_is_refused_never_overwritten() {
    // Legacy test 3, atomic-era shape: the tag already exists on origin at a
    // different commit BEFORE the cut (the only way a divergent remote tag can
    // exist once pushes are atomic). The precondition check sees it, refuses
    // loudly before the battery and before any push, and origin's tag still
    // points where it did.
    let f = fixture();
    let divergent_sha = git_out(&f.origin, &["rev-parse", "refs/heads/main"]);
    git(&f.origin, &["tag", "v0.2.0", &divergent_sha]);
    let origin_main_before = divergent_sha.clone();
    let out = run_cut(&f, &["--version", "0.2.0"]);
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(err.contains("already exists on origin"), "{err}");
    assert!(err.contains("refusing to reuse"), "{err}");
    // The remote tag still points where it did, and main did not move.
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "v0.2.0^{commit}"]),
        divergent_sha
    );
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "refs/heads/main"]),
        origin_main_before
    );
}

// ---------------------------------------------------------------------------
// Mid-ceremony aborts.
// ---------------------------------------------------------------------------

#[test]
fn back_merge_conflict_aborts_loudly_and_pushes_nothing() {
    let f = fixture();
    // develop rewrites version.txt — a guaranteed conflict with the hotfix
    // bump when main is back-merged (the hotfix path pins main, so develop
    // legitimately diverges).
    git(&f.repo, &["checkout", "-q", "develop"]);
    std::fs::write(f.repo.join("version.txt"), "9.9.9-wip\n").unwrap();
    git(&f.repo, &["add", "-A"]);
    commit(&f.repo, "feat: divergent version edit");
    git(&f.repo, &["push", "-q", "origin", "develop"]);
    let origin_main = git_out(&f.origin, &["rev-parse", "refs/heads/main"]);
    let origin_dev = git_out(&f.origin, &["rev-parse", "refs/heads/develop"]);

    let out = run_cut(&f, &["--version", "0.1.1", "--hotfix"]);
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(err.contains("BACK-MERGE CONFLICT"), "{err}");
    assert!(err.contains("Nothing was pushed"), "{err}");
    // Origin untouched; the conflicted merge was aborted, not left half-done.
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "refs/heads/main"]),
        origin_main
    );
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "refs/heads/develop"]),
        origin_dev
    );
    assert_eq!(git_out(&f.origin, &["tag"]), "");
    assert!(!ref_exists(&f.repo, "MERGE_HEAD"));
    // Local main carries the release merge + tag, exactly as the printed
    // recovery instructions describe.
    assert!(ref_exists(&f.repo, "refs/tags/v0.1.1"));
}

#[test]
fn develop_moved_mid_cut_race_aborts_before_any_push() {
    // The battery runs detached, so a gate CAN move develop — exactly the
    // race the guard must catch before anything reaches origin.
    let f = fixture_with_gates(
        "  { name = \"mover\", command = \"git update-ref refs/heads/develop HEAD~1\" },",
    );
    git(&f.repo, &["checkout", "-q", "develop"]);
    std::fs::write(f.repo.join("b.txt"), "b").unwrap();
    git(&f.repo, &["add", "-A"]);
    commit(&f.repo, "feat: second");
    git(&f.repo, &["push", "-q", "origin", "develop"]);
    let origin_main = git_out(&f.origin, &["rev-parse", "refs/heads/main"]);
    let origin_dev = git_out(&f.origin, &["rev-parse", "refs/heads/develop"]);

    let out = run_cut(&f, &["--version", "0.2.0"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("moved during the cut"));
    // Nothing reached origin.
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "refs/heads/main"]),
        origin_main
    );
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "refs/heads/develop"]),
        origin_dev
    );
    assert_eq!(git_out(&f.origin, &["tag"]), "");
}

// ---------------------------------------------------------------------------
// Pre-push shim passthrough (scope item 8 semantics, via core.hooksPath).
// ---------------------------------------------------------------------------

#[test]
fn prepush_shim_blocks_main_push_without_pipeline_env() {
    let f = fixture();
    install_prepush_shim(&f);
    let origin_main = git_out(&f.origin, &["rev-parse", "refs/heads/main"]);
    std::fs::write(f.repo.join("c.txt"), "c").unwrap();
    git(&f.repo, &["add", "-A"]);
    commit(&f.repo, "feat: direct-to-main attempt");

    let out = push_with_shim(&f.repo, "main", false);
    assert!(
        !out.status.success(),
        "main push without the pipeline env must be blocked"
    );
    let err = text(&out.stderr);
    assert!(err.contains("BLOCKED"), "{err}");
    assert!(err.contains("hex release cut"), "{err}");
    // The blocked push changed nothing on origin.
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "refs/heads/main"]),
        origin_main
    );
}

#[test]
fn prepush_shim_allows_main_push_with_pipeline_env() {
    let f = fixture();
    install_prepush_shim(&f);
    std::fs::write(f.repo.join("c.txt"), "c").unwrap();
    git(&f.repo, &["add", "-A"]);
    commit(&f.repo, "feat: pipeline push");

    let out = push_with_shim(&f.repo, "main", true);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "refs/heads/main"]),
        git_out(&f.repo, &["rev-parse", "refs/heads/main"])
    );
}

#[test]
fn prepush_shim_passes_develop_and_tag_pushes_through() {
    let f = fixture();
    install_prepush_shim(&f);
    git(&f.repo, &["checkout", "-q", "develop"]);
    std::fs::write(f.repo.join("d.txt"), "d").unwrap();
    git(&f.repo, &["add", "-A"]);
    commit(&f.repo, "feat: develop work");
    git(&f.repo, &["tag", "v9.9.9"]);

    let out = push_with_shim(&f.repo, "develop", false);
    assert!(
        out.status.success(),
        "develop push must pass: {}",
        text(&out.stderr)
    );
    let out = push_with_shim(&f.repo, "v9.9.9", false);
    assert!(
        out.status.success(),
        "tag push must pass: {}",
        text(&out.stderr)
    );
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "refs/heads/develop"]),
        git_out(&f.repo, &["rev-parse", "refs/heads/develop"])
    );
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "v9.9.9^{commit}"]),
        git_out(&f.repo, &["rev-parse", "v9.9.9^{commit}"])
    );
}

#[test]
fn prepush_shim_warns_but_never_bricks_when_binary_missing() {
    // Footgun-guard, not a security boundary: with no hex binary reachable,
    // the shim prints one warning and the push proceeds.
    let f = fixture();
    install_prepush_shim(&f);
    std::fs::write(f.repo.join("e.txt"), "e").unwrap();
    git(&f.repo, &["add", "-A"]);
    commit(&f.repo, "feat: pushed without hex available");

    let out = Command::new("/usr/bin/git")
        .args(["push", "origin", "main"])
        .current_dir(&f.repo)
        .env("PATH", "/usr/bin:/bin") // git available, hex not
        .env_remove("HEX_RELEASE_PIPELINE")
        .output()
        .expect("run git push");
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stderr).contains("skipping git-guard"));
    assert_eq!(
        git_out(&f.origin, &["rev-parse", "refs/heads/main"]),
        git_out(&f.repo, &["rev-parse", "refs/heads/main"])
    );
}
