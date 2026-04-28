"""Red-path tests for the hex-events static compiler.

One test per validator error code. All fixtures use pytest tmp_path.
No writes to ~/.hex-events/.
"""
import textwrap
import pytest

from validators import schema as schema_v
from validators import producer_check as producer_v
from validators import deadcode as deadcode_v


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _catalog(*events):
    return {evt: {"producers": [{"kind": "scheduler", "name": evt}], "consumers": []} for evt in events}


def _empty_catalog():
    return {}


def _codes(issues):
    return {i["code"] for i in issues}


def _run_all(path: str, catalog: dict) -> list[dict]:
    issues = []
    issues += schema_v.validate(path)
    issues += producer_v.validate(path, catalog)
    issues += deadcode_v.validate(path)
    return issues


# ---------------------------------------------------------------------------
# EVENT_NO_PRODUCER
# ---------------------------------------------------------------------------

def test_event_no_producer(tmp_path):
    """Policy subscribing to timer.tick.never fails with EVENT_NO_PRODUCER."""
    p = tmp_path / "no-producer.yaml"
    p.write_text(textwrap.dedent("""\
        name: no-producer-policy
        description: subscribes to a nonexistent event
        rules:
          - name: will-never-fire
            trigger:
              event: timer.tick.never
            actions:
              - type: shell
                command: echo nope
    """))
    catalog = _empty_catalog()
    issues = _run_all(str(p), catalog)
    assert "EVENT_NO_PRODUCER" in _codes(issues), f"Expected EVENT_NO_PRODUCER, got: {_codes(issues)}"


# ---------------------------------------------------------------------------
# SCHEMA_FLAT_FORM
# ---------------------------------------------------------------------------

def test_schema_flat_form_rejected(tmp_path):
    """Policy using deprecated flat trigger:/action: form fails with SCHEMA_FLAT_FORM."""
    p = tmp_path / "flat.yaml"
    p.write_text(textwrap.dedent("""\
        name: flat-form-policy
        description: deprecated shape
        trigger:
          event: timer.tick.daily
        action:
          type: shell
          command: echo old
    """))
    catalog = _catalog("timer.tick.daily")
    issues = schema_v.validate(str(p))
    assert "SCHEMA_FLAT_FORM" in _codes(issues), f"Expected SCHEMA_FLAT_FORM, got: {_codes(issues)}"


# ---------------------------------------------------------------------------
# DUPLICATE_RULE_NAME
# ---------------------------------------------------------------------------

def test_duplicate_rule_name(tmp_path):
    """Policy with two rules sharing the same name fails with DUPLICATE_RULE_NAME."""
    p = tmp_path / "dup-rule.yaml"
    p.write_text(textwrap.dedent("""\
        name: dup-rule-policy
        description: two rules named foo
        rules:
          - name: foo
            trigger:
              event: timer.tick.daily
            actions:
              - type: shell
                command: echo first
          - name: foo
            trigger:
              event: timer.tick.daily
            actions:
              - type: shell
                command: echo second
    """))
    catalog = _catalog("timer.tick.daily")
    issues = _run_all(str(p), catalog)
    assert "DUPLICATE_RULE_NAME" in _codes(issues), f"Expected DUPLICATE_RULE_NAME, got: {_codes(issues)}"


# ---------------------------------------------------------------------------
# DUPLICATE_POLICY_NAME
# ---------------------------------------------------------------------------

def test_duplicate_policy_name(tmp_path):
    """Two policies with the same name fail with DUPLICATE_POLICY_NAME in corpus check."""
    p1 = tmp_path / "pol-a.yaml"
    p2 = tmp_path / "pol-b.yaml"
    shared = textwrap.dedent("""\
        name: foo
        description: same name used twice
        rules:
          - name: only-rule
            trigger:
              event: timer.tick.daily
            actions:
              - type: shell
                command: echo x
    """)
    p1.write_text(shared)
    p2.write_text(shared)
    issues = deadcode_v.validate_corpus([str(p1), str(p2)])
    assert "DUPLICATE_POLICY_NAME" in _codes(issues), f"Expected DUPLICATE_POLICY_NAME, got: {_codes(issues)}"


# ---------------------------------------------------------------------------
# UNKNOWN_ACTION_TYPE
# ---------------------------------------------------------------------------

def test_unknown_action_type(tmp_path):
    """Policy with action type 'bogus' fails with UNKNOWN_ACTION_TYPE."""
    p = tmp_path / "bogus-type.yaml"
    p.write_text(textwrap.dedent("""\
        name: bogus-action-policy
        description: uses an unknown action type
        rules:
          - name: bad-action
            trigger:
              event: timer.tick.daily
            actions:
              - type: bogus
                command: echo should-fail
    """))
    catalog = _catalog("timer.tick.daily")
    issues = _run_all(str(p), catalog)
    assert "UNKNOWN_ACTION_TYPE" in _codes(issues), f"Expected UNKNOWN_ACTION_TYPE, got: {_codes(issues)}"


# ---------------------------------------------------------------------------
# NO_ACTIONS
# ---------------------------------------------------------------------------

def test_no_actions(tmp_path):
    """Rule with empty actions list fails with NO_ACTIONS."""
    p = tmp_path / "no-actions.yaml"
    p.write_text(textwrap.dedent("""\
        name: no-actions-policy
        description: rule has empty actions
        rules:
          - name: empty-rule
            trigger:
              event: timer.tick.daily
            actions: []
    """))
    catalog = _catalog("timer.tick.daily")
    issues = _run_all(str(p), catalog)
    assert "NO_ACTIONS" in _codes(issues), f"Expected NO_ACTIONS, got: {_codes(issues)}"


# ---------------------------------------------------------------------------
# RATE_LIMIT_CADENCE_MISMATCH (warning, not error)
# ---------------------------------------------------------------------------

def test_rate_limit_cadence_mismatch_is_warning(tmp_path):
    """Rate_limit window shorter than trigger cadence produces RATE_LIMIT_CADENCE_MISMATCH warning."""
    p = tmp_path / "cadence-mismatch.yaml"
    p.write_text(textwrap.dedent("""\
        name: cadence-mismatch-policy
        description: rate_limit window is shorter than hourly trigger
        rate_limit:
          window: 30m
          max: 1
        rules:
          - name: hourly-rule
            trigger:
              event: timer.tick.hourly
            actions:
              - type: shell
                command: echo tick
    """))
    catalog = _catalog("timer.tick.hourly")
    issues = _run_all(str(p), catalog)
    codes = _codes(issues)
    assert "RATE_LIMIT_CADENCE_MISMATCH" in codes, f"Expected RATE_LIMIT_CADENCE_MISMATCH, got: {codes}"
    # Must be a warning, not an error
    mismatch_issues = [i for i in issues if i["code"] == "RATE_LIMIT_CADENCE_MISMATCH"]
    assert all(i["severity"] == "warning" for i in mismatch_issues), \
        "RATE_LIMIT_CADENCE_MISMATCH must be a warning, not an error"
