# recipe.py
"""YAML recipe loader for hex-events."""
import fnmatch
import logging
import os
from dataclasses import dataclass, field
from pathlib import Path

import yaml

log = logging.getLogger("hex-events")

# Condition and Action are now canonical in policy.py; imported here for
# backwards compatibility so existing code can still use `from recipe import Condition`.
from policy import Condition, Action  # noqa: E402 — canonical definitions in policy.py

@dataclass
class Recipe:
    name: str
    trigger_event: str  # supports glob patterns
    conditions: list[Condition] = field(default_factory=list)
    actions: list[Action] = field(default_factory=list)
    source_file: str | None = None

    @classmethod
    def from_dict(cls, data: dict, source_file: str | None = None) -> "Recipe":
        name = data["name"]
        trigger_event = data["trigger"]["event"]
        conditions = [
            Condition(field=c["field"], op=c["op"], value=c["value"])
            for c in data.get("conditions", [])
        ]
        actions = []
        for a in data["actions"]:
            atype = a["type"]
            params = {k: v for k, v in a.items() if k != "type"}
            actions.append(Action(type=atype, params=params))
        return cls(
            name=name,
            trigger_event=trigger_event,
            conditions=conditions,
            actions=actions,
            source_file=source_file,
        )

    def matches_event_type(self, event_type: str) -> bool:
        return fnmatch.fnmatch(event_type, self.trigger_event)


def load_recipes(recipes_dir: str) -> list[Recipe]:
    """Load all .yaml files from a directory into Recipe objects."""
    recipes = []
    for fname in sorted(os.listdir(recipes_dir)):
        if not fname.endswith((".yaml", ".yml")):
            continue
        fpath = os.path.join(recipes_dir, fname)
        try:
            with open(fpath) as f:
                data = yaml.safe_load(f)
            if not isinstance(data, dict) or "name" not in data or "trigger" not in data or "actions" not in data:
                log.warning("Skipping invalid recipe: %s", fpath)
                continue
            recipes.append(Recipe.from_dict(data, source_file=fpath))
        except Exception as e:
            log.warning("Failed to load recipe %s: %s", fpath, e)
    return recipes
