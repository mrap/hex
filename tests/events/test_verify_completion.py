"""Tests for verify-boi-completion.sh"""
import os
import subprocess
import tempfile

import pytest

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPT = os.path.join(_REPO_ROOT, "scripts", "verify-boi-completion.sh")


def run_script(spec_id: str, repo_path: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", SCRIPT, spec_id, repo_path],
        capture_output=True,
        text=True,
    )


def init_bare_remote(tmpdir: str) -> str:
    """Create a bare remote repo and return its path."""
    remote = os.path.join(tmpdir, "remote.git")
    os.makedirs(remote)
    subprocess.run(["git", "init", "--bare", remote], check=True, capture_output=True)
    return remote


def init_repo_with_remote(tmpdir: str, remote_path: str) -> str:
    """Create a git repo with an initial commit pushed to remote."""
    repo = os.path.join(tmpdir, "repo")
    os.makedirs(repo)
    subprocess.run(["git", "init", repo], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "config", "user.email", "test@test.com"], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "config", "user.name", "Test"], check=True, capture_output=True)
    # Initial commit
    readme = os.path.join(repo, "README.md")
    with open(readme, "w") as f:
        f.write("hello\n")
    subprocess.run(["git", "-C", repo, "add", "."], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "commit", "-m", "init"], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "remote", "add", "origin", remote_path], check=True, capture_output=True)
    # Use whatever branch was created (main or master)
    branch = subprocess.run(
        ["git", "-C", repo, "rev-parse", "--abbrev-ref", "HEAD"],
        check=True, capture_output=True, text=True
    ).stdout.strip()
    subprocess.run(["git", "-C", repo, "push", "-u", "origin", branch], check=True, capture_output=True)
    return repo


# ── Tests ─────────────────────────────────────────────────────────────────────

def test_script_exists():
    assert os.path.isfile(SCRIPT), f"Script not found: {SCRIPT}"


def test_missing_args_exits_nonzero():
    result = subprocess.run(["bash", SCRIPT], capture_output=True, text=True)
    assert result.returncode != 0


def test_nonexistent_repo_fails():
    result = run_script("q-test", "/tmp/nonexistent-repo-xyz-12345")
    assert result.returncode == 1
    assert "does not exist" in result.stdout + result.stderr


def test_dirty_repo_fails():
    with tempfile.TemporaryDirectory() as tmpdir:
        remote = init_bare_remote(tmpdir)
        repo = init_repo_with_remote(tmpdir, remote)

        # Add uncommitted change
        dirty_file = os.path.join(repo, "dirty.txt")
        with open(dirty_file, "w") as f:
            f.write("uncommitted\n")

        result = run_script("q-test", repo)
        assert result.returncode == 1, f"Expected failure; stdout={result.stdout!r} stderr={result.stderr!r}"
        combined = result.stdout + result.stderr
        assert "Uncommitted" in combined or "uncommitted" in combined


def test_unpushed_commits_fails():
    with tempfile.TemporaryDirectory() as tmpdir:
        remote = init_bare_remote(tmpdir)
        repo = init_repo_with_remote(tmpdir, remote)

        # Add a committed-but-not-pushed change
        new_file = os.path.join(repo, "new.txt")
        with open(new_file, "w") as f:
            f.write("new content\n")
        subprocess.run(["git", "-C", repo, "add", "."], check=True, capture_output=True)
        subprocess.run(["git", "-C", repo, "commit", "-m", "unpushed commit"], check=True, capture_output=True)

        result = run_script("q-test", repo)
        assert result.returncode == 1, f"Expected failure; stdout={result.stdout!r} stderr={result.stderr!r}"
        combined = result.stdout + result.stderr
        assert "npu" in combined.lower()  # "unpushed"


def test_clean_repo_passes():
    with tempfile.TemporaryDirectory() as tmpdir:
        remote = init_bare_remote(tmpdir)
        repo = init_repo_with_remote(tmpdir, remote)

        result = run_script("q-clean", repo)
        assert result.returncode == 0, f"Expected success; stdout={result.stdout!r} stderr={result.stderr!r}"
        assert "VERIFIED: q-clean" in result.stdout


def test_verified_message_contains_spec_id():
    with tempfile.TemporaryDirectory() as tmpdir:
        remote = init_bare_remote(tmpdir)
        repo = init_repo_with_remote(tmpdir, remote)

        result = run_script("q-147", repo)
        assert result.returncode == 0
        assert "q-147" in result.stdout
