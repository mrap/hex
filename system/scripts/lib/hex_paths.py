"""Shared path resolution for hex Python scripts."""
from pathlib import Path

def find_agent_root() -> Path:
    """Walk up from this file to find the directory containing CLAUDE.md."""
    d = Path(__file__).resolve().parent
    while d != d.parent:
        if (d / "CLAUDE.md").exists():
            return d
        d = d.parent
    raise FileNotFoundError("Could not find CLAUDE.md")

AGENT_ROOT = find_agent_root()
HEX_DIR = AGENT_ROOT / ".hex"
MEMORY_DB = HEX_DIR / "memory.db"
CONTEXT_DB = HEX_DIR / "context_store.db"
