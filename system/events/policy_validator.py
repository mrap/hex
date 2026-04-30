"""Policy validation for hex-events."""
import re
import yaml

VALID_ACTION_TYPES = {"shell", "emit", "notify", "update-file", "noop"}
VALID_CONDITION_OPS = {"eq", "neq", "contains", "gt", "lt", "gte", "lte", "glob", "regex"}
_DURATION_RE = re.compile(r"^\d+[smhd]$")


def _validate_condition_dict(condition: dict, prefix: str) -> list[str]:
    """Validate a single condition dict. Returns list of error strings."""
    errors = []
    cond_type = condition.get("type")
    if cond_type == "shell":
        if not isinstance(condition.get("command"), str):
            errors.append(f"{prefix}: shell condition missing 'command' (must be a string)")
    else:
        # field-based condition
        if not isinstance(condition.get("field"), str):
            errors.append(f"{prefix}: condition missing 'field' (must be a string)")
        op = condition.get("op")
        if op not in VALID_CONDITION_OPS:
            errors.append(
                f"{prefix}: condition.op '{op}' is not valid "
                f"(expected: {', '.join(sorted(VALID_CONDITION_OPS))})"
            )
        if "value" not in condition:
            errors.append(f"{prefix}: condition missing 'value'")
    return errors


def validate_policy(policy: dict, filename: str = "<unknown>") -> list[str]:
    """Validate a policy dict against the hex-events schema.
    Returns list of error strings. Empty list = valid."""
    errors = []

    if not isinstance(policy.get("name"), str):
        errors.append(f"{filename}: missing or invalid 'name' (must be a string)")

    lifecycle = policy.get("lifecycle")
    if lifecycle is not None:
        errors.append(
            f"{filename}: 'lifecycle' is deprecated — use 'max_fires' + 'after_limit' instead"
        )

    max_fires = policy.get("max_fires")
    if max_fires is not None:
        if not isinstance(max_fires, int) or max_fires <= 0:
            errors.append(
                f"{filename}: 'max_fires' must be a positive integer, got {max_fires!r}"
            )

    after_limit = policy.get("after_limit")
    if after_limit is not None:
        if after_limit not in ("delete", "disable"):
            errors.append(
                f"{filename}: invalid 'after_limit' value {after_limit!r} — must be 'delete' or 'disable'"
            )

    rules = policy.get("rules")
    if not isinstance(rules, list) or len(rules) == 0:
        errors.append(f"{filename}: missing or empty 'rules' (must be a non-empty list)")
        return errors  # can't validate rules if they don't exist

    for rule in rules:
        rule_name = rule.get("name", "<unnamed>") if isinstance(rule, dict) else "<unnamed>"
        prefix = f"{filename} rule '{rule_name}'"

        if not isinstance(rule, dict):
            errors.append(f"{filename}: rule is not a dict")
            continue

        if not isinstance(rule.get("name"), str):
            errors.append(f"{prefix}: missing or invalid 'name' (must be a string)")

        ttl = rule.get("ttl")
        if ttl is not None:
            if not isinstance(ttl, str) or not _DURATION_RE.match(ttl):
                errors.append(
                    f"{prefix}: invalid 'ttl' value {ttl!r} "
                    f"(expected duration like '7d', '24h', '30m', '60s')"
                )

        trigger = rule.get("trigger")
        if not isinstance(trigger, dict):
            errors.append(f"{prefix}: missing or invalid 'trigger' (must be a dict)")
        else:
            if not isinstance(trigger.get("event"), str):
                errors.append(f"{prefix}: trigger missing 'event' (must be a string)")

        actions = rule.get("actions")
        if not isinstance(actions, list) or len(actions) == 0:
            errors.append(f"{prefix}: missing or empty 'actions' (must be a non-empty list)")
        else:
            for i, action in enumerate(actions):
                action_prefix = f"{prefix} action[{i}]"
                if not isinstance(action, dict):
                    errors.append(f"{action_prefix}: action is not a dict")
                    continue

                atype = action.get("type")
                if atype not in VALID_ACTION_TYPES:
                    errors.append(
                        f"{action_prefix}: invalid type '{atype}' "
                        f"(expected: {', '.join(sorted(VALID_ACTION_TYPES))})"
                    )
                elif atype == "shell" and not isinstance(action.get("command"), str):
                    errors.append(f"{action_prefix}: shell action missing 'command' (must be a string)")
                elif atype == "emit" and not isinstance(action.get("event"), str):
                    errors.append(f"{action_prefix}: emit action missing 'event' (must be a string)")

        condition = rule.get("condition")
        if condition is not None:
            if not isinstance(condition, dict):
                errors.append(f"{prefix}: 'condition' must be a dict")
            else:
                errors.extend(_validate_condition_dict(condition, f"{prefix} condition"))

        conditions = rule.get("conditions")
        if conditions is not None:
            if not isinstance(conditions, list):
                errors.append(f"{prefix}: 'conditions' must be a list")
            else:
                for i, cond in enumerate(conditions):
                    cond_prefix = f"{prefix} conditions[{i}]"
                    if not isinstance(cond, dict):
                        errors.append(f"{cond_prefix}: must be a dict")
                    else:
                        errors.extend(_validate_condition_dict(cond, cond_prefix))

        if isinstance(trigger, dict):
            trigger_conditions = trigger.get("conditions")
            if trigger_conditions is not None:
                if not isinstance(trigger_conditions, list):
                    errors.append(f"{prefix}: trigger 'conditions' must be a list")
                else:
                    for i, cond in enumerate(trigger_conditions):
                        cond_prefix = f"{prefix} trigger.conditions[{i}]"
                        if not isinstance(cond, dict):
                            errors.append(f"{cond_prefix}: must be a dict")
                        else:
                            errors.extend(_validate_condition_dict(cond, cond_prefix))

    return errors


def validate_policy_file(filepath: str) -> list[str]:
    """Read a YAML policy file and validate it. Returns list of error strings."""
    try:
        with open(filepath) as f:
            policy = yaml.safe_load(f)
    except Exception as e:
        return [f"{filepath}: failed to parse YAML: {e}"]

    if not isinstance(policy, dict):
        return [f"{filepath}: policy file must be a YAML dict, got {type(policy).__name__}"]

    return validate_policy(policy, filepath)
