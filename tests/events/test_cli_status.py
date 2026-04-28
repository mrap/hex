"""Tests for hex_events_cli.py — cmd_status policy count display."""
import os
import sys
import tempfile
import textwrap

import pytest
import yaml

# conftest.py adds parent dir to sys.path
import hex_events_cli
from policy import load_policies


def _write_policy(directory, filename, name):
    """Write a minimal valid policy YAML file."""
    data = {
        "name": name,
        "description": "Test policy",
        "standing_orders": [],
        "reflection_ids": [],
        "provides": {"events": []},
        "requires": {"events": ["test.event"]},
        "rules": [
            {
                "name": "rule-1",
                "trigger": {"event": "test.event"},
                "conditions": [],
                "actions": [{"type": "shell", "command": "echo hello"}],
            }
        ],
    }
    path = os.path.join(directory, filename)
    with open(path, "w") as f:
        yaml.dump(data, f)
    return path


class TestLoadAllPolicies:
    def test_count_matches_yaml_files(self, tmp_path):
        """_load_all_policies returns one policy per valid YAML file."""
        _write_policy(str(tmp_path), "p1.yaml", "policy-one")
        _write_policy(str(tmp_path), "p2.yaml", "policy-two")
        _write_policy(str(tmp_path), "p3.yaml", "policy-three")

        orig_policies = hex_events_cli.POLICIES_DIR
        orig_recipes = hex_events_cli.RECIPES_DIR
        try:
            hex_events_cli.POLICIES_DIR = str(tmp_path)
            hex_events_cli.RECIPES_DIR = str(tmp_path)
            policies, d = hex_events_cli._load_all_policies()
        finally:
            hex_events_cli.POLICIES_DIR = orig_policies
            hex_events_cli.RECIPES_DIR = orig_recipes

        assert len(policies) == 3
        assert d == str(tmp_path)

    def test_empty_dir_returns_zero(self, tmp_path):
        """Empty policies dir returns empty list without error."""
        orig_policies = hex_events_cli.POLICIES_DIR
        orig_recipes = hex_events_cli.RECIPES_DIR
        try:
            hex_events_cli.POLICIES_DIR = str(tmp_path)
            hex_events_cli.RECIPES_DIR = str(tmp_path)
            policies, _ = hex_events_cli._load_all_policies()
        finally:
            hex_events_cli.POLICIES_DIR = orig_policies
            hex_events_cli.RECIPES_DIR = orig_recipes

        assert policies == []

    def test_nonexistent_dir_returns_zero(self):
        """Non-existent policies dir returns empty list without error."""
        orig_policies = hex_events_cli.POLICIES_DIR
        orig_recipes = hex_events_cli.RECIPES_DIR
        try:
            hex_events_cli.POLICIES_DIR = "/nonexistent_dir_test_cli_status"
            hex_events_cli.RECIPES_DIR = "/nonexistent_dir_test_cli_status"
            policies, _ = hex_events_cli._load_all_policies()
        finally:
            hex_events_cli.POLICIES_DIR = orig_policies
            hex_events_cli.RECIPES_DIR = orig_recipes

        assert policies == []


class TestCmdStatusOutput:
    def test_output_uses_policies_label(self, tmp_path, capsys):
        """cmd_status output contains 'Policies:' not 'Recipes:'."""
        _write_policy(str(tmp_path), "p1.yaml", "policy-one")

        orig_policies = hex_events_cli.POLICIES_DIR
        orig_recipes = hex_events_cli.RECIPES_DIR
        orig_db = hex_events_cli.DB_PATH
        try:
            hex_events_cli.POLICIES_DIR = str(tmp_path)
            hex_events_cli.RECIPES_DIR = str(tmp_path)
            # Point DB at a temp path so EventsDB doesn't fail
            hex_events_cli.DB_PATH = str(tmp_path / "test_events.db")

            import argparse
            args = argparse.Namespace()
            hex_events_cli.cmd_status(args)
        finally:
            hex_events_cli.POLICIES_DIR = orig_policies
            hex_events_cli.RECIPES_DIR = orig_recipes
            hex_events_cli.DB_PATH = orig_db

        captured = capsys.readouterr()
        assert "Policies:" in captured.out
        assert "Recipes:" not in captured.out

    def test_policy_count_is_nonzero(self, tmp_path, capsys):
        """cmd_status reports correct non-zero policy count."""
        _write_policy(str(tmp_path), "p1.yaml", "policy-one")
        _write_policy(str(tmp_path), "p2.yaml", "policy-two")

        orig_policies = hex_events_cli.POLICIES_DIR
        orig_recipes = hex_events_cli.RECIPES_DIR
        orig_db = hex_events_cli.DB_PATH
        try:
            hex_events_cli.POLICIES_DIR = str(tmp_path)
            hex_events_cli.RECIPES_DIR = str(tmp_path)
            hex_events_cli.DB_PATH = str(tmp_path / "test_events.db")

            import argparse
            args = argparse.Namespace()
            hex_events_cli.cmd_status(args)
        finally:
            hex_events_cli.POLICIES_DIR = orig_policies
            hex_events_cli.RECIPES_DIR = orig_recipes
            hex_events_cli.DB_PATH = orig_db

        captured = capsys.readouterr()
        assert "2 loaded" in captured.out
