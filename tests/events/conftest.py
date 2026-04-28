"""Configure sys.path so tests can import hex-events modules from system/events/."""
import sys
import os
import pytest

# system/events/ = three levels up from this file (tests/events/ → tests/ → repo root → system/events/)
REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
EVENTS_SRC = os.path.join(REPO_ROOT, "system", "events")

sys.path.insert(0, EVENTS_SRC)


@pytest.fixture
def repo_root():
    """Return the absolute path to the hex-events source in system/events/."""
    return EVENTS_SRC
