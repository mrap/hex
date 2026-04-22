"""hex-integration validate <name> — dry-run schema and file check."""
import argparse
import json
import os
import sys


def main():
    parser = argparse.ArgumentParser(prog="hex-integration validate")
    parser.add_argument("name", help="Bundle name")
    parser.add_argument("--hex-root", required=True)
    parser.add_argument("--json", dest="json_out", default="false")
    parser.add_argument("--quiet", default="false")
    args = parser.parse_args()

    json_out = args.json_out == "true"
    quiet = args.quiet == "true"
    hex_root = args.hex_root
    name = args.name

    lib_dir = os.path.join(hex_root, ".hex", "lib")
    if lib_dir not in sys.path:
        sys.path.insert(0, lib_dir)
    from integration import bundle as bundle_mod
    from integration import secrets as secrets_mod

    bundles_dir = os.path.join(hex_root, "integrations")
    bundle_dir = os.path.join(bundles_dir, name)
    secrets_dir = os.path.join(hex_root, ".hex", "secrets")

    errors = []

    def log(msg):
        if not quiet:
            print(f"[validate] {msg}", file=sys.stderr)

    # 1. Bundle dir exists
    if not os.path.isdir(bundle_dir):
        msg = f"bundle '{name}' not found in {bundles_dir}"
        errors.append(msg)
        if json_out:
            print(json.dumps({"name": name, "ok": False, "errors": errors}))
        else:
            print(f"[validate] FAIL: {msg}", file=sys.stderr)
        return 1

    # 2. Parse manifest
    try:
        manifest = bundle_mod.parse_manifest(bundle_dir)
    except ValueError as e:
        errors.append(str(e))
        if json_out:
            print(json.dumps({"name": name, "ok": False, "errors": errors}))
        else:
            print(f"[validate] FAIL: {e}", file=sys.stderr)
        return 1

    # 3. Schema validation
    ok, schema_errors = bundle_mod.validate_schema(manifest, bundle_dir)
    errors.extend(schema_errors)

    # 4. Required files: events/ dir, probe.sh
    events_dir = os.path.join(bundle_dir, "events")
    if not os.path.isdir(events_dir):
        log("events/ directory missing (ok for template-style bundles)")

    probe_script = manifest.get("probe", {}).get("script", "probe.sh")
    probe_path = os.path.join(bundle_dir, probe_script)
    if not os.path.isfile(probe_path):
        errors.append(f"probe script '{probe_script}' not found")

    # 5. Secrets check (dry-run — just report, no fail on missing since it's validate)
    secrets_schema = manifest.get("secrets", {})
    if secrets_schema:
        sec_ok, sec_errors, sec_missing = secrets_mod.validate_secrets(name, secrets_schema, secrets_dir)
        if sec_missing:
            log(f"secrets: missing keys (non-fatal for validate): {', '.join(sec_missing)}")
        for err in sec_errors:
            errors.append(f"secret: {err}")

    # 6. Maintenance scripts exist if listed (script paths are relative to bundle_dir)
    maintenance = manifest.get("maintenance") or []
    for item in maintenance:
        if isinstance(item, dict):
            script = item.get("script", "")
            if script:
                script_path = os.path.join(bundle_dir, script)
                if not os.path.isfile(script_path):
                    errors.append(f"maintenance script '{script}' not found")

    result = {
        "name": name,
        "ok": len(errors) == 0,
        "errors": errors,
    }

    if json_out:
        print(json.dumps(result, indent=2))
    else:
        if result["ok"]:
            print(f"[validate] OK: {name}")
        else:
            print(f"[validate] FAIL: {name}", file=sys.stderr)
            for err in errors:
                print(f"  - {err}", file=sys.stderr)

    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
