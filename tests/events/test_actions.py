# test_actions.py
import json
import os
import tempfile
import pytest
from actions import get_action_handler
from actions.shell import ShellAction
from actions.update_file import UpdateFileAction

def test_get_shell_handler():
    handler = get_action_handler("shell")
    assert handler is not None

def test_shell_action_echo():
    action = ShellAction()
    result = action.run({"command": "echo hello"}, event_payload={})
    assert result["status"] == "success"
    assert "hello" in result["output"]

def test_shell_action_failure():
    action = ShellAction()
    result = action.run({"command": "false"}, event_payload={})
    assert result["status"] == "error"

def test_shell_action_template():
    action = ShellAction()
    result = action.run(
        {"command": "echo {{ event.name }}"},
        event_payload={"name": "test-event"},
    )
    assert result["status"] == "success"
    assert "test-event" in result["output"]

def test_update_file_action(tmp_path):
    target = tmp_path / "test.md"
    target.write_text("Status: Pending\nOther: line\n")
    action = UpdateFileAction()
    result = action.run(
        {"target": str(target), "pattern": "Status: Pending", "replace": "Status: Done ✓"},
        event_payload={},
    )
    assert result["status"] == "success"
    assert "Done ✓" in target.read_text()

def test_get_unknown_handler():
    handler = get_action_handler("nonexistent")
    assert handler is None
