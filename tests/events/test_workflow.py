# test_workflow.py
"""Tests for workflow grouping: directory-based policy loading, enable/disable,
shared config, and backward compatibility."""
import os
import shutil
import tempfile
import yaml
import pytest
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from policy import load_policies, _load_workflow_config, _is_workflow_disabled
from actions.shell import ShellAction
from actions.emit import EmitAction
from actions.update_file import UpdateFileAction
from actions.notify import NotifyAction


@pytest.fixture
def policies_dir(tmp_path):
    """Create a temporary policies directory with test data."""
    d = tmp_path / "policies"
    d.mkdir()
    return str(d)


def _write_yaml(path, data):
    with open(path, "w") as f:
        yaml.dump(data, f)


def _make_policy(name, trigger_event="test.event"):
    """Return a minimal old-format policy dict."""
    return {
        "name": name,
        "trigger": {"event": trigger_event},
        "actions": [{"type": "emit", "event": f"{name}.done"}],
    }


def _make_new_policy(name, trigger_event="test.event"):
    """Return a minimal new-format policy dict."""
    return {
        "name": name,
        "rules": [{
            "name": f"{name}-rule",
            "trigger": {"event": trigger_event},
            "actions": [{"type": "emit", "event": f"{name}.done"}],
        }],
    }


class TestStandalonePolicies:
    """Standalone (root-level) policies must work unchanged."""

    def test_load_flat_policies(self, policies_dir):
        _write_yaml(os.path.join(policies_dir, "policy-a.yaml"), _make_policy("policy-a"))
        _write_yaml(os.path.join(policies_dir, "policy-b.yaml"), _make_policy("policy-b"))

        policies = load_policies(policies_dir)
        assert len(policies) == 2
        names = {p.name for p in policies}
        assert names == {"policy-a", "policy-b"}

    def test_standalone_has_no_workflow(self, policies_dir):
        _write_yaml(os.path.join(policies_dir, "solo.yaml"), _make_policy("solo"))

        policies = load_policies(policies_dir)
        assert len(policies) == 1
        assert policies[0].workflow is None
        assert policies[0].workflow_config == {}

    def test_new_format_standalone(self, policies_dir):
        _write_yaml(os.path.join(policies_dir, "new.yaml"), _make_new_policy("new"))

        policies = load_policies(policies_dir)
        assert len(policies) == 1
        assert policies[0].name == "new"
        assert policies[0].workflow is None


class TestWorkflowLoading:
    """Policies inside workflow directories get workflow metadata."""

    def test_workflow_policies_loaded(self, policies_dir):
        wf_dir = os.path.join(policies_dir, "my-workflow")
        os.makedirs(wf_dir)
        _write_yaml(os.path.join(wf_dir, "_config.yaml"), {
            "name": "my-workflow",
            "description": "test workflow",
            "enabled": True,
            "config": {"key1": "val1"},
        })
        _write_yaml(os.path.join(wf_dir, "p1.yaml"), _make_policy("p1"))
        _write_yaml(os.path.join(wf_dir, "p2.yaml"), _make_policy("p2"))

        policies = load_policies(policies_dir)
        assert len(policies) == 2
        for p in policies:
            assert p.workflow == "my-workflow"
            assert p.workflow_config == {"key1": "val1"}

    def test_workflow_without_config(self, policies_dir):
        """A directory without _config.yaml is still a workflow (name = dir name)."""
        wf_dir = os.path.join(policies_dir, "bare-wf")
        os.makedirs(wf_dir)
        _write_yaml(os.path.join(wf_dir, "p1.yaml"), _make_policy("p1"))

        policies = load_policies(policies_dir)
        assert len(policies) == 1
        assert policies[0].workflow == "bare-wf"
        assert policies[0].workflow_config == {}

    def test_config_name_overrides_dirname(self, policies_dir):
        wf_dir = os.path.join(policies_dir, "dir-name")
        os.makedirs(wf_dir)
        _write_yaml(os.path.join(wf_dir, "_config.yaml"), {
            "name": "config-name",
            "enabled": True,
        })
        _write_yaml(os.path.join(wf_dir, "p1.yaml"), _make_policy("p1"))

        policies = load_policies(policies_dir)
        assert policies[0].workflow == "config-name"

    def test_underscore_files_skipped(self, policies_dir):
        """Files starting with _ are reserved for workflow metadata."""
        wf_dir = os.path.join(policies_dir, "wf")
        os.makedirs(wf_dir)
        _write_yaml(os.path.join(wf_dir, "_config.yaml"), {"name": "wf", "enabled": True})
        _write_yaml(os.path.join(wf_dir, "_internal.yaml"), _make_policy("internal"))
        _write_yaml(os.path.join(wf_dir, "real.yaml"), _make_policy("real"))

        policies = load_policies(policies_dir)
        assert len(policies) == 1
        assert policies[0].name == "real"

    def test_mixed_standalone_and_workflow(self, policies_dir):
        """Standalone and workflow policies coexist."""
        _write_yaml(os.path.join(policies_dir, "standalone.yaml"), _make_policy("standalone"))

        wf_dir = os.path.join(policies_dir, "wf")
        os.makedirs(wf_dir)
        _write_yaml(os.path.join(wf_dir, "_config.yaml"), {
            "name": "wf", "enabled": True, "config": {"x": 1},
        })
        _write_yaml(os.path.join(wf_dir, "grouped.yaml"), _make_policy("grouped"))

        policies = load_policies(policies_dir)
        assert len(policies) == 2
        standalone = [p for p in policies if p.workflow is None]
        grouped = [p for p in policies if p.workflow == "wf"]
        assert len(standalone) == 1
        assert len(grouped) == 1
        assert grouped[0].workflow_config == {"x": 1}


