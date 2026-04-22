"""Parse integration.yaml and validate bundle schema."""
import json
import os
import subprocess
from typing import Any


REQUIRED_TOP_LEVEL = ["name", "description", "owner", "tier"]
REQUIRED_PROBE = ["script"]
VALID_TIERS = {"critical", "standard", "slow"}


def _yaml_to_dict(yaml_path: str) -> dict:
    """Parse a YAML file via PyYAML (homebrew-managed, not a new pip dep)."""
    try:
        import yaml  # noqa: PLC0415
        with open(yaml_path) as f:
            return yaml.safe_load(f) or {}
    except ImportError:
        # Fallback: ruby on macOS always has yaml+json
        result = subprocess.run(
            ["ruby", "-ryaml", "-rjson", "-e",
             "puts JSON.generate(YAML.safe_load(ARGF.read))"],
            stdin=open(yaml_path),
            capture_output=True,
            text=True,
            check=True,
        )
        return json.loads(result.stdout)


def parse_manifest(bundle_dir: str) -> dict[str, Any]:
    """Parse integration.yaml from bundle_dir. Raises ValueError on error."""
    yaml_path = os.path.join(bundle_dir, "integration.yaml")
    if not os.path.isfile(yaml_path):
        raise ValueError(f"integration.yaml not found in {bundle_dir}")
    try:
        data = _yaml_to_dict(yaml_path)
    except Exception as e:
        raise ValueError(f"Failed to parse integration.yaml: {e}") from e
    if not isinstance(data, dict):
        raise ValueError("integration.yaml must be a YAML mapping")
    return data


def validate_schema(manifest: dict, bundle_dir: str) -> tuple[bool, list[str]]:
    """Validate manifest against required schema. Returns (ok, errors)."""
    errors: list[str] = []

    for field in REQUIRED_TOP_LEVEL:
        if not manifest.get(field):
            errors.append(f"Missing required field: {field}")

    tier = manifest.get("tier", "")
    if tier and tier not in VALID_TIERS:
        errors.append(f"Invalid tier '{tier}'. Must be one of: {', '.join(sorted(VALID_TIERS))}")

    probe = manifest.get("probe", {})
    if not isinstance(probe, dict):
        errors.append("'probe' must be a mapping")
    else:
        for field in REQUIRED_PROBE:
            if not probe.get(field):
                errors.append(f"Missing required probe field: probe.{field}")

    # depends_on and provides must be lists if present
    for list_field in ("depends_on", "provides"):
        val = manifest.get(list_field)
        if val is not None and not isinstance(val, list):
            errors.append(f"'{list_field}' must be a list")

    # maintenance must be a list if present
    maintenance = manifest.get("maintenance")
    if maintenance is not None and not isinstance(maintenance, list):
        errors.append("'maintenance' must be a list")

    # events must be a list if present
    events = manifest.get("events")
    if events is not None and not isinstance(events, list):
        errors.append("'events' must be a list")

    # Validate file references exist
    if not errors:
        probe_script = probe.get("script", "")
        probe_path = os.path.join(bundle_dir, probe_script)
        if probe_script and not os.path.isfile(probe_path):
            errors.append(f"probe.script '{probe_script}' not found in bundle")

    return (len(errors) == 0, errors)


def compute_manifest_hash(bundle_dir: str) -> str:
    """Compute a stable hash of integration.yaml content."""
    import hashlib
    yaml_path = os.path.join(bundle_dir, "integration.yaml")
    with open(yaml_path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()[:16]
