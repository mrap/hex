"""hex-integration uninstall <name> — uninstall a bundle."""
import argparse
import os
import sys


def main():
    parser = argparse.ArgumentParser(prog="hex-integration uninstall")
    parser.add_argument("name", help="Bundle name")
    parser.add_argument("--hex-root", required=True)
    parser.add_argument("--json", dest="json_out", default="false")
    parser.add_argument("--dry-run", dest="dry_run", default="false")
    parser.add_argument("--no-telemetry", dest="no_telemetry", default="false")
    parser.add_argument("--quiet", default="false")
    parser.add_argument("--force", action="store_true", help="Remove even if other bundles depend on this one")
    parser.add_argument("--delete-secrets", dest="delete_secrets", action="store_true",
                        help="Delete .hex/secrets/<name>.env without prompting")
    args = parser.parse_args()

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
    from integration import state as state_mod
    from integration import telemetry as telemetry_mod

    state_dir = os.path.join(hex_root, "projects", "integrations", "_state")
    symlinks_dir = os.path.join(hex_root, ".hex", "scripts", "integrations")
    secrets_dir = os.path.join(hex_root, ".hex", "secrets")
    policies_dir = os.path.expanduser("~/.hex-events/policies")

    def log(msg):
        if not quiet:
            print(f"[uninstall] {msg}", file=sys.stderr)

    def emit(event, payload=None):
        if not no_telemetry:
            telemetry_mod.emit(event, payload)

    # 1. Read state — idempotent: warn if not installed, exit 0
    existing_state = state_mod.read_state(name, state_dir)
    if existing_state is None:
        log(f"WARNING: '{name}' is not installed — nothing to do")
        return 0

    # 2. Reverse dependency check — read each installed bundle's manifest for depends_on
    if not args.force:
        dependents = []
        for installed_name in state_mod.list_installed(state_dir):
            if installed_name == name:
                continue
            s = state_mod.read_state(installed_name, state_dir)
            if not s:
                continue
            bundle_path = s.get("bundle_path") or os.path.join(hex_root, "integrations", installed_name)
            try:
                manifest_i = bundle_mod.parse_manifest(bundle_path)
                if name in (manifest_i.get("depends_on") or []):
                    dependents.append(installed_name)
            except Exception:
                pass
        if dependents:
            print(
                f"[uninstall] ERROR: cannot uninstall '{name}' — the following installed bundle(s) depend on it: "
                f"{', '.join(dependents)}. Uninstall those first or use --force.",
                file=sys.stderr,
            )
            emit("hex.integration.uninstalled.fail", {"name": name, "reason": "has_dependents", "dependents": dependents})
            return 1

    if dry_run:
        log(f"[DRY RUN] Would uninstall {name}")
        return 0

    removed_policies = []
    removed_symlink = False
    try:
        # 3. Remove compiled policies
        removed_policies = compile_mod.remove_compiled_policies(name, policies_dir)
        for p in removed_policies:
            log(f"Removed policy: {p}")

        # 4. Remove probe symlink if it points into this bundle
        symlink_path = os.path.join(symlinks_dir, f"{name}.sh")
        if os.path.islink(symlink_path):
            link_target = os.readlink(symlink_path)
            # Accept both absolute and relative targets pointing into this bundle
            bundle_fragment = os.path.join("integrations", name, "probe.sh")
            if bundle_fragment in link_target or link_target.endswith(f"{name}/probe.sh"):
                os.unlink(symlink_path)
                removed_symlink = True
                log(f"Removed symlink: {symlink_path}")
            else:
                log(f"WARNING: symlink {symlink_path} points to unexpected target '{link_target}' — leaving intact")

        # 5. Delete state file
        state_mod.delete_state(name, state_dir)
        log(f"Removed state: {state_dir}/{name}.json")

        # 6. Handle secrets file
        secrets_path = os.path.join(secrets_dir, f"{name}.env")
        if os.path.isfile(secrets_path):
            if args.delete_secrets:
                os.unlink(secrets_path)
                log(f"Deleted secrets file: {secrets_path}")
            else:
                # Interactive prompt — only if stdin is a tty
                if sys.stdin.isatty():
                    answer = input(f"[uninstall] Delete secrets file {secrets_path}? [y/N] ").strip().lower()
                    if answer == "y":
                        os.unlink(secrets_path)
                        log(f"Deleted secrets file: {secrets_path}")
                    else:
                        log(f"Leaving secrets file intact: {secrets_path}")
                else:
                    log(f"Leaving secrets file intact (non-interactive): {secrets_path}")

        # 7. Emit telemetry
        emit("hex.integration.uninstalled.ok", {
            "name": name,
            "removed_policies": len(removed_policies),
            "removed_symlink": removed_symlink,
        })

        log(f"Uninstalled {name} successfully")
        return 0

    except Exception as e:
        print(f"[uninstall] ERROR: {e}", file=sys.stderr)
        emit("hex.integration.uninstalled.fail", {"name": name, "reason": str(e)})
        return 1


if __name__ == "__main__":
    sys.exit(main())
