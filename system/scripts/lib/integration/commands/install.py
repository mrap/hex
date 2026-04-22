"""hex-integration install <name> — install a bundle."""
import argparse
import os
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(prog="hex-integration install")
    parser.add_argument("name", help="Bundle name")
    parser.add_argument("--hex-root", required=True)
    parser.add_argument("--json", dest="json_out", default="false")
    parser.add_argument("--dry-run", dest="dry_run", default="false")
    parser.add_argument("--no-telemetry", dest="no_telemetry", default="false")
    parser.add_argument("--quiet", default="false")
    args = parser.parse_args()

    json_out = args.json_out == "true"
    dry_run = args.dry_run == "true"
    no_telemetry = args.no_telemetry == "true"
    quiet = args.quiet == "true"

    hex_root = args.hex_root
    name = args.name

    lib_dir = os.path.join(hex_root, ".hex", "lib")
    if lib_dir not in sys.path:
        sys.path.insert(0, lib_dir)
    from integration import bundle as bundle_mod
    from integration import compile as compile_mod
    from integration import secrets as secrets_mod
    from integration import state as state_mod
    from integration import telemetry as telemetry_mod

    bundles_dir = os.path.join(hex_root, "integrations")
    bundle_dir = os.path.join(bundles_dir, name)
    secrets_dir = os.path.join(hex_root, ".hex", "secrets")
    state_dir = os.path.join(hex_root, "projects", "integrations", "_state")
    symlinks_dir = os.path.join(hex_root, ".hex", "scripts", "integrations")
    policies_dir = os.path.expanduser("~/.hex-events/policies")

    def log(msg):
        if not quiet:
            print(f"[install] {msg}", file=sys.stderr)

    def emit(event, payload=None):
        if not no_telemetry:
            telemetry_mod.emit(event, payload)

    # 1. Resolve bundle dir
    if not os.path.isdir(bundle_dir):
        print(f"[install] ERROR: bundle '{name}' not found in {bundles_dir}", file=sys.stderr)
        emit("hex.integration.installed.fail", {"name": name, "reason": "bundle_not_found"})
        return 1

    # 2. Parse + validate manifest
    try:
        manifest = bundle_mod.parse_manifest(bundle_dir)
    except ValueError as e:
        print(f"[install] ERROR: {e}", file=sys.stderr)
        emit("hex.integration.installed.fail", {"name": name, "reason": "parse_error"})
        return 1

    ok, errors = bundle_mod.validate_schema(manifest, bundle_dir)
    if not ok:
        print("[install] ERROR: schema validation failed:", file=sys.stderr)
        for err in errors:
            print(f"  - {err}", file=sys.stderr)
        emit("hex.integration.installed.fail", {"name": name, "reason": "schema_invalid"})
        return 1

    # 3. Check depends_on
    depends_on = manifest.get("depends_on") or []
    for dep in depends_on:
        if not state_mod.is_installed(dep, state_dir):
            print(f"[install] ERROR: dependency '{dep}' is not installed. Run: hex-integration install {dep}", file=sys.stderr)
            emit("hex.integration.installed.fail", {"name": name, "reason": f"dep_not_installed:{dep}"})
            return 1

    # 4. Validate secrets
    secrets_schema = manifest.get("secrets", {})
    sec_ok, sec_errors, sec_missing = secrets_mod.validate_secrets(name, secrets_schema, secrets_dir)
    if not sec_ok:
        if sec_missing:
            print(f"[install] ERROR: missing required secret(s): {', '.join(sec_missing)}", file=sys.stderr)
            example = secrets_mod.example_env_path(bundle_dir)
            print(f"  Copy {example} to {secrets_dir}/{name}.env and fill in values.", file=sys.stderr)
        for err in sec_errors:
            print(f"[install] ERROR: secret validation: {err}", file=sys.stderr)
        emit("hex.integration.installed.fail", {"name": name, "reason": "secrets_invalid"})
        return 3

    # Idempotency: check if already installed with same hash
    current_hash = bundle_mod.compute_manifest_hash(bundle_dir)
    existing_state = state_mod.read_state(name, state_dir)
    if existing_state and existing_state.get("version") == current_hash:
        symlink_path = os.path.join(symlinks_dir, f"{name}.sh")
        expected_target = os.path.join("..", "..", "..", "integrations", name, "probe.sh")
        symlink_ok = (
            os.path.islink(symlink_path) and os.readlink(symlink_path) == expected_target
        )
        policies_ok = all(
            os.path.isfile(p) for p in existing_state.get("compiled_policies", [])
        )
        if symlink_ok and policies_ok:
            log(f"{name}: already installed (no changes)")
            return 0
    elif existing_state and existing_state.get("version") != current_hash:
        log(f"{name}: bundle changed (hash {current_hash[:8]}), updating")

    if dry_run:
        log(f"[DRY RUN] Would install {name} (hash {current_hash[:8]})")
        return 0

    # 5. Compile event policies
    compiled = compile_mod.compile_policies(bundle_dir, name, policies_dir)
    log(f"Compiled {len(compiled)} policy file(s)")

    # 6. Create symlink: .hex/scripts/integrations/<name>.sh -> ../../../integrations/<name>/probe.sh
    os.makedirs(symlinks_dir, exist_ok=True)
    symlink_path = os.path.join(symlinks_dir, f"{name}.sh")
    target = os.path.join("..", "..", "..", "integrations", name, "probe.sh")

    if os.path.islink(symlink_path):
        if os.readlink(symlink_path) != target:
            os.unlink(symlink_path)
            os.symlink(target, symlink_path)
            log(f"Updated symlink: {symlink_path} -> {target}")
        else:
            log(f"Symlink already correct: {symlink_path}")
    elif os.path.exists(symlink_path):
        os.unlink(symlink_path)
        os.symlink(target, symlink_path)
        log(f"Replaced file with symlink: {symlink_path} -> {target}")
    else:
        os.symlink(target, symlink_path)
        log(f"Created symlink: {symlink_path} -> {target}")

    # 7. Write state
    installed_at = state_mod.now_iso()
    state_data = {
        "name": name,
        "tier": manifest.get("tier", "standard"),
        "installed_at": installed_at,
        "bundle_path": bundle_dir,
        "compiled_policies": compiled,
        "version": current_hash,
    }
    state_mod.write_state(name, state_dir, state_data)
    log(f"State written to {state_dir}/{name}.json")

    # 8. Emit telemetry
    emit("hex.integration.installed.ok", {
        "name": name,
        "tier": manifest.get("tier"),
        "version": current_hash,
        "compiled_policies": len(compiled),
    })

    # 9. Smoke test probe (non-fatal)
    probe_script = manifest.get("probe", {}).get("script", "probe.sh")
    probe_path = os.path.join(bundle_dir, probe_script)
    if os.path.isfile(probe_path):
        log(f"Running probe smoke test...")
        try:
            result = subprocess.run(
                ["bash", probe_path],
                capture_output=True,
                text=True,
                timeout=30,
                env={**os.environ, "HEX_ROOT": hex_root},
            )
            if result.returncode == 0:
                log("Probe smoke test passed")
            else:
                log(f"Probe returned {result.returncode} (non-fatal — monitor will track)")
        except Exception as e:
            log(f"Probe error (non-fatal): {e}")

    log(f"Installed {name} successfully")
    return 0


if __name__ == "__main__":
    sys.exit(main())
