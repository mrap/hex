"""hex-integration rotate <name> — run the bundle's rotation script."""
import argparse
import json
import os
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(prog="hex-integration rotate")
    parser.add_argument("name", help="Bundle name")
    parser.add_argument("--hex-root", required=True)
    parser.add_argument("--json", dest="json_out", default="false")
    parser.add_argument("--no-telemetry", dest="no_telemetry", default="false")
    parser.add_argument("--quiet", default="false")
    args = parser.parse_args()

    json_out = args.json_out == "true"
    no_telemetry = args.no_telemetry == "true"
    quiet = args.quiet == "true"
    hex_root = args.hex_root
    name = args.name

    lib_dir = os.path.join(hex_root, ".hex", "lib")
    if lib_dir not in sys.path:
        sys.path.insert(0, lib_dir)
    from integration import state as state_mod
    from integration import telemetry as telemetry_mod

    state_dir = os.path.join(hex_root, "projects", "integrations", "_state")
    bundles_dir = os.path.join(hex_root, "integrations")

    def log(msg):
        if not quiet:
            print(f"[rotate] {msg}", file=sys.stderr)

    def emit(event, payload=None):
        if not no_telemetry:
            telemetry_mod.emit(event, payload)

    if not state_mod.is_installed(name, state_dir):
        print(f"[rotate] ERROR: '{name}' is not installed", file=sys.stderr)
        return 1

    rotate_script = os.path.join(bundles_dir, name, "maintenance", "rotate.sh")
    if not os.path.isfile(rotate_script):
        if json_out:
            print(json.dumps({"name": name, "ok": False, "reason": "no_rotation_defined"}))
        else:
            print(f"[rotate] no rotation defined for '{name}'", file=sys.stderr)
        return 5

    log(f"Running rotate for {name}: {rotate_script}")
    try:
        result = subprocess.run(
            ["bash", rotate_script],
            capture_output=True,
            text=True,
            timeout=120,
            env={**os.environ, "HEX_ROOT": hex_root},
        )
        rc = result.returncode
        stdout = result.stdout.strip()
        stderr = result.stderr.strip()
    except subprocess.TimeoutExpired:
        emit("hex.integration.rotated.fail", {"name": name, "reason": "timeout"})
        print(f"[rotate] FAIL: rotation timed out for {name}", file=sys.stderr)
        return 1
    except Exception as e:
        emit("hex.integration.rotated.fail", {"name": name, "reason": str(e)})
        print(f"[rotate] FAIL: {e}", file=sys.stderr)
        return 1

    ok = rc == 0
    event = "hex.integration.rotated.ok" if ok else "hex.integration.rotated.fail"
    emit(event, {"name": name, "rc": rc})

    if json_out:
        print(json.dumps({"name": name, "rc": rc, "ok": ok, "stdout": stdout, "stderr": stderr}))
    else:
        if ok:
            print(f"[rotate] {name}: rotation complete")
        else:
            print(f"[rotate] {name}: rotation failed (exit {rc})", file=sys.stderr)
        if stdout:
            print(stdout)
        if stderr and not quiet:
            print(stderr, file=sys.stderr)

    return rc


if __name__ == "__main__":
    sys.exit(main())
