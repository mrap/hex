"""hex-integration probe <name> — run the integration check harness."""
import argparse
import json
import os
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(prog="hex-integration probe")
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

    def log(msg):
        if not quiet:
            print(f"[probe] {msg}", file=sys.stderr)

    def emit(event, payload=None):
        if not no_telemetry:
            telemetry_mod.emit(event, payload)

    if not state_mod.is_installed(name, state_dir):
        print(f"[probe] ERROR: '{name}' is not installed", file=sys.stderr)
        return 1

    harness = os.path.join(hex_root, ".hex", "scripts", "hex-integration-check.sh")
    if not os.path.isfile(harness):
        print(f"[probe] ERROR: harness not found: {harness}", file=sys.stderr)
        emit("hex.integration.probed.fail", {"name": name, "reason": "harness_missing"})
        return 1

    log(f"Running probe for {name} via {harness}")
    try:
        result = subprocess.run(
            ["bash", harness, name],
            capture_output=True,
            text=True,
            timeout=60,
            env={**os.environ, "HEX_ROOT": hex_root},
        )
        rc = result.returncode
        stdout = result.stdout.strip()
        stderr = result.stderr.strip()
    except subprocess.TimeoutExpired:
        emit("hex.integration.probed.fail", {"name": name, "reason": "timeout"})
        print(f"[probe] FAIL: probe timed out for {name}", file=sys.stderr)
        return 1
    except Exception as e:
        emit("hex.integration.probed.fail", {"name": name, "reason": str(e)})
        print(f"[probe] FAIL: {e}", file=sys.stderr)
        return 1

    # Update last_probed in state
    st = state_mod.read_state(name, state_dir) or {}
    from integration.state import now_iso
    st["last_probed"] = now_iso()
    st["last_probe_rc"] = rc
    state_mod.write_state(name, state_dir, st)

    ok = rc in (0, 1)  # 0=pass, 1=degraded, 2=sub-check-not-found is a real error
    event = "hex.integration.probed.ok" if ok else "hex.integration.probed.fail"
    emit(event, {"name": name, "rc": rc})

    if json_out:
        print(json.dumps({"name": name, "rc": rc, "ok": ok, "stdout": stdout, "stderr": stderr}))
    else:
        status_str = "PASS" if rc == 0 else ("DEGRADED" if rc == 1 else "FAIL")
        print(f"[probe] {name}: {status_str} (exit {rc})")
        if stdout:
            print(stdout)
        if stderr and not quiet:
            print(stderr, file=sys.stderr)

    return 0 if ok else rc


if __name__ == "__main__":
    sys.exit(main())
