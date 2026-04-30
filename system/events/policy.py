# policy.py
"""Policy loader for hex-events v2.

Evolves recipe.py to support multi-rule policies with metadata, rate limiting,
and static provides/requires declarations. Backwards-compatible with old recipe
YAML format (auto-wrapped as a single-rule policy).
"""
import fnmatch
import logging
import os
import time
from dataclasses import dataclass, field
from typing import Optional

import yaml
from db import parse_duration

log = logging.getLogger("hex-events")


# ---------------------------------------------------------------------------
# Canonical dataclasses (Condition and Action live here; recipe.py re-exports)
# ---------------------------------------------------------------------------

@dataclass
class Condition:
    # For shell conditions (type: shell)
    type: Optional[str] = None
    command: Optional[str] = None
    # For field-based conditions
    field: Optional[str] = None
    op: Optional[str] = None
    value: Optional[str | int | float | bool] = None


@dataclass
class Action:
    type: str
    params: dict  # all other fields from the action dict


@dataclass
class Rule:
    """A single event-condition-action rule inside a policy."""
    name: str
    trigger_event: str  # supports glob patterns
    conditions: list[Condition] = field(default_factory=list)
    actions: list[Action] = field(default_factory=list)
    ttl: Optional[str] = None  # e.g. "7d", "24h" — auto-skip rule after this age

    def matches_event_type(self, event_type: str) -> bool:
        return fnmatch.fnmatch(event_type, self.trigger_event)


@dataclass
class Policy:
    """A named group of rules with metadata and rate limiting."""
    name: str
    rules: list[Rule]
    description: str = ""
    standing_orders: list[str] = field(default_factory=list)
    reflection_ids: list[str] = field(default_factory=list)
    provides: dict = field(default_factory=dict)   # {"events": [...]}
    requires: dict = field(default_factory=dict)   # {"events": [...]}
    rate_limit: Optional[dict] = None              # {"max_fires": N, "window": "1h"}
    source_file: Optional[str] = None
    max_fires: Optional[int] = None               # fire at most N times, then auto-disable
    after_limit: str = "disable"                 # delete | disable — action when ttl or max_fires limit is hit
    workflow: Optional[str] = None                 # workflow directory name
    workflow_config: dict = field(default_factory=dict)  # from _config.yaml
    # Runtime state — not from YAML
    last_fires: list[float] = field(default_factory=list)


# ---------------------------------------------------------------------------
# Rate limiting helpers
# ---------------------------------------------------------------------------

def check_rate_limit(policy: Policy) -> bool:
    """Return True if the policy is allowed to fire now."""
    if not policy.rate_limit:
        return True
    max_fires = policy.rate_limit.get("max_fires", 0)
    window_str = policy.rate_limit.get("window", "1h")
    window_secs = parse_duration(str(window_str))
    cutoff = time.time() - window_secs
    recent = [t for t in policy.last_fires if t >= cutoff]
    return len(recent) < max_fires


def record_fire(policy: Policy) -> None:
    """Record a policy fire timestamp for rate-limit accounting."""
    policy.last_fires.append(time.time())


# ---------------------------------------------------------------------------
# Internal parsers
# ---------------------------------------------------------------------------

def _parse_conditions(raw: list) -> list[Condition]:
    conditions = []
    for c in (raw or []):
        if c.get("type") == "shell":
            conditions.append(Condition(type="shell", command=c["command"]))
        else:
            conditions.append(Condition(field=c["field"], op=c["op"], value=c["value"]))
    return conditions


def _parse_actions(raw: list) -> list[Action]:
    actions = []
    for a in (raw or []):
        atype = a["type"]
        params = {k: v for k, v in a.items() if k != "type"}
        actions.append(Action(type=atype, params=params))
    return actions


def _parse_rule(data: dict, policy_name: str, idx: int) -> Rule:
    name = data.get("name") or f"{policy_name}.rule-{idx}"
    trigger = data["trigger"]
    trigger_event = trigger["event"]
    # Support conditions at rule level OR nested under trigger:
    raw_conditions = data.get("conditions") or trigger.get("conditions") or []
    if not raw_conditions and "condition" in data:
        raw_conditions = [data["condition"]]
    conditions = _parse_conditions(raw_conditions)
    actions = _parse_actions(data.get("actions", []))
    return Rule(name=name, trigger_event=trigger_event,
                conditions=conditions, actions=actions,
                ttl=data.get("ttl"))


def _infer_provides_requires(trigger_event: str, actions: list[Action]) -> tuple[dict, dict]:
    """Infer provides/requires from old-format recipe fields."""
    emitted = [a.params["event"] for a in actions
               if a.type == "emit" and "event" in a.params]
    provides = {"events": emitted} if emitted else {}
    requires = {"events": [trigger_event]} if trigger_event else {}
    return provides, requires


# ---------------------------------------------------------------------------
# Policy loading
# ---------------------------------------------------------------------------

def _is_new_format(data: dict) -> bool:
    """Return True if the YAML uses the new policy format (has 'rules' list)."""
    return "rules" in data and isinstance(data["rules"], list)


def _is_old_format(data: dict) -> bool:
    """Return True if the YAML uses the old recipe format (has 'trigger' + 'actions')."""
    return "trigger" in data and "actions" in data and "name" in data


def _policy_from_new(data: dict, source_file: str) -> Policy:
    rules = [_parse_rule(r, data["name"], i) for i, r in enumerate(data["rules"])]
    return Policy(
        name=data["name"],
        description=data.get("description", ""),
        standing_orders=list(data.get("standing_orders", []) or []),
        reflection_ids=list(data.get("reflection_ids", []) or []),
        provides=dict(data.get("provides") or {}),
        requires=dict(data.get("requires") or {}),
        rate_limit=data.get("rate_limit"),
        max_fires=data.get("max_fires"),
        after_limit=data.get("after_limit", "disable"),
        rules=rules,
        source_file=source_file,
    )