class TestWorkflowDisable:
    """Disabled workflows have all their policies skipped."""

    def test_disabled_via_config(self, policies_dir):
        wf_dir = os.path.join(policies_dir, "wf")
        os.makedirs(wf_dir)
        _write_yaml(os.path.join(wf_dir, "_config.yaml"), {
            "name": "wf", "enabled": False,
        })
        _write_yaml(os.path.join(wf_dir, "p1.yaml"), _make_policy("p1"))

        policies = load_policies(policies_dir)
        assert len(policies) == 0

    def test_disabled_via_file(self, policies_dir):
        wf_dir = os.path.join(policies_dir, "wf")
        os.makedirs(wf_dir)
        _write_yaml(os.path.join(wf_dir, "_config.yaml"), {
            "name": "wf", "enabled": True,
        })
        _write_yaml(os.path.join(wf_dir, "p1.yaml"), _make_policy("p1"))
        with open(os.path.join(wf_dir, ".disabled"), "w") as f:
            f.write("")

        policies = load_policies(policies_dir)
        assert len(policies) == 0

    def test_disabled_file_overrides_config_enabled(self, policies_dir):
        """.disabled file takes precedence over enabled: true in config."""
        wf_dir = os.path.join(policies_dir, "wf")
        os.makedirs(wf_dir)
        _write_yaml(os.path.join(wf_dir, "_config.yaml"), {
            "name": "wf", "enabled": True,
        })
        _write_yaml(os.path.join(wf_dir, "p1.yaml"), _make_policy("p1"))
        with open(os.path.join(wf_dir, ".disabled"), "w") as f:
            f.write("")

        policies = load_policies(policies_dir)
        assert len(policies) == 0

    def test_reenable_by_removing_disabled_file(self, policies_dir):
        wf_dir = os.path.join(policies_dir, "wf")
        os.makedirs(wf_dir)
        _write_yaml(os.path.join(wf_dir, "_config.yaml"), {
            "name": "wf", "enabled": True,
        })
        _write_yaml(os.path.join(wf_dir, "p1.yaml"), _make_policy("p1"))

        disabled_path = os.path.join(wf_dir, ".disabled")
        with open(disabled_path, "w") as f:
            f.write("")
        assert len(load_policies(policies_dir)) == 0

        os.remove(disabled_path)
        assert len(load_policies(policies_dir)) == 1


class TestLoadWorkflowConfig:
    """Unit tests for _load_workflow_config helper."""

    def test_missing_config(self, tmp_path):
        d = str(tmp_path / "wf")
        os.makedirs(d)
        assert _load_workflow_config(d) == {}

    def test_valid_config(self, tmp_path):
        d = str(tmp_path / "wf")
        os.makedirs(d)
        _write_yaml(os.path.join(d, "_config.yaml"), {
            "name": "test", "enabled": True, "config": {"a": 1},
        })
        cfg = _load_workflow_config(d)
        assert cfg["name"] == "test"
        assert cfg["config"] == {"a": 1}

    def test_invalid_yaml_returns_empty(self, tmp_path):
        d = str(tmp_path / "wf")
        os.makedirs(d)
        with open(os.path.join(d, "_config.yaml"), "w") as f:
            f.write(": invalid: yaml: [")
        assert _load_workflow_config(d) == {}


