"""hex-integration update <name> — re-compile policies + refresh symlink."""
import argparse
import os
import sys


def main():
    parser = argparse.ArgumentParser(prog="hex-integration update")
    parser.add_argument("name", help="Bundle name")
    parser.add_argument("--hex-root", required=True)
    parser.add_argument("--json", dest="json_out", default="false")
    parser.add_argument("--dry-run", dest="dry_run", default="false")
    parser.add_argument("--no-telemetry", dest="no_telemetry", default="false")
    parser.add_argument("--quiet", default="false")
    parser.add_argument("--force", action="store_true", default=False)
    args = parser.parse_args()

    dry_run = args.dry_run == "true"
    no_telemetry = args.no_telemetry == "true"
    quiet = args.quiet == "true"
    force = args.force

    hex_root = args.hex_root
    name = args.name

    lib_dir = os.path.join(hex_root, ".hex", "lib")
    if lib_dir not in sys.path:
        sys.path.insert(0, lib_dir)
    from integration import bundle as bundle_mod
    from integration import compile as compile_mod
    from integration import state as state_mod
    from integration import telemetry as telemetry_mod

    state_dir = os.path.join(hex_root, "projects", "integrations", "_state")
    bundles_dir = os.path.join(hex_root, "integrations")
    bundle_dir = os.path.join(bundles_dir, name)
    symlinks_dir = os.path.join(hex_root, ".hex", "scripts", "integrations")
    policies_dir = os.path.expanduser("~/.hex-events/policies")

    def log(msg):
        if not quiet:
            print(f"[update] {msg}", file=sys.stderr)

    def emit(event, payload=None):
        if not no_telemetry:
            telemetry_mod.emit(event, payload)

    # 1. Must be installed
    existing_state = state_mod.read_state(name, state_dir)
    if not existing_state:
        print(f"[update] ERROR: '{name}' is not installed — run: hex-integration install {name}", file=sys.stderr)
        return 4

    # 2. Check if anything changed
    if not os.path.isdir(bundle_dir):
        print(f"[update] ERROR: bundle dir not found: {bundle_dir}", file=sys.stderr)
        emit("hex.integration.updated.fail", {"name": name, "reason": "bundle_not_found"})
        return 1

    current_hash = bundle_mod.compute_manifest_hash(bundle_dir)
    old_hash = existing_state.get("version", "")
    symlink_path = os.path.join(symlinks_dir, f"{name}.sh")
    expected_target = os.path.join("..", "..", "..", "integrations", name, "probe.sh")
    symlink_ok = os.path.islink(symlink_path) and os.readlink(symlink_path) == expected_target
    policies_ok = all(
        os.path.isfile(p) for p in existing_state.get("compiled_policies", [])
    )

    if not force and current_hash == old_hash and symlink_ok and policies_ok:
        log(f"{name}: no changes detected")
        return 0

    if dry_run:
        log(f"[DRY RUN] Would update {name} (hash {current_hash[:8]})")
        return 0

    # 3. Re-compile all policies (atomic)
    compiled = compile_mod.compile_policies(bundle_dir, name, policies_dir)
    log(f"Compiled {len(compiled)} policy file(s)")

    # Refresh probe symlink
    os.makedirs(symlinks_dir, exist_ok=True)
    if os.path.islink(symlink_path):
        if os.readlink(symlink_path) != expected_target:
            os.unlink(symlink_path)
            os.symlink(expected_target, symlink_path)
            log(f"Updated symlink: {symlink_path} -> {expected_target}")
        else:
            log(f"Symlink already correct: {symlink_path}")
    elif os.path.exists(symlink_path):
        os.unlink(symlink_path)
        os.symlink(expected_target, symlink_path)
        log(f"Replaced file with symlink: {symlink_path} -> {expected_target}")
    else:
        os.symlink(expected_target, symlink_path)
        log(f"Created symlink: {symlink_path} -> {expected_target}")

    # Rewrite state — preserve installed_at, bump version
    try:
        manifest = bundle_mod.parse_manifest(bundle_dir)
    except ValueError as e:
        print(f"[update] ERROR: {e}", file=sys.stderr)
        emit("hex.integration.updated.fail", {"name": name, "reason": "parse_error"})
        return 1

    state_data = {
        "name": name,
        "tier": manifest.get("tier", existing_state.get("tier", "standard")),
        "installed_at": existing_state.get("installed_at", state_mod.now_iso()),
        "updated_at": state_mod.now_iso(),
        "bundle_path": bundle_dir,
        "compiled_policies": compiled if compiled else existing_state.get("compiled_policies", []),
        "version": current_hash,
    }
    state_mod.write_state(name, state_dir, state_data)
    log(f"State updated")

    # 5. Emit telemetry
    emit("hex.integration.updated.ok", {
        "name": name,
        "version": current_hash,
        "compiled_policies": len(compiled),
        "forced": force,
    })

    log(f"Updated {name} successfully")
    return 0


if __name__ == "__main__":
    sys.exit(main())
