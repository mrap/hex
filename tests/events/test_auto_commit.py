"""Tests for auto-commit-boi-output.sh and init-boi-project-repo.sh"""
import os
import subprocess
import tempfile

import pytest

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
AUTO_COMMIT = os.path.join(_REPO_ROOT, "scripts", "auto-commit-boi-output.sh")
INIT_REPO = os.path.join(_REPO_ROOT, "scripts", "init-boi-project-repo.sh")


# ── Helpers ───────────────────────────────────────────────────────────────────

def git(*args, cwd=None, check=True):
    return subprocess.run(
        ["git"] + list(args),
        cwd=cwd,
        capture_output=True,
        text=True,
        check=check,
    )


def make_repo(tmpdir, name="repo"):
    """Create a git repo with one initial commit. Returns repo path."""
    repo = os.path.join(tmpdir, name)
    os.makedirs(repo)
    git("init", cwd=repo)
    git("config", "user.email", "test@test.com", cwd=repo)
    git("config", "user.name", "Test", cwd=repo)
    readme = os.path.join(repo, "README.md")
    with open(readme, "w") as f:
        f.write("hello\n")
    git("add", ".", cwd=repo)
    git("commit", "-m", "init", cwd=repo)
    return repo


def run_auto_commit(spec_id, repo_path, ops_log=None):
    env = os.environ.copy()
    if ops_log:
        env["HOME"] = os.path.dirname(os.path.dirname(ops_log))  # HOME/.boi/ops-actions.log
    return subprocess.run(
        ["bash", AUTO_COMMIT, spec_id, repo_path],
        capture_output=True,
        text=True,
        env=env,
    )


def run_init_repo(repo_path, ops_log_dir=None):
    env = os.environ.copy()
    if ops_log_dir:
        env["HOME"] = ops_log_dir
    return subprocess.run(
        ["bash", INIT_REPO, repo_path],
        capture_output=True,
        text=True,
        env=env,
    )


# ── auto-commit-boi-output.sh tests ──────────────────────────────────────────

def test_auto_commit_script_exists():
    assert os.path.isfile(AUTO_COMMIT), f"Script not found: {AUTO_COMMIT}"


def test_auto_commit_on_dirty_repo_creates_commit():
    """Auto-commit creates a commit when repo has uncommitted changes."""
    with tempfile.TemporaryDirectory() as tmpdir:
        repo = make_repo(tmpdir)

        # Add an uncommitted file
        new_file = os.path.join(repo, "output.txt")
        with open(new_file, "w") as f:
            f.write("boi output\n")

        result = run_auto_commit("q-test-1", repo)
        assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"

        # Confirm commit was created
        log = git("log", "--oneline", "-1", cwd=repo)
        assert "q-test-1" in log.stdout
        assert "auto-committed by hex-ops" in log.stdout

        # Repo should be clean now
        status = git("status", "--porcelain", cwd=repo)
        assert status.stdout.strip() == ""


def test_auto_commit_on_clean_repo_is_noop():
    """Auto-commit exits 0 and makes no commit when repo is clean."""
    with tempfile.TemporaryDirectory() as tmpdir:
        repo = make_repo(tmpdir)

        # Get commit count before
        before = git("rev-list", "--count", "HEAD", cwd=repo).stdout.strip()

        result = run_auto_commit("q-test-clean", repo)
        assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
        assert "clean" in result.stdout.lower() or "nothing" in result.stdout.lower()

        # Commit count unchanged
        after = git("rev-list", "--count", "HEAD", cwd=repo).stdout.strip()
        assert before == after


def test_auto_commit_on_non_git_directory_skips():
    """Auto-commit skips (exit 0) when given a non-git directory."""
    with tempfile.TemporaryDirectory() as tmpdir:
        non_git = os.path.join(tmpdir, "not-a-repo")
        os.makedirs(non_git)

        result = run_auto_commit("q-test-nongit", non_git)
        assert result.returncode == 0, f"stderr={result.stderr!r}"
        assert "not a git repo" in result.stderr.lower() or "skipping" in result.stderr.lower()