class TestIsWorkflowDisabled:
    """Unit tests for _is_workflow_disabled helper."""

    def test_enabled_by_default(self, tmp_path):
        d = str(tmp_path / "wf")
        os.makedirs(d)
        assert _is_workflow_disabled(d, {}) is False

    def test_disabled_by_config(self, tmp_path):
        d = str(tmp_path / "wf")
        os.makedirs(d)
        assert _is_workflow_disabled(d, {"enabled": False}) is True

    def test_disabled_by_file(self, tmp_path):
        d = str(tmp_path / "wf")
        os.makedirs(d)
        with open(os.path.join(d, ".disabled"), "w") as f:
            f.write("")
        assert _is_workflow_disabled(d, {"enabled": True}) is True


class TestBackwardCompatibility:
    """Existing tests depend on load_policies working with flat dirs."""

    def test_empty_dir(self, policies_dir):
        policies = load_policies(policies_dir)
        assert policies == []

    def test_non_yaml_files_ignored(self, policies_dir):
        with open(os.path.join(policies_dir, "README.md"), "w") as f:
            f.write("not a policy")
        with open(os.path.join(policies_dir, "notes.txt"), "w") as f:
            f.write("not a policy")
        policies = load_policies(policies_dir)
        assert policies == []

    def test_old_format_still_works(self, policies_dir):
        _write_yaml(os.path.join(policies_dir, "old.yaml"), {
            "name": "old-policy",
            "trigger": {"event": "file.changed"},
            "actions": [{"type": "emit", "event": "old.done"}],
        })
        policies = load_policies(policies_dir)
        assert len(policies) == 1
        assert policies[0].name == "old-policy"
        assert policies[0].rules[0].trigger_event == "file.changed"

    def test_new_format_still_works(self, policies_dir):
        _write_yaml(os.path.join(policies_dir, "new.yaml"), {
            "name": "new-policy",
            "rules": [{
                "name": "r1",
                "trigger": {"event": "file.changed"},
                "actions": [{"type": "emit", "event": "new.done"}],
            }],
        })
        policies = load_policies(policies_dir)
        assert len(policies) == 1
        assert policies[0].name == "new-policy"


class TestWorkflowConfigTemplateInjection:
    """Verify {{ workflow.config.X }} and {{ workflow.name }} resolve in action handlers."""

    def test_shell_renders_workflow_config_in_command(self):
        handler = ShellAction()
        wf_ctx = {"name": "boi-lifecycle", "config": {"scripts_dir": "/opt/scripts"}}
        result = handler.run(
            {"command": "echo {{ workflow.config.scripts_dir }}"},
            event_payload={"type": "test"},
            workflow_context=wf_ctx,
        )
        assert result["status"] == "success"
        assert result["output"] == "/opt/scripts"

    def test_shell_renders_workflow_name(self):
        handler = ShellAction()
        wf_ctx = {"name": "meeting-notes", "config": {}}
        result = handler.run(
            {"command": "echo {{ workflow.name }}"},
            event_payload={"type": "test"},
            workflow_context=wf_ctx,
        )
        assert result["status"] == "success"
        assert result["output"] == "meeting-notes"

    def test_shell_works_without_workflow_context(self):
        handler = ShellAction()
        result = handler.run(
            {"command": "echo {{ event.msg }}"},
            event_payload={"msg": "hello"},
        )
        assert result["status"] == "success"
        assert result["output"] == "hello"

    def test_emit_renders_workflow_config_in_payload(self):
        handler = EmitAction()
        wf_ctx = {"name": "ops", "config": {"threshold": 5}}
        result = handler.run(
            {"event": "test.out",
             "payload": '{"val": "{{ workflow.config.threshold }}"}'},
            event_payload={"type": "test"},
            workflow_context=wf_ctx,
        )
        assert result["status"] == "success"
        assert result["emitted"] == "test.out"

    def test_update_file_renders_workflow_config(self, tmp_path):
        target = tmp_path / "test.txt"
        target.write_text("old content")
        handler = UpdateFileAction()
        wf_ctx = {"name": "wf", "config": {"marker": "REPLACED"}}
        result = handler.run(
            {"target": str(target),
             "pattern": "old",
             "replace": "{{ workflow.config.marker }}"},
            event_payload={},
            workflow_context=wf_ctx,
        )
        assert result["status"] == "success"
        assert result["changed"] is True
        assert target.read_text() == "REPLACED content"

    def test_shell_sub_actions_get_workflow_context(self):
        handler = ShellAction()
        wf_ctx = {"name": "test-wf", "config": {"out_dir": "/tmp/test"}}
        result = handler.run(
            {"command": "echo ok",
             "on_success": [
                 {"type": "shell",
                  "command": "echo {{ workflow.config.out_dir }}"}
             ]},
            event_payload={"type": "test"},
            workflow_context=wf_ctx,
        )
        assert result["status"] == "success"
