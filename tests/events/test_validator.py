"""Tests for validator.py — static graph validation and observed graph (t-6). TDD first."""
import os
import sys
import tempfile

import pytest
import yaml

sys.path.insert(0, os.path.expanduser("~/.hex-events"))

from validator import (
    build_static_graph,
    validate_graph,
    get_observed_events,
    compare_graphs,
    _detect_cycles,
)
from policy import Policy, Rule, load_policies
from recipe import Action, Condition
from db import EventsDB


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def make_policy(name, trigger, provides_events=None, requires_events=None, emit_actions=None):
    """Build a minimal Policy for testing."""
    actions = []
    if emit_actions:
        for evt in emit_actions:
            actions.append(Action(type="emit", params={"event": evt}))
    rule = Rule(name=f"{name}.rule", trigger_event=trigger, actions=actions)
    return Policy(
        name=name,
        rules=[rule],
        provides={"events": provides_events} if provides_events else {},
        requires={"events": requires_events} if requires_events else {},
    )


@pytest.fixture
def db():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    d = EventsDB(path)
    yield d
    d.close()
    os.unlink(path)


# ---------------------------------------------------------------------------
# build_static_graph
# ---------------------------------------------------------------------------

def test_build_static_graph_basic():
    policies = [
        make_policy("p1", trigger="git.commit", provides_events=["check.due"]),
        make_policy("p2", trigger="check.due", provides_events=["policy.violation"]),
    ]
    graph = build_static_graph(policies)
    assert "check.due" in graph["provided_by"]
    assert "p1" in graph["provided_by"]["check.due"]
    assert "check.due" in graph["required_by"]
    assert "p2" in graph["required_by"]["check.due"]


def test_build_static_graph_adapter_events():
    policies = []
    adapter_events = {"timer.tick.30m", "timer.tick.1h"}
    graph = build_static_graph(policies, adapter_events=adapter_events)
    assert "timer.tick.30m" in graph["provided_by"]
    assert "@adapter:scheduler" in graph["provided_by"]["timer.tick.30m"]


def test_build_static_graph_infers_requires_from_rule_trigger():
    """If requires is empty, trigger events from rules are used."""
    p = make_policy("p1", trigger="git.push", provides_events=["done.event"])
    # No explicit requires set
    p.requires = {}
    graph = build_static_graph([p])
    assert "git.push" in graph["required_by"]


def test_build_static_graph_edges():
    policies = [
        make_policy("pol", trigger="A", provides_events=["B"], requires_events=["A"]),
    ]
    graph = build_static_graph(policies)
    assert ("A", "B", "pol") in graph["edges"]


# ---------------------------------------------------------------------------
# validate_graph — satisfied requires
# ---------------------------------------------------------------------------

def test_validate_satisfied_requires():
    """No unsatisfied requires when all triggers are provided by someone."""
    policies = [
        make_policy("producer", trigger="external.event",
                    provides_events=["check.due"], requires_events=["external.event"]),
        make_policy("consumer", trigger="check.due",
                    provides_events=["done"], requires_events=["check.due"]),
    ]
    adapter_events = {"external.event"}
    graph = build_static_graph(policies, adapter_events=adapter_events)
    result = validate_graph(graph)
    assert result["unsatisfied"] == []


def test_validate_unsatisfied_requires():
    """Missing provider → shows up in unsatisfied."""
    policies = [
        make_policy("consumer", trigger="ghost.event",
                    provides_events=["done"], requires_events=["ghost.event"]),
    ]
    graph = build_static_graph(policies)
    result = validate_graph(graph)
    unsatisfied_events = [u["event"] for u in result["unsatisfied"]]
    assert "ghost.event" in unsatisfied_events


def test_validate_unsatisfied_includes_policy_name():
    """The unsatisfied entry names the policy that requires the event."""
    p = make_policy("needy", trigger="missing.evt",
                    requires_events=["missing.evt"], provides_events=[])
    graph = build_static_graph([p])
    result = validate_graph(graph)
    assert any(
        "needy" in u["required_by"]
        for u in result["unsatisfied"]
        if u["event"] == "missing.evt"
    )


def test_validate_satisfied_by_adapter():
    """Adapter-provided events count as satisfying requires."""
    p = make_policy("timer-consumer", trigger="timer.tick.30m",
                    requires_events=["timer.tick.30m"], provides_events=["timer.done"])
    adapter_events = {"timer.tick.30m"}
    graph = build_static_graph([p], adapter_events=adapter_events)
    result = validate_graph(graph)
    assert result["unsatisfied"] == []


# ---------------------------------------------------------------------------
# validate_graph — orphan detection
# ---------------------------------------------------------------------------

def test_validate_orphan_provides():
    """A provided event with no consumer is an orphan."""
    p = make_policy("lone-provider", trigger="external",
                    provides_events=["orphan.event"], requires_events=["external"])
    adapter_events = {"external"}
    graph = build_static_graph([p], adapter_events=adapter_events)
    result = validate_graph(graph)
    orphan_events = [o["event"] for o in result["orphan_provides"]]
    assert "orphan.event" in orphan_events


def test_validate_no_orphan_when_consumed():
    """No orphan when the provided event is consumed by another policy."""
    policies = [
        make_policy("a", trigger="ext", provides_events=["mid"],
                    requires_events=["ext"]),
        make_policy("b", trigger="mid", provides_events=["final"],
                    requires_events=["mid"]),
    ]
    adapter_events = {"ext"}
    graph = build_static_graph(policies, adapter_events=adapter_events)
    result = validate_graph(graph)
    orphan_events = [o["event"] for o in result["orphan_provides"]]
    assert "mid" not in orphan_events


