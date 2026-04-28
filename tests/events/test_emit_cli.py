# test_emit_cli.py
import json
import os
import sys
import tempfile
import subprocess
import pytest
from db import EventsDB

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


@pytest.fixture
def db_path():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    yield path
    os.unlink(path)

def test_emit_inserts_event(db_path):
    """Test hex-emit inserts an event into the database."""
    emit_script = os.path.join(_REPO_ROOT, "hex_emit.py")
    result = subprocess.run(
        [sys.executable, emit_script, "--db", db_path, "test.event", '{"key":"val"}', "test-source"],
        capture_output=True, text=True,
    )
    assert result.returncode == 0
    db = EventsDB(db_path)
    events = db.history(limit=1)
    assert len(events) == 1
    assert events[0]["event_type"] == "test.event"
    assert json.loads(events[0]["payload"])["key"] == "val"
    db.close()
