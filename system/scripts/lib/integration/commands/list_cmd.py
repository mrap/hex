"""hex-integration list — show installed + available bundles."""
import argparse
import json
import os
import sys


def main():
    parser = argparse.ArgumentParser(prog="hex-integration list")
    parser.add_argument("--hex-root", required=True)
    parser.add_argument("--json", dest="json_out", default="false")
    parser.add_argument("--quiet", default="false")
    args = parser.parse_args()

    json_out = args.json_out == "true"
    hex_root = args.hex_root

    lib_dir = os.path.join(hex_root, ".hex", "lib")
    if lib_dir not in sys.path:
        sys.path.insert(0, lib_dir)
    from integration import state as state_mod

    bundles_dir = os.path.join(hex_root, "integrations")
    state_dir = os.path.join(hex_root, "projects", "integrations", "_state")

    # Discover available bundles (dirs with integration.yaml)
    available = []
    if os.path.isdir(bundles_dir):
        for entry in sorted(os.listdir(bundles_dir)):
            yaml_path = os.path.join(bundles_dir, entry, "integration.yaml")
            if os.path.isdir(os.path.join(bundles_dir, entry)) and os.path.isfile(yaml_path):
                available.append(entry)

    installed_names = set(state_mod.list_installed(state_dir))

    rows = []
    for name in available:
        status = "installed" if name in installed_names else "available"
        tier = "?"
        last_probed = "-"

        # Try to get tier from state or manifest
        if name in installed_names:
            st = state_mod.read_state(name, state_dir)
            if st:
                tier = st.get("tier", "?")
                last_probed = st.get("last_probed", "-")

        if tier == "?":
            # Try parsing manifest quickly
            try:
                lib_dir2 = os.path.join(hex_root, ".hex", "lib")
                if lib_dir2 not in sys.path:
                    sys.path.insert(0, lib_dir2)
                from integration import bundle as bundle_mod
                bundle_dir = os.path.join(bundles_dir, name)
                mf = bundle_mod.parse_manifest(bundle_dir)
                tier = mf.get("tier", "?")
            except Exception:
                pass

        rows.append({
            "name": name,
            "status": status,
            "tier": tier,
            "last_probed": last_probed,
        })

    if json_out:
        print(json.dumps(rows, indent=2))
    else:
        # Print table
        header = f"{'NAME':<30} {'STATUS':<12} {'TIER':<10} {'LAST_PROBED'}"
        print(header)
        print("-" * len(header))
        for r in rows:
            print(f"{r['name']:<30} {r['status']:<12} {r['tier']:<10} {r['last_probed']}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
