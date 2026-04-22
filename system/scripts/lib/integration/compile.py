"""Compile bundle event policy templates into ~/.hex-events/policies/."""
import os
import shutil
import tempfile
from datetime import datetime, timezone


GENERATED_HEADER = "# generated_from: {source}\n# installed_at: {ts}\n"
GENERATED_MARKER = "# generated_from:"


def _policy_stem(source_path: str) -> str:
    """Extract stem from events/<stem>.yaml path."""
    return os.path.splitext(os.path.basename(source_path))[0]


def _output_name(bundle_name: str, stem: str) -> str:
    return f"{bundle_name}-{stem}.yaml"


def compile_policies(
    bundle_dir: str,
    bundle_name: str,
    policies_dir: str,
    dry_run: bool = False,
) -> list[str]:
    """
    Compile all events/*.yaml from bundle_dir into policies_dir.
    Returns list of output policy file paths written (or would-be written on dry_run).
    Idempotent: skips unchanged files.
    """
    events_dir = os.path.join(bundle_dir, "events")
    if not os.path.isdir(events_dir):
        return []

    os.makedirs(policies_dir, exist_ok=True)
    written: list[str] = []
    ts = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    for fname in sorted(os.listdir(events_dir)):
        if not fname.endswith(".yaml"):
            continue
        stem = _policy_stem(fname)
        source_rel = f"integrations/{bundle_name}/events/{fname}"
        out_name = _output_name(bundle_name, stem)
        out_path = os.path.join(policies_dir, out_name)
        src_path = os.path.join(events_dir, fname)

        with open(src_path) as f:
            body = f.read()

        header = GENERATED_HEADER.format(source=source_rel, ts=ts)
        # Strip any existing generated header from body (idempotency)
        lines = body.splitlines(keepends=True)
        body_lines = []
        for line in lines:
            if line.startswith("# generated_from:") or line.startswith("# installed_at:"):
                continue
            body_lines.append(line)
        new_content = header + "".join(body_lines)

        # Check if existing file already has same body (ignoring header ts)
        if os.path.isfile(out_path):
            with open(out_path) as f:
                existing = f.read()
            # Compare body content, ignoring installed_at timestamp differences
            existing_body = _strip_header(existing)
            new_body = _strip_header(new_content)
            if existing_body == new_body:
                continue  # No change

        if not dry_run:
            # Atomic write: write to .tmp then mv
            tmp_path = out_path + ".tmp"
            try:
                with open(tmp_path, "w") as f:
                    f.write(new_content)
                os.replace(tmp_path, out_path)
            finally:
                if os.path.exists(tmp_path):
                    os.unlink(tmp_path)

        written.append(out_path)

    return written


def _strip_header(content: str) -> str:
    """Remove generated header lines for comparison."""
    lines = content.splitlines(keepends=True)
    return "".join(
        line for line in lines
        if not line.startswith("# generated_from:") and not line.startswith("# installed_at:")
    )


def list_compiled_policies(bundle_name: str, policies_dir: str) -> list[str]:
    """List compiled policy files for a bundle (by generated_from marker)."""
    if not os.path.isdir(policies_dir):
        return []
    result = []
    prefix = f"{bundle_name}-"
    for fname in os.listdir(policies_dir):
        if not fname.startswith(prefix) or not fname.endswith(".yaml"):
            continue
        fpath = os.path.join(policies_dir, fname)
        try:
            with open(fpath) as f:
                first_line = f.readline()
            if GENERATED_MARKER in first_line:
                result.append(fpath)
        except OSError:
            pass
    return sorted(result)


def remove_compiled_policies(bundle_name: str, policies_dir: str) -> list[str]:
    """Remove all compiled policies for a bundle. Returns list of removed paths."""
    removed = []
    for fpath in list_compiled_policies(bundle_name, policies_dir):
        os.unlink(fpath)
        removed.append(fpath)
    return removed