def test_auto_commit_logs_to_ops_actions_log():
    """Auto-commit appends a line to ops-actions.log."""
    with tempfile.TemporaryDirectory() as tmpdir:
        repo = make_repo(tmpdir)

        # Create a dirty file
        with open(os.path.join(repo, "change.txt"), "w") as f:
            f.write("change\n")

        # Override HOME so ops-actions.log goes into tmpdir
        fake_home = os.path.join(tmpdir, "home")
        os.makedirs(os.path.join(fake_home, ".boi"), exist_ok=True)
        ops_log = os.path.join(fake_home, ".boi", "ops-actions.log")

        env = os.environ.copy()
        env["HOME"] = fake_home
        result = subprocess.run(
            ["bash", AUTO_COMMIT, "q-log-test", repo],
            capture_output=True, text=True, env=env,
        )
        assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"

        assert os.path.isfile(ops_log), "ops-actions.log was not created"
        log_content = open(ops_log).read()
        assert "auto-commit" in log_content
        assert "q-log-test" in log_content


# ── init-boi-project-repo.sh tests ───────────────────────────────────────────

def test_init_repo_script_exists():
    assert os.path.isfile(INIT_REPO), f"Script not found: {INIT_REPO}"


def test_init_repo_creates_gitignore_with_correct_patterns():
    """init script creates .gitignore with required patterns."""
    with tempfile.TemporaryDirectory() as tmpdir:
        project = os.path.join(tmpdir, "my-project")
        os.makedirs(project)

        fake_home = os.path.join(tmpdir, "home")
        os.makedirs(os.path.join(fake_home, ".boi"), exist_ok=True)

        env = os.environ.copy()
        env["HOME"] = fake_home
        result = subprocess.run(
            ["bash", INIT_REPO, project],
            capture_output=True, text=True, env=env,
        )
        assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"

        gitignore_path = os.path.join(project, ".gitignore")
        assert os.path.isfile(gitignore_path), ".gitignore not created"

        content = open(gitignore_path).read()
        required = ["__pycache__/", "*.pyc", "data/*.db", "data/reports/", "data/heartbeat.txt", "*.log"]
        for pattern in required:
            assert pattern in content, f"Missing pattern in .gitignore: {pattern!r}"


def test_init_repo_is_idempotent():
    """Running init script twice on same repo exits 0 without error."""
    with tempfile.TemporaryDirectory() as tmpdir:
        project = os.path.join(tmpdir, "my-project")
        os.makedirs(project)
        # Pre-seed a file so there's something to commit
        with open(os.path.join(project, "main.py"), "w") as f:
            f.write("print('hello')\n")

        fake_home = os.path.join(tmpdir, "home")
        os.makedirs(os.path.join(fake_home, ".boi"), exist_ok=True)
        env = os.environ.copy()
        env["HOME"] = fake_home

        # First run
        r1 = subprocess.run(["bash", INIT_REPO, project], capture_output=True, text=True, env=env)
        assert r1.returncode == 0, f"First run failed: {r1.stderr!r}"

        # Verify it became a git repo
        assert os.path.isdir(os.path.join(project, ".git"))

        # Second run — should detect .git already exists and exit 0
        r2 = subprocess.run(["bash", INIT_REPO, project], capture_output=True, text=True, env=env)
        assert r2.returncode == 0, f"Second run failed: {r2.stderr!r}"
        combined = r2.stdout + r2.stderr
        assert "already" in combined.lower()

        # Commit count should still be 1 (second run made no new commit)
        log = git("log", "--oneline", cwd=project)
        commit_count = len(log.stdout.strip().splitlines())
        assert commit_count == 1, f"Expected 1 commit after idempotent run, got {commit_count}"
