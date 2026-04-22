"""hex-integration status [<name>] — show state for one or all installed bundles."""
import argparse
import json
import os
import sys


def main():
    parser = argparse.ArgumentParser(prog="hex-integration status")
    parser.add_argument("name", nargs="?", help="Bundle name (omit for all)")
    parser.add_argument("--hex-root", required=True)
    parser.add_argument("--json", dest="json_out", default="false")
    parser.add_argument("--quiet", default="false")
    args = parser.parse_args()

    json_out = args.json_out == "true"
    hex_root = args.hex_root
    name = args.name

    lib_dir = os.path.join(hex_root, ".hex", "lib")
    if lib_dir not in sys.path:
        sys.path.insert(0, lib_dir)
    from integration import state as state_mod

    state_dir = os.path.join(hex_root, "projects", "integrations", "_state")

    if name:
        # Single integration status
        st = state_mod.read_state(name, state_dir)
        if st is None:
            print(f"[status] '{name}' is not installed", file=sys.stderr)
            return 1
        if json_out:
            print(json.dumps(st, indent=2))
        else:
            print(f"Integration: {name}")
            print(f"  tier:       {st.get('tier', '?')}")
            print(f"  installed:  {st.get('installed_at', '?')}")
            print(f"  version:    {st.get('version', '?')}")
            print(f"  policies:   {len(st.get('compiled_policies', []))}")
            last = st.get("last_probed", "-")
            print(f"  last probe: {last}")
    else:
        # All installed
        installed = state_mod.list_installed(state_dir)
        if not installed:
            print("[status] No integrations installed")
            return 0

        rows = []
        for n in installed:
            st = state_mod.read_state(n, state_dir) or {}
            rows.append({
                "name": n,
                "tier": st.get("tier", "?"),
                "installed_at": st.get("installed_at", "?"),
                "version": st.get("version", "?"),
                "policies": len(st.get("compiled_policies", [])),
                "last_probed": st.get("last_probed", "-"),
            })

        if json_out:
            print(json.dumps(rows, indent=2))
        else:
            hdr = f"{'NAME':<30} {'TIER':<10} {'INSTALLED':<22} {'VERSION':<10} {'POLICIES'}"
            print(hdr)
            print("-" * len(hdr))
            for r in rows:
                print(f"{r['name']:<30} {r['tier']:<10} {r['installed_at']:<22} {r['version']:<10} {r['policies']}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
