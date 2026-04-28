"""Tests for the boi-completion-gate policy.

Tests:
  1. Policy loads correctly (metadata, provides/requires, 1 rule)
  2. Rule matches boi.spec.completed events
  3. Rule has shell action with correct command shape
  4. provides/requires are correct
  5. standing_orders and reflection_ids are set
  Integration:
  6. Verify script runs and detects dirty repo (exits 1)
  7. Verify script runs on clean repo (exits 0)
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile

import pytest

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, _REPO_ROOT)

from db import EventsDB
from policy import load_policies
from hex_eventd import _process_event_policies

POLICY_FILE = os.path.join(_REPO_ROOT, "policies", "boi-lifecycle", "boi-completion-gate.yaml")
VERIFY_SCRIPT = os.path.join(_REPO_ROOT, "scripts", "verify-boi-completion.sh")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def load_gate_policy():
    policies_dir = os.path.join(_REPO_ROOT, "policies")
    policies = load_policies(policies_dir)
    for p in policies:
        if p.name == "boi-completion-gate":
            return p
    raise RuntimeError("boi-completion-gate policy not found")


def make_bare_remote(tmp_dir):
    """Create a bare git repo to act as remote."""
    remote = os.path.join(tmp_dir, "remote.git")
    os.makedirs(remote)
    subprocess.run(["git", "init", "--bare", remote], check=True, capture_output=True)
    return remote


def make_clean_repo(tmp_dir, remote_path):
    """Create a git repo with one committed+pushed file."""
    repo = os.path.join(tmp_dir, "clean_repo")
    os.makedirs(repo)
    subprocess.run(["git", "init", repo], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "config", "user.email", "test@test.com"], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "config", "user.name", "Test"], check=True, capture_output=True)
    f = os.path.join(repo, "README.md")
    with open(f, "w") as fh:
        fh.write("clean\n")
    subprocess.run(["git", "-C", repo, "add", "."], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "commit", "-m", "init"], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "remote", "add", "origin", remote_path], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "push", "-u", "origin", "HEAD"], check=True, capture_output=True)
    return repo


def make_dirty_repo(tmp_dir):
    """Create a git repo with uncommitted changes whose remote becomes unreachable.

    Flow: initial commit is pushed to a bare remote, then the remote URL is
    changed to a non-existent path. This means:
    - auto-commit-boi-output.sh commits the dirty file but cannot push
    - verify-boi-completion.sh on retry detects unpushed commits (origin/HEAD
      exists from the initial push, new commit is not there) → FAIL → violation

    Returns (repo_path, bare_remote_path) so callers can restore the remote.
    """
    remote = make_bare_remote(tmp_dir)
    repo = os.path.join(tmp_dir, "dirty_repo")
    os.makedirs(repo)
    subprocess.run(["git", "init", repo], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "config", "user.email", "test@test.com"], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "config", "user.name", "Test"], check=True, capture_output=True)
    f = os.path.join(repo, "README.md")
    with open(f, "w") as fh:
        fh.write("original\n")
    subprocess.run(["git", "-C", repo, "add", "."], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "commit", "-m", "init"], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "remote", "add", "origin", remote], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "push", "-u", "origin", "HEAD"], check=True, capture_output=True)
    # Explicitly set origin/HEAD so verify-boi-completion.sh can detect unpushed commits
    subprocess.run(["git", "-C", repo, "remote", "set-head", "origin", "main"], capture_output=True)
    subprocess.run(["git", "-C", repo, "remote", "set-head", "origin", "master"], capture_output=True)
    # Break the remote URL so future pushes fail but origin/HEAD still exists
    broken_remote = os.path.join(tmp_dir, "nonexistent_remote.git")
    subprocess.run(["git", "-C", repo, "remote", "set-url", "origin", broken_remote], check=True, capture_output=True)
    # Now dirty: untracked file
    dirty = os.path.join(repo, "dirty.txt")
    with open(dirty, "w") as fh:
        fh.write("uncommitted\n")
    return repo


# ---------------------------------------------------------------------------
# Test 1: Policy loads correctly
# ---------------------------------------------------------------------------

def test_policy_loads():
    policy = load_gate_policy()
    assert policy.name == "boi-completion-gate"
    assert len(policy.rules) == 1


def test_policy_metadata():
    policy = load_gate_policy()
    assert "R-028" in policy.reflection_ids
    assert "R-047" in policy.reflection_ids
    assert "6" in policy.standing_orders
    assert "12" in policy.standing_orders


def test_policy_provides_requires():
    policy = load_gate_policy()
    assert "boi.completion.verified" in policy.provides.get("events", [])
    assert "policy.violation" in policy.provides.get("events", [])
    assert "boi.output.committed" in policy.requires.get("events", [])


# ---------------------------------------------------------------------------
# Test 2: Rule matching
# ---------------------------------------------------------------------------

def test_rule_matches_boi_output_committed():
    policy = load_gate_policy()
    matching = [r for r in policy.rules if r.matches_event_type("boi.output.committed")]
    assert len(matching) == 1
    assert matching[0].name == "verify-after-commit"


def test_rule_does_not_match_other_events():
    policy = load_gate_policy()
    for event in ["boi.spec.completed", "boi.spec.failed", "boi.iteration.done", "policy.violation"]:
        matching = [r for r in policy.rules if r.matches_event_type(event)]
        assert len(matching) == 0, f"Rule should not match {event}"


# ---------------------------------------------------------------------------
# Test 3: Action shape
# ---------------------------------------------------------------------------

def test_rule_has_shell_action():
    policy = load_gate_policy()
    rule = policy.rules[0]
    assert len(rule.actions) == 1
    action = rule.actions[0]
    assert action.type == "shell"
    assert "command" in action.params
    assert "verify-boi-completion.sh" in action.params["command"]
    assert "on_success" in action.params
    assert "on_failure" in action.params


def test_on_success_emits_verified():
    policy = load_gate_policy()
    rule = policy.rules[0]
    action = rule.actions[0]
    on_success = action.params["on_success"]
    assert len(on_success) == 1
    assert on_success[0]["type"] == "emit"
    assert on_success[0]["event"] == "boi.completion.verified"


def test_on_failure_emits_violation():
    policy = load_gate_policy()
    rule = policy.rules[0]
    action = rule.actions[0]
    on_failure = action.params["on_failure"]
    assert len(on_failure) == 1
    assert on_failure[0]["type"] == "emit"
    assert on_failure[0]["event"] == "policy.violation"
    assert on_failure[0]["payload"]["policy"] == "boi-completion-gate"


# ---------------------------------------------------------------------------
# Integration tests: verify script behavior
# ---------------------------------------------------------------------------

def test_integration_dirty_repo_fails():
    """Dirty repo (uncommitted changes) must cause verify script to exit non-zero."""
    with tempfile.TemporaryDirectory() as tmp:
        repo = make_dirty_repo(tmp)
        result = subprocess.run(
            ["bash", VERIFY_SCRIPT, "q-test", repo],
            capture_output=True, text=True
        )
        assert result.returncode != 0, (
            f"Expected non-zero exit for dirty repo.\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )
        combined = result.stdout + result.stderr
        assert "uncommitted" in combined.lower() or "dirty" in combined.lower() or "changes" in combined.lower()


def test_integration_clean_repo_passes():
    """Clean repo with everything pushed must cause verify script to exit 0."""
    with tempfile.TemporaryDirectory() as tmp:
        remote = make_bare_remote(tmp)
        repo = make_clean_repo(tmp, remote)
        result = subprocess.run(
            ["bash", VERIFY_SCRIPT, "q-test", repo],
            capture_output=True, text=True
        )
        assert result.returncode == 0, (
            f"Expected zero exit for clean repo.\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )
        assert "VERIFIED" in result.stdout


# ---------------------------------------------------------------------------
# Full-pipeline integration tests: event → policy engine → emit
# ---------------------------------------------------------------------------

def _make_temp_db(tmp_dir: str) -> "EventsDB":
    """Create a temp SQLite EventsDB."""
    db_path = os.path.join(tmp_dir, "test.db")
    return EventsDB(db_path)


def _load_gate_policy_list() -> list:
    """Load boi-completion-gate from the repo policies dir."""
    policies_dir = os.path.join(_REPO_ROOT, "policies")
    return [p for p in load_policies(policies_dir) if p.name == "boi-completion-gate"]


def _count_events(db: "EventsDB", event_type: str) -> int:
    row = db.conn.execute(
        "SELECT COUNT(*) as cnt FROM events WHERE event_type = ?", (event_type,)
    ).fetchone()
    return row["cnt"]


def _emit_and_process(db: "EventsDB", policies: list,
                      event_type: str, payload: dict, max_ticks: int = 5) -> None:
    """Insert an event into the DB and run daemon ticks until no unprocessed events remain.

    Multi-tick processing is needed for policies with retry steps that emit follow-up events
    (e.g. boi.completion.retry-verify) that must be processed in subsequent ticks.
    """
    db.insert_event(event_type, json.dumps(payload), "test")
    for _ in range(max_ticks):
        events = db.get_unprocessed()
        if not events:
            break
        for event in events:
            _process_event_policies(event, policies, db)


def test_integration_pipeline_dirty_repo_emits_violation():
    """Full pipeline: boi.spec.completed on dirty repo → policy.violation emitted."""
    policies = _load_gate_policy_list()
    assert policies, "boi-completion-gate policy not found in ~/.hex-events/policies"

    os.environ["HEX_EVENTS_HOME"] = _REPO_ROOT

    with tempfile.TemporaryDirectory() as tmp:
        repo = make_dirty_repo(tmp)
        db = _make_temp_db(tmp)

        _emit_and_process(db, policies, "boi.output.committed", {
            "spec_id": "q-integration-test",
            "target_repo": repo,
            "tasks_done": 3,
            "tasks_total": 5,
        })

        violation_count = _count_events(db, "policy.violation")
        verified_count = _count_events(db, "boi.completion.verified")

        db.close()

    assert violation_count >= 1, (
        "Expected policy.violation to be emitted for dirty repo, got 0"
    )
    assert verified_count == 0, (
        "Expected no boi.completion.verified for dirty repo"
    )


def test_integration_pipeline_clean_repo_emits_verified():
    """Full pipeline: boi.spec.completed on clean repo → boi.completion.verified emitted."""
    policies = _load_gate_policy_list()
    assert policies, "boi-completion-gate policy not found in ~/.hex-events/policies"

    os.environ["HEX_EVENTS_HOME"] = _REPO_ROOT

    with tempfile.TemporaryDirectory() as tmp:
        remote = make_bare_remote(tmp)
        repo = make_clean_repo(tmp, remote)
        db = _make_temp_db(tmp)

        _emit_and_process(db, policies, "boi.output.committed", {
            "spec_id": "q-integration-clean",
            "target_repo": repo,
            "tasks_done": 5,
            "tasks_total": 5,
        })

        violation_count = _count_events(db, "policy.violation")
        verified_count = _count_events(db, "boi.completion.verified")

        db.close()

    assert verified_count >= 1, (
        "Expected boi.completion.verified to be emitted for clean repo, got 0"
    )
    assert violation_count == 0, (
        "Expected no policy.violation for clean repo"
    )


def test_integration_pipeline_dirty_then_clean():
    """Full scenario: dirty repo fails, commit+push, clean repo succeeds."""
    policies = _load_gate_policy_list()
    assert policies, "boi-completion-gate policy not found"

    os.environ["HEX_EVENTS_HOME"] = _REPO_ROOT

    with tempfile.TemporaryDirectory() as tmp:
        db = _make_temp_db(tmp)

        # Step 1: dirty repo → violation
        repo = make_dirty_repo(tmp)
        _emit_and_process(db, policies, "boi.output.committed", {
            "spec_id": "q-scenario-test",
            "target_repo": repo,
            "tasks_done": 3,
            "tasks_total": 5,
        })
        assert _count_events(db, "policy.violation") >= 1

        # Step 2: simulate what auto-commit would have done — commit dirty files,
        # restore the remote, and push so the repo is clean for the next verify.
        real_remote = os.path.join(tmp, "remote.git")
        subprocess.run(
            ["git", "-C", repo, "remote", "set-url", "origin", real_remote],
            check=True, capture_output=True
        )
        subprocess.run(["git", "-C", repo, "add", "."], check=True, capture_output=True)
        subprocess.run(
            ["git", "-C", repo, "commit", "-m", "auto-commit boi output"],
            check=True, capture_output=True
        )
        subprocess.run(
            ["git", "-C", repo, "push", "-u", "origin", "HEAD"],
            check=True, capture_output=True
        )

        # Step 3: clean repo → verified
        _emit_and_process(db, policies, "boi.output.committed", {
            "spec_id": "q-scenario-test",
            "target_repo": repo,
            "tasks_done": 5,
            "tasks_total": 5,
        })
        assert _count_events(db, "boi.completion.verified") >= 1

        db.close()