def _policy_from_old(data: dict, source_file: str) -> Policy:
    """Auto-wrap an old recipe YAML into a single-rule Policy."""
    trigger_event = data["trigger"]["event"]
    conditions = _parse_conditions(data.get("conditions", []))
    actions = _parse_actions(data["actions"])

    rule = Rule(
        name=data["name"],
        trigger_event=trigger_event,
        conditions=conditions,
        actions=actions,
    )

    provides, requires = _infer_provides_requires(trigger_event, actions)

    return Policy(
        name=data["name"],
        description=data.get("description", ""),
        standing_orders=[],
        reflection_ids=[],
        provides=provides,
        requires=requires,
        rate_limit=None,
        after_limit=data.get("after_limit", "disable"),
        rules=[rule],
        source_file=source_file,
    )


def _load_single_policy(fpath: str, workflow_name: str = None,
                        workflow_config: dict = None,
                        on_invalid=None) -> Optional[Policy]:
    """Load a single policy YAML file. Returns None on error."""
    try:
        from policy_validator import validate_policy
        with open(fpath) as f:
            data = yaml.safe_load(f)
        if not isinstance(data, dict):
            import sys
            msg = f"[POLICY LOAD ERROR] Skipping non-dict YAML: {fpath}"
            log.error(msg)
            print(msg, file=sys.stderr)
            return None
        if data.get("enabled") is False:
            log.debug("Skipping disabled policy: %s", fpath)
            return None
        if _is_new_format(data):
            errors = validate_policy(data, fpath)
            if errors:
                if on_invalid:
                    on_invalid(fpath, errors)
                else:
                    import sys
                    for err in errors:
                        msg = f"[POLICY VALIDATION ERROR] {err}"
                        log.error(msg)
                        print(msg, file=sys.stderr)
                return None
            if "name" not in data:
                import sys
                msg = f"[POLICY LOAD ERROR] Skipping policy without name: {fpath}"
                log.error(msg)
                print(msg, file=sys.stderr)
                return None
            policy = _policy_from_new(data, source_file=fpath)
        elif _is_old_format(data):
            policy = _policy_from_old(data, source_file=fpath)
        else:
            import sys
            msg = f"[POLICY LOAD ERROR] Skipping unrecognized YAML format: {fpath}"
            log.error(msg)
            print(msg, file=sys.stderr)
            return None
        if workflow_name:
            policy.workflow = workflow_name
            policy.workflow_config = workflow_config or {}
        return policy
    except Exception as e:
        import sys
        msg = f"[POLICY LOAD ERROR] Failed to load {fpath}: {e}"
        log.error(msg)
        print(msg, file=sys.stderr)
        return None


def _load_workflow_config(workflow_dir: str) -> dict:
    """Load _config.yaml from a workflow directory. Returns {} if missing."""
    config_path = os.path.join(workflow_dir, "_config.yaml")
    if not os.path.isfile(config_path):
        return {}
    try:
        with open(config_path) as f:
            data = yaml.safe_load(f)
        return data if isinstance(data, dict) else {}
    except Exception as e:
        import sys
        msg = f"[POLICY LOAD ERROR] Failed to load workflow config {config_path}: {e}"
        log.error(msg)
        print(msg, file=sys.stderr)
        return {}


def _is_workflow_disabled(workflow_dir: str, workflow_cfg: dict) -> bool:
    """Check if a workflow is disabled via .disabled file or config."""
    if os.path.exists(os.path.join(workflow_dir, ".disabled")):
        return True
    return not workflow_cfg.get("enabled", True)


def load_policies(policies_dir: str, on_invalid=None) -> list[Policy]:
    """Load policies from a directory, supporting workflow subdirectories.

    Top-level .yaml/.yml files are standalone policies (backward compatible).
    Subdirectories are workflows: policies inside them inherit the workflow
    name and shared config from _config.yaml.

    Supports both new policy format (has 'rules' array) and old recipe format
    (has 'trigger' + 'actions'). Old format is auto-wrapped as a single-rule
    policy with inferred provides/requires.

    on_invalid: optional callback(fpath: str, errors: list[str]) called when a
        policy fails schema validation. If None, validation errors are logged as
        warnings but the policy is still skipped.
    """
    from policy_validator import validate_policy
    policies = []
    for entry in sorted(os.listdir(policies_dir)):
        entry_path = os.path.join(policies_dir, entry)

        if os.path.isfile(entry_path) and entry.endswith((".yaml", ".yml")):
            # Standalone policy in root (backward compatible)
            policy = _load_single_policy(entry_path)
            if policy:
                policies.append(policy)

        elif os.path.isdir(entry_path):
            # Workflow directory
            workflow_cfg = _load_workflow_config(entry_path)
            workflow_name = workflow_cfg.get("name", entry)
            if _is_workflow_disabled(entry_path, workflow_cfg):
                log.info("Skipping disabled workflow: %s", workflow_name)
                continue
            shared_config = workflow_cfg.get("config", {})
            for fname in sorted(os.listdir(entry_path)):
                if fname.startswith("_") or fname == ".disabled":
                    continue
                if not fname.endswith((".yaml", ".yml")):
                    continue
                fpath = os.path.join(entry_path, fname)
                policy = _load_single_policy(
                    fpath, workflow_name=workflow_name,
                    workflow_config=shared_config,
                )
                if policy:
                    policies.append(policy)

    return policies