# ---------------------------------------------------------------------------
# validate_graph — cycle detection
# ---------------------------------------------------------------------------

def test_validate_no_cycles_linear():
    policies = [
        make_policy("a", trigger="ext", provides_events=["b.evt"],
                    requires_events=["ext"]),
        make_policy("b", trigger="b.evt", provides_events=["c.evt"],
                    requires_events=["b.evt"]),
    ]
    graph = build_static_graph(policies, adapter_events={"ext"})
    result = validate_graph(graph)
    assert result["cycles"] == []


def test_validate_detects_two_node_cycle():
    """A → B → A should be detected as a cycle."""
    policies = [
        make_policy("a", trigger="b.evt", provides_events=["a.evt"],
                    requires_events=["b.evt"]),
        make_policy("b", trigger="a.evt", provides_events=["b.evt"],
                    requires_events=["a.evt"]),
    ]
    graph = build_static_graph(policies)
    result = validate_graph(graph)
    assert len(result["cycles"]) > 0


def test_validate_detects_three_node_cycle():
    """A → B → C → A cycle."""
    policies = [
        make_policy("a", trigger="c.evt", provides_events=["a.evt"], requires_events=["c.evt"]),
        make_policy("b", trigger="a.evt", provides_events=["b.evt"], requires_events=["a.evt"]),
        make_policy("c", trigger="b.evt", provides_events=["c.evt"], requires_events=["b.evt"]),
    ]
    graph = build_static_graph(policies)
    result = validate_graph(graph)
    assert len(result["cycles"]) > 0


def test_detect_cycles_empty_graph():
    assert _detect_cycles({}) == []


def test_detect_cycles_no_cycle():
    adj = {"A": {"B"}, "B": {"C"}}
    assert _detect_cycles(adj) == []


def test_detect_cycles_self_loop():
    adj = {"A": {"A"}}
    cycles = _detect_cycles(adj)
    assert len(cycles) > 0


# ---------------------------------------------------------------------------
# validate_graph — valid flag
# ---------------------------------------------------------------------------

def test_validate_valid_flag_true():
    """valid=True when no unsatisfied and no cycles."""
    p = make_policy("p", trigger="ext", provides_events=["done"],
                    requires_events=["ext"])
    graph = build_static_graph([p], adapter_events={"ext"})
    result = validate_graph(graph)
    assert result["valid"] is True


def test_validate_valid_flag_false_on_unsatisfied():
    p = make_policy("p", trigger="missing", requires_events=["missing"], provides_events=[])
    graph = build_static_graph([p])
    result = validate_graph(graph)
    assert result["valid"] is False


def test_validate_valid_flag_false_on_cycle():
    policies = [
        make_policy("a", trigger="b.evt", provides_events=["a.evt"], requires_events=["b.evt"]),
        make_policy("b", trigger="a.evt", provides_events=["b.evt"], requires_events=["a.evt"]),
    ]
    graph = build_static_graph(policies)
    result = validate_graph(graph)
    assert result["valid"] is False


# ---------------------------------------------------------------------------
# get_observed_events
# ---------------------------------------------------------------------------

def test_get_observed_events_counts(db):
    db.insert_event("git.commit", '{}', "test")
    db.insert_event("git.commit", '{}', "test")
    db.insert_event("file.modified", '{}', "test")
    observed = get_observed_events(db, days=7)
    assert observed["event_counts"]["git.commit"] == 2
    assert observed["event_counts"]["file.modified"] == 1


def test_get_observed_events_empty_db(db):
    observed = get_observed_events(db, days=7)
    assert observed["event_counts"] == {}
    assert observed["policy_triggers"] == {}


def test_get_observed_events_policy_triggers(db):
    eid = db.insert_event("git.commit", '{}', "test")
    db.mark_processed(eid, recipe="my-policy")
    db.log_action(eid, "my-policy", "emit", "landings.check-due", "ok")
    observed = get_observed_events(db, days=7)
    assert "git.commit" in observed["policy_triggers"]
    policies = [p["policy"] for p in observed["policy_triggers"]["git.commit"]]
    assert "my-policy" in policies


# ---------------------------------------------------------------------------
# compare_graphs
# ---------------------------------------------------------------------------

def test_compare_graphs_in_static_only():
    policies = [make_policy("p", trigger="a.evt", provides_events=["b.evt"],
                             requires_events=["a.evt"])]
    graph = build_static_graph(policies, adapter_events={"a.evt"})
    observed = {"event_counts": {}, "policy_triggers": {}}
    cmp = compare_graphs(graph, observed)
    assert "a.evt" in cmp["in_static_only"] or "b.evt" in cmp["in_static_only"]


def test_compare_graphs_in_observed_only(db):
    db.insert_event("unknown.event", '{}', "external")
    observed = get_observed_events(db, days=7)
    graph = build_static_graph([])
    cmp = compare_graphs(graph, observed)
    assert "unknown.event" in cmp["in_observed_only"]


def test_compare_graphs_in_both():
    policies = [make_policy("p", trigger="known.event", provides_events=["done"],
                             requires_events=["known.event"])]
    adapter_events = {"known.event"}
    graph = build_static_graph(policies, adapter_events=adapter_events)
    observed = {"event_counts": {"known.event": 5, "done": 3}, "policy_triggers": {}}
    cmp = compare_graphs(graph, observed)
    assert "known.event" in cmp["in_both"]
