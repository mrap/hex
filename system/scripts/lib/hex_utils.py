"""Shared utilities for hex Python scripts.

Provides canonical implementations of common patterns so individual scripts
do not duplicate path resolution, YAML loading, event emission, date helpers,
or logging setup.
"""

from __future__ import annotations

import logging
import os
import subprocess
from pathlib import Path
from typing import Union


def get_hex_root() -> Path:
    """Return the hex workspace root directory.

    Reads the HEX_DIR environment variable first; falls back to ~/hex
    when the variable is absent or empty.

    Returns:
        Absolute Path to the hex root directory.
    """
    agent_dir = os.environ.get("HEX_DIR", "").strip()
    if agent_dir:
        return Path(agent_dir)
    return Path.home() / "hex"


def load_yaml(path: Union[str, Path]) -> dict:
    """Load a YAML file and return its contents as a dictionary.

    Handles missing files and parse errors gracefully, logging a warning and
    returning an empty dict rather than raising.

    Args:
        path: Filesystem path to the YAML file.

    Returns:
        Parsed YAML content, or an empty dict on any failure.
    """
    logger = logging.getLogger(__name__)
    resolved = Path(path)
    try:
        import yaml  # imported lazily — not all scripts need PyYAML
    except ImportError:
        logger.warning("PyYAML not installed; cannot load %s", resolved)
        return {}

    try:
        with open(resolved, encoding="utf-8") as fh:
            data = yaml.safe_load(fh)
        return data if isinstance(data, dict) else {}
    except FileNotFoundError:
        logger.warning("YAML file not found: %s", resolved)
        return {}
    except yaml.YAMLError as exc:
        logger.warning("Invalid YAML in %s: %s", resolved, exc)
        return {}
    except OSError as exc:
        logger.warning("Cannot read %s: %s", resolved, exc)
        return {}


def emit_event(event_type: str, payload: dict) -> None:
    """Emit a hex event via the hex-emit.sh shell script.

    The script is resolved relative to HEX_DIR so it works in any
    deployment without hardcoded paths.

    Args:
        event_type: Dot-separated event name, e.g. ``"hex.session.reflected"``.
        payload: Arbitrary JSON-serialisable dict attached to the event.
    """
    import json as _json

    logger = logging.getLogger(__name__)
    emit_script = get_hex_root() / ".hex" / "bin" / "hex-emit.sh"
    if not emit_script.exists():
        logger.warning("hex-emit.sh not found at %s; event %s dropped", emit_script, event_type)
        return
    try:
        subprocess.run(
            ["bash", str(emit_script), event_type, _json.dumps(payload)],
            check=False,
            capture_output=True,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        logger.warning("emit_event failed for %s: %s", event_type, exc)


def get_today() -> str:
    """Return today's date as an ISO 8601 string (YYYY-MM-DD).

    Uses the system date; does not rely on context-window assumptions.

    Returns:
        Date string in ``YYYY-MM-DD`` format.
    """
    from datetime import date

    return date.today().isoformat()


def setup_logging(name: str) -> logging.Logger:
    """Configure and return a named logger with a consistent format.

    Safe to call multiple times; will not add duplicate handlers to an already-
    configured logger.

    Args:
        name: Logger name, typically ``__name__`` of the calling module.

    Returns:
        Configured :class:`logging.Logger` instance.
    """
    logger = logging.getLogger(name)
    if not logger.handlers:
        handler = logging.StreamHandler()
        handler.setFormatter(
            logging.Formatter("%(asctime)s %(name)s %(levelname)s %(message)s")
        )
        logger.addHandler(handler)
        logger.setLevel(logging.INFO)
    return logger
