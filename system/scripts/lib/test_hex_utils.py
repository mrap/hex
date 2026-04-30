"""Tests for hex_utils shared utility library."""

from __future__ import annotations

import logging
import os
import re
import sys
from pathlib import Path

import pytest

# Ensure the lib package is importable when pytest is run from any cwd.
sys.path.insert(0, str(Path(__file__).parent.parent))

from lib.hex_utils import (
    emit_event,
    get_hex_root,
    get_today,
    load_yaml,
    setup_logging,
)


# ---------------------------------------------------------------------------
# get_hex_root
# ---------------------------------------------------------------------------


def test_get_hex_root_from_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """HEX_DIR env var is used when set."""
    monkeypatch.setenv("HEX_DIR", "/tmp/test-hex-root")
    result = get_hex_root()
    assert result == Path("/tmp/test-hex-root")


def test_get_hex_root_default(monkeypatch: pytest.MonkeyPatch) -> None:
    """Falls back to ~/hex when HEX_DIR is absent."""
    monkeypatch.delenv("HEX_DIR", raising=False)
    result = get_hex_root()
    assert result == Path.home() / "hex"


def test_get_hex_root_empty_env_uses_default(monkeypatch: pytest.MonkeyPatch) -> None:
    """Empty HEX_DIR string is treated the same as unset."""
    monkeypatch.setenv("HEX_DIR", "")
    result = get_hex_root()
    assert result == Path.home() / "hex"


# ---------------------------------------------------------------------------
# load_yaml
# ---------------------------------------------------------------------------


def test_load_yaml_valid(tmp_path: Path) -> None:
    """Valid YAML file is parsed into a dict."""
    yaml_file = tmp_path / "config.yaml"
    yaml_file.write_text("key: value\nnumber: 42\n")
    result = load_yaml(yaml_file)
    assert result == {"key": "value", "number": 42}


def test_load_yaml_missing(tmp_path: Path) -> None:
    """Missing file returns an empty dict instead of raising."""
    result = load_yaml(tmp_path / "nonexistent.yaml")
    assert result == {}


def test_load_yaml_invalid(tmp_path: Path) -> None:
    """Invalid YAML returns an empty dict instead of raising."""
    bad_file = tmp_path / "bad.yaml"
    bad_file.write_text("key: [unclosed bracket\n")
    result = load_yaml(bad_file)
    assert result == {}


def test_load_yaml_non_dict_returns_empty(tmp_path: Path) -> None:
    """YAML that is not a mapping (e.g. bare list) returns empty dict."""
    list_file = tmp_path / "list.yaml"
    list_file.write_text("- item1\n- item2\n")
    result = load_yaml(list_file)
    assert result == {}


# ---------------------------------------------------------------------------
# get_today
# ---------------------------------------------------------------------------


def test_get_today_format() -> None:
    """Return value matches YYYY-MM-DD."""
    today = get_today()
    assert re.fullmatch(r"\d{4}-\d{2}-\d{2}", today), f"Unexpected format: {today!r}"


# ---------------------------------------------------------------------------
# setup_logging
# ---------------------------------------------------------------------------


def test_setup_logging_returns_logger() -> None:
    """setup_logging returns a Logger with the given name."""
    logger = setup_logging("test.hex_utils")
    assert isinstance(logger, logging.Logger)
    assert logger.name == "test.hex_utils"


def test_setup_logging_has_handler() -> None:
    """Logger produced by setup_logging has at least one handler."""
    logger = setup_logging("test.hex_utils.handler")
    assert len(logger.handlers) >= 1


def test_setup_logging_idempotent() -> None:
    """Calling setup_logging twice does not add duplicate handlers."""
    name = "test.hex_utils.idempotent"
    setup_logging(name)
    setup_logging(name)
    logger = logging.getLogger(name)
    assert len(logger.handlers) == 1
