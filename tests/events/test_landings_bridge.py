"""Tests for update-landings-from-boi.sh (landings bridge script).

Tests:
  1. Script adds sub-item to matching landing on dispatch
  2. Script updates status to Done on completion
  3. Script updates status to Failed on failure
  4. Script creates new landing when no match found
  5. Script appends to changelog
  6. Script handles missing landings file gracefully
  7. Keyword matching: "hex-ops auto-remediation" matches exact landing name
  8. Keyword matching: "polymarket-dashboard" vs "Polymarket paper trader" — 1-word overlap,
     no match → creates new landing (documents current threshold=2 behavior)
  9. Idempotency: dispatching same spec twice doesn't create duplicate sub-items
"""
import os
import stat
import subprocess
import textwrap
from pathlib import Path

import pytest

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPT_PATH = os.path.join(_REPO_ROOT, "scripts", "update-landings-from-boi.sh")
TEST_DATE = "2026-03-19"


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def tmp_home(tmp_path):
    """Set up a fake HOME directory with today.sh, landings dir, and .boi dir.

    The update-landings-from-boi.sh script looks for today.sh at
    $HOME/hex/.claude/scripts/today.sh (using HEX_HOME=$HOME/hex by default).
    """
    scripts_dir = tmp_path / "hex" / ".claude" / "scripts"
    scripts_dir.mkdir(parents=True)
    today_sh = scripts_dir / "today.sh"
    today_sh.write_text(f"#!/usr/bin/env bash\necho '{TEST_DATE}'\n")
    today_sh.chmod(today_sh.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    landings_dir = tmp_path / "hex" / "landings"
    landings_dir.mkdir(parents=True)

    boi_dir = tmp_path / ".boi"
    boi_dir.mkdir(parents=True)

    return tmp_path


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def run_script(tmp_home, event_type, spec_id, spec_title, tasks_done="0", tasks_total="1"):
    env = os.environ.copy()
    env["HOME"] = str(tmp_home)
    return subprocess.run(
        ["bash", SCRIPT_PATH, event_type, spec_id, spec_title, tasks_done, tasks_total],
        capture_output=True,
        text=True,
        env=env,
    )


def get_landings_file(tmp_home):
    return tmp_home / "hex" / "landings" / f"{TEST_DATE}.md"


def write_landings(tmp_home, content):
    lf = get_landings_file(tmp_home)
    lf.write_text(content)
    return lf


# ---------------------------------------------------------------------------
# Sample landings files
# ---------------------------------------------------------------------------

HEX_OPS_LANDINGS = textwrap.dedent("""\
    # Landings 2026-03-19

    ### L1. hex-ops auto-remediation
    **Priority:** L1
    **Status:** In Progress

    | Sub-item | Owner | Action | Status |
    |----------|-------|--------|--------|
    | Initial task | hex | Plan | Done ✓ |

    ## Changelog
    - 09:00 — session started
""")

POLYMARKET_LANDINGS = textwrap.dedent("""\
    # Landings 2026-03-19

    ### L1. Polymarket paper trader
    **Priority:** L1
    **Status:** In Progress

    | Sub-item | Owner | Action | Status |
    |----------|-------|--------|--------|
    | Initial task | hex | Plan | Done ✓ |

    ## Changelog
    - 09:00 — session started
""")

NO_MATCH_LANDINGS = textwrap.dedent("""\
    # Landings 2026-03-19

    ### L1. Something completely different
    **Priority:** L1
    **Status:** In Progress

    | Sub-item | Owner | Action | Status |
    |----------|-------|--------|--------|
    | Initial task | hex | Plan | Done ✓ |

    ## Changelog
    - 09:00 — session started
""")


# ---------------------------------------------------------------------------
# Test 1: dispatch adds sub-item to matching landing
# ---------------------------------------------------------------------------

def test_dispatch_adds_subitem_to_matching_landing(tmp_home):
    lf = write_landings(tmp_home, HEX_OPS_LANDINGS)
    result = run_script(tmp_home, "dispatched", "q-159", "hex-ops auto-remediation bridge")

    assert result.returncode == 0, f"Script failed:\n{result.stderr}"
    content = lf.read_text()
    assert "BOI q-159:" in content
    assert "In Progress" in content


# ---------------------------------------------------------------------------
# Test 2: completion updates status to Done
# ---------------------------------------------------------------------------

def test_completion_updates_status_to_done(tmp_home):
    lf = write_landings(tmp_home, HEX_OPS_LANDINGS)
    run_script(tmp_home, "dispatched", "q-159", "hex-ops auto-remediation bridge")
    result = run_script(tmp_home, "completed", "q-159", "hex-ops auto-remediation bridge", "3", "3")

    assert result.returncode == 0, f"Script failed:\n{result.stderr}"
    content = lf.read_text()
    # The row for q-159 should now show Done ✓
    boi_row = [line for line in content.splitlines() if "BOI q-159:" in line]
    assert len(boi_row) == 1
    assert "Done ✓" in boi_row[0]
    assert "In Progress" not in boi_row[0]


# ---------------------------------------------------------------------------
# Test 3: failure updates status to Failed
# ---------------------------------------------------------------------------

def test_failure_updates_status_to_failed(tmp_home):
    lf = write_landings(tmp_home, HEX_OPS_LANDINGS)
    run_script(tmp_home, "dispatched", "q-159", "hex-ops auto-remediation bridge")
    result = run_script(tmp_home, "failed", "q-159", "hex-ops auto-remediation bridge")

    assert result.returncode == 0, f"Script failed:\n{result.stderr}"
    content = lf.read_text()
    boi_row = [line for line in content.splitlines() if "BOI q-159:" in line]
    assert len(boi_row) == 1
    assert "Failed" in boi_row[0]
    assert "In Progress" not in boi_row[0]


# ---------------------------------------------------------------------------
# Test 4: creates new landing when no match found
# ---------------------------------------------------------------------------

def test_creates_new_landing_on_no_match(tmp_home):
    lf = write_landings(tmp_home, NO_MATCH_LANDINGS)
    result = run_script(tmp_home, "dispatched", "q-999", "totally unrelated boi spec task")

    assert result.returncode == 0, f"Script failed:\n{result.stderr}"
    content = lf.read_text()
    # L2 should be created since L1 had no match
    assert "### L2." in content
    assert "BOI q-999:" in content
    assert "In Progress" in content


# ---------------------------------------------------------------------------
# Test 5: appends to changelog
# ---------------------------------------------------------------------------

def test_appends_to_changelog(tmp_home):
    lf = write_landings(tmp_home, HEX_OPS_LANDINGS)
    run_script(tmp_home, "dispatched", "q-159", "hex-ops auto-remediation bridge")
    content = lf.read_text()

    # Changelog entry should contain spec_id and event type
    changelog_lines = [
        line for line in content.splitlines()
        if "BOI q-159 dispatched:" in line
    ]
    assert len(changelog_lines) >= 1, "No changelog entry found for dispatch"


# ---------------------------------------------------------------------------
# Test 6: missing landings file exits 0 gracefully
# ---------------------------------------------------------------------------

def test_missing_landings_file_exits_gracefully(tmp_home):
    # Do NOT create the landings file
    result = run_script(tmp_home, "dispatched", "q-159", "hex-ops auto-remediation bridge")

    assert result.returncode == 0, f"Expected exit 0 on missing file, got:\n{result.stderr}"
    assert "No landings file" in result.stdout or result.returncode == 0


# ---------------------------------------------------------------------------
# Test 7: keyword matching — "hex-ops auto-remediation" matches exact landing name
# ---------------------------------------------------------------------------

def test_keyword_matching_exact(tmp_home):
    lf = write_landings(tmp_home, HEX_OPS_LANDINGS)
    # "hex-ops auto-remediation" splits to: {hex, ops, auto, remediation}
    # landing "hex-ops auto-remediation": {hex, ops, auto, remediation} → 4-word overlap → match
    result = run_script(tmp_home, "dispatched", "q-158", "hex-ops auto-remediation")

    assert result.returncode == 0, result.stderr
    content = lf.read_text()
    assert "BOI q-158:" in content
    # Should match L1 (not create L2)
    assert "### L2." not in content


# ---------------------------------------------------------------------------
# Test 8: keyword matching — "polymarket-dashboard" vs "Polymarket paper trader"
#         Only 1 significant word overlap ("polymarket"), threshold requires 2+.
#         Expected behavior: no match → new landing L2 created.
# ---------------------------------------------------------------------------

def test_keyword_matching_single_word_no_match(tmp_home):
    lf = write_landings(tmp_home, POLYMARKET_LANDINGS)
    # "polymarket-dashboard" → {polymarket, dashboard}
    # "Polymarket paper trader" → {polymarket, paper, trader}
    # overlap = {polymarket} → 1 word → below threshold of 2 → no match
    result = run_script(tmp_home, "dispatched", "q-200", "polymarket-dashboard")

    assert result.returncode == 0, result.stderr
    content = lf.read_text()
    # With only 1-word overlap, no match → creates L2
    assert "BOI q-200:" in content
    assert "### L2." in content


# ---------------------------------------------------------------------------
# Test 9: idempotency — dispatching same spec twice doesn't create duplicate sub-items
# ---------------------------------------------------------------------------

def test_idempotency_no_duplicate_subitem(tmp_home):
    lf = write_landings(tmp_home, HEX_OPS_LANDINGS)
    run_script(tmp_home, "dispatched", "q-159", "hex-ops auto-remediation bridge")
    run_script(tmp_home, "dispatched", "q-159", "hex-ops auto-remediation bridge")
    content = lf.read_text()

    count = content.count("BOI q-159:")
    assert count == 1, f"Expected 1 sub-item after 2 dispatches, found {count}"
