"""Green-path tests for the hex-events static compiler.

All fixtures use pytest tmp_path. No writes to ~/.hex-events/.
"""
import os
import textwrap
import pytest

from validators import schema as schema_v
from validators import producer_check as producer_v
from validators import deadcode as deadcode_v


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _catalog(*events):
    """Build a minimal catalog dict with the given events as scheduler-produced."""
    return {evt: {"producers": [{"kind": "scheduler", "name": evt}], "consumers": []} for evt in events}


def _run_all(path: str, catalog: dict) -> list[dict]:
    issues = []
    issues += schema_v.validate(path)
    issues += producer_v.validate(path, catalog)
    issues += deadcode_v.validate(path)
    return issues


def _errors(issues):
    return [i for i in issues if i["severity"] == "error"]


# ---------------------------------------------------------------------------
# Synthetic good-policy fixture
# ---------------------------------------------------------------------------

GOOD_POLICY = textwrap.dedent("""\
    name: test-good-policy
    description: A fully valid policy with all optional fields

    requires:
      events:
        - timer.tick.hourly

    provides:
      events:
        - my.namespace.done

    rate_limit:
      window: 2h
      max: 1

    rules:
      - name: hourly-probe
        trigger:
          event: timer.tick.hourly
        actions:
          - type: shell
            command: echo hello
""")


def test_synthetic_good_policy_compiles_clean(tmp_path):
    """Synthetic policy with all valid constructs produces zero errors."""
    p = tmp_path / "good.yaml"
    p.write_text(GOOD_POLICY)
    catalog = _catalog("timer.tick.hourly")
    issues = _run_all(str(p), catalog)
    assert _errors(issues) == [], f"Unexpected errors: {_errors(issues)}"


def test_synthetic_good_policy_no_warnings(tmp_path):
    """Synthetic policy produces zero warnings (rate_limit window > cadence)."""
    p = tmp_path / "good.yaml"
    p.write_text(GOOD_POLICY)
    catalog = _catalog("timer.tick.hourly")
    issues = _run_all(str(p), catalog)
    assert issues == [], f"Unexpected issues: {issues}"


# ---------------------------------------------------------------------------
# Real kalshi bundle (source file, not operational copy)
# ---------------------------------------------------------------------------

# Override-able for forks / CI. Skips cleanly when the dev-only path is absent.
KALSHI_POLICY = os.environ.get(
    "HEX_EVENTS_TEST_KALSHI_POLICY",
    os.path.expanduser("~/mrap-hex/integrations/kalshi/events/key-rotation-reminder.yaml"),
)


def test_real_kalshi_policy_compiles_clean():
    """Real kalshi source policy compiles clean (timer.tick.weekly is a known producer)."""
    if not os.path.exists(KALSHI_POLICY):
        pytest.skip(
            f"Kalshi policy fixture not present at {KALSHI_POLICY}. "
            "Set HEX_EVENTS_TEST_KALSHI_POLICY to point at any real bundle policy, "
            "or ignore on CI/forks."
        )
    catalog = _catalog("timer.tick.weekly")
    issues = _run_all(KALSHI_POLICY, catalog)
    errors = _errors(issues)
    assert errors == [], f"Unexpected errors in kalshi policy: {errors}"


# ---------------------------------------------------------------------------
# Corpus-level: two distinct good policies have no duplicate names
# ---------------------------------------------------------------------------

def test_no_duplicate_policy_names_across_two_good_policies(tmp_path):
    """Two policies with distinct names produce no DUPLICATE_POLICY_NAME errors."""
    p1 = tmp_path / "pol1.yaml"
    p2 = tmp_path / "pol2.yaml"
    p1.write_text(textwrap.dedent("""\
        name: alpha-policy
        description: first
        rules:
          - name: rule-a
            trigger:
              event: timer.tick.daily
            actions:
              - type: shell
                command: echo a
    """))
    p2.write_text(textwrap.dedent("""\
        name: beta-policy
        description: second
        rules:
          - name: rule-b
            trigger:
              event: timer.tick.daily
            actions:
              - type: shell
                command: echo b
    """))
    catalog = _catalog("timer.tick.daily")
    corpus_issues = deadcode_v.validate_corpus([str(p1), str(p2)])
    assert corpus_issues == [], f"Unexpected corpus issues: {corpus_issues}"


# ---------------------------------------------------------------------------
# Multi-rule valid policy
# ---------------------------------------------------------------------------

def test_multi_rule_policy_compiles_clean(tmp_path):
    """Policy with multiple distinct rules compiles clean."""
    p = tmp_path / "multi.yaml"
    p.write_text(textwrap.dedent("""\
        name: multi-rule-policy
        description: has two distinct rules
        rules:
          - name: first-rule
            trigger:
              event: timer.tick.daily
            actions:
              - type: shell
                command: echo first
          - name: second-rule
            trigger:
              event: timer.tick.weekly
            actions:
              - type: notify
                message: weekly ping
    """))
    catalog = _catalog("timer.tick.daily", "timer.tick.weekly")
    issues = _run_all(str(p), catalog)
    assert _errors(issues) == [], f"Unexpected errors: {_errors(issues)}"
