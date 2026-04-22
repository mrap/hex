"""Validate .hex/secrets/<name>.env against bundle schema. Never writes secrets."""
import os
import stat


def load_env_file(env_path: str) -> dict[str, str]:
    """Parse a .env file, returning key->value pairs. Ignores comments."""
    result: dict[str, str] = {}
    if not os.path.isfile(env_path):
        return result
    with open(env_path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if "=" in line:
                key, _, val = line.partition("=")
                # Strip surrounding quotes if present
                val = val.strip()
                if len(val) >= 2 and val[0] == val[-1] and val[0] in ('"', "'"):
                    val = val[1:-1]
                result[key.strip()] = val
    return result


def validate_secrets(
    name: str,
    secrets_schema: dict,
    secrets_dir: str,
) -> tuple[bool, list[str], list[str]]:
    """
    Validate secrets against schema.
    Returns (ok, errors, missing_keys).
    errors: hard failures (exit 5)
    missing_keys: list of missing required keys
    """
    errors: list[str] = []
    missing: list[str] = []

    if not secrets_schema:
        return (True, [], [])

    required = secrets_schema.get("required", [])
    if not required:
        return (True, [], [])

    env_path = os.path.join(secrets_dir, f"{name}.env")
    if not os.path.isfile(env_path):
        for req in required:
            key = req.get("name", "") if isinstance(req, dict) else str(req)
            missing.append(key)
        return (False, [], missing)

    env_vars = load_env_file(env_path)

    for req in required:
        if isinstance(req, dict):
            key = req.get("name", "")
            key_type = req.get("type", "string")
        else:
            key = str(req)
            key_type = "string"

        val = env_vars.get(key, "").strip()
        if not val:
            missing.append(key)
            continue

        if key_type == "path":
            if not os.path.isfile(val):
                errors.append(f"{key}={val} — file not found")
            else:
                mode = oct(stat.S_IMODE(os.stat(val).st_mode))
                if stat.S_IMODE(os.stat(val).st_mode) & 0o177:
                    errors.append(f"{key}={val} — expected chmod 600, got {mode}")

    ok = len(errors) == 0 and len(missing) == 0
    return (ok, errors, missing)


def example_env_path(bundle_dir: str) -> str:
    return os.path.join(bundle_dir, "secret.env.example")


def create_env_stub(name: str, bundle_dir: str, secrets_dir: str) -> str:
    """
    Copy secret.env.example to secrets_dir/<name>.env as a stub.
    Returns the created path. Never overwrites an existing file.
    """
    example = example_env_path(bundle_dir)
    env_path = os.path.join(secrets_dir, f"{name}.env")
    if os.path.isfile(env_path):
        return env_path
    if os.path.isfile(example):
        import shutil
        shutil.copy2(example, env_path)
    else:
        with open(env_path, "w") as f:
            f.write(f"# Secrets for {name} integration\n")
    return env_path
