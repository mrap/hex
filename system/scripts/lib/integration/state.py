"""Read/write projects/integrations/_state/<name>.json."""
import json
import os
import tempfile
from datetime import datetime, timezone
from typing import Any


def read_state(name: str, state_dir: str) -> dict[str, Any] | None:
    """Return state dict or None if not installed."""
    path = os.path.join(state_dir, f"{name}.json")
    if not os.path.isfile(path):
        return None
    try:
        with open(path) as f:
            return json.load(f)
    except (json.JSONDecodeError, OSError):
        return None


def write_state(name: str, state_dir: str, data: dict[str, Any]) -> None:
    """Atomically write state file (tmp+mv)."""
    os.makedirs(state_dir, exist_ok=True)
    path = os.path.join(state_dir, f"{name}.json")
    tmp_path = path + ".tmp"
    try:
        with open(tmp_path, "w") as f:
            json.dump(data, f, indent=2)
            f.write("\n")
        os.replace(tmp_path, path)
    finally:
        if os.path.exists(tmp_path):
            os.unlink(tmp_path)


def delete_state(name: str, state_dir: str) -> bool:
    """Delete state file. Returns True if deleted, False if not found."""
    path = os.path.join(state_dir, f"{name}.json")
    if os.path.isfile(path):
        os.unlink(path)
        return True
    return False


def is_installed(name: str, state_dir: str) -> bool:
    return os.path.isfile(os.path.join(state_dir, f"{name}.json"))


def list_installed(state_dir: str) -> list[str]:
    """Return list of installed integration names."""
    if not os.path.isdir(state_dir):
        return []
    names = []
    for fname in sorted(os.listdir(state_dir)):
        if fname.endswith(".json") and not fname.startswith(".") and not fname.startswith("_"):
            names.append(fname[:-5])
    return names


def now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
