"""Test that emit action renders Jinja2 templates in dict payload values."""
import json
import pytest
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))

from db import EventsDB
from actions.emit import EmitAction


@pytest.fixture
def db(tmp_path):
    return EventsDB(str(tmp_path / "test.db"))


def test_dict_payload_templates_are_rendered(db):
    """Dict payload values containing {{ }} must be Jinja2-rendered."""
    emitter = EmitAction()
    result = emitter.run(
        params={
            "event": "test.rendered",
            "payload": {
                "spec_id": "{{ event.spec_id }}",
                "literal": "no-template-here",
            },
        },
        event_payload={"spec_id": "q-123", "other": "data"},
        db=db,
    )
    assert result["status"] == "success"

    row = db.conn.execute(
        "SELECT payload FROM events WHERE event_type = 'test.rendered'"
    ).fetchone()
    payload = json.loads(row["payload"])
    assert payload["spec_id"] == "q-123", f"Template not rendered: {payload}"
    assert payload["literal"] == "no-template-here"


def test_dict_payload_without_templates_unchanged(db):
    """Dict payloads with no templates should pass through unchanged."""
    emitter = EmitAction()
    result = emitter.run(
        params={
            "event": "test.plain",
            "payload": {"key": "value", "num": 42},
        },
        event_payload={},
        db=db,
    )
    assert result["status"] == "success"
    row = db.conn.execute(
        "SELECT payload FROM events WHERE event_type = 'test.plain'"
    ).fetchone()
    payload = json.loads(row["payload"])
    assert payload == {"key": "value", "num": 42}


def test_cancel_group_template_rendered(db):
    """cancel_group containing {{ }} must be rendered."""
    emitter = EmitAction()
    result = emitter.run(
        params={
            "event": "test.deferred",
            "payload": {"id": "{{ event.id }}"},
            "delay": "1m",
            "cancel_group": "check-{{ event.id }}",
        },
        event_payload={"id": "q-999"},
        db=db,
    )
    assert result["status"] == "success"

    row = db.conn.execute("SELECT * FROM deferred_events").fetchone()
    payload = json.loads(row["payload"])
    assert payload["id"] == "q-999"
    assert row["cancel_group"] == "check-q-999"
