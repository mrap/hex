#!/usr/bin/env python3
"""Read-only managed Cargo target boundary for Foundation callers.

Use the installed BOI checker whenever its canonical location exists.  The
bootstrap validator is deliberately available only when that location is
absent, so a broken or old installation cannot become a policy bypass.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import sys
import time
from typing import Any, Dict, Iterable, Optional, Tuple


SCHEMA_VERSION = "boi.managed-target-check.v1"
MAX_CHECKER_OUTPUT_BYTES = 64 * 1024
CHECKER_TIMEOUT_SECONDS = 3.0
_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
_POLICY_REVISION = re.compile(r".+:sha256:[0-9a-f]{64}\Z", re.DOTALL)
_SOURCES = {"ARGUMENT", "CARGO_TARGET_DIR", "BOI_CARGO_TARGET_DIR", "DAEMON_TOML"}
_DAEMON_CONFIG_KEYS = {
    "phase_wall_clock_budget_secs",
    "goose_attempt_timeout_secs",
    "cargo_target_dir",
    "worker_runtime_policy",
    "managed_target_policy",
}
_WORKER_RUNTIME_POLICY_KEYS = {"allowed_providers", "require_explicit_effort", "approved_models"}
_OBSERVED_TIMEOUT_MAX = (1 << 63) - 1


class ManagedTargetCheckError(RuntimeError):
    """A failure that prevents a caller from launching Cargo."""

    def __init__(self, code: str, detail: str) -> None:
        super().__init__(detail)
        self.code = code
        self.detail = detail


def _fail(code: str, detail: str) -> None:
    raise ManagedTargetCheckError(code, detail)


def _path_from_value(value: Any, label: str) -> Path:
    if not isinstance(value, str) or not value:
        _fail("EMPTY_TARGET", "%s must not be empty" % label)
    if not os.path.isabs(value):
        _fail("RELATIVE_TARGET", "%s must be an absolute path" % label)
    return Path(value)


def _resolve_with_nearest_existing_parent(path: Path) -> Path:
    """Match BOI's lexical traversal and existing-alias resolution behavior."""
    raw = os.fspath(path)
    if not raw:
        _fail("EMPTY_TARGET", "target must not be empty")
    if not os.path.isabs(raw):
        _fail("RELATIVE_TARGET", "target must be an absolute path")
    # Resolve each existing component before applying a later ``..``.  In
    # particular, ``allowed/link/..`` must mean the parent of link's target,
    # not ``allowed``.  Normalizing first would bypass a denied alias target.
    parts = raw.split(os.sep)
    resolved = Path(os.sep)
    for part in parts:
        if not part or part == ".":
            continue
        if part == "..":
            if resolved == Path(os.sep):
                _fail("TARGET_RESOLUTION", "parent traversal escapes the filesystem root")
            resolved = resolved.parent
            continue
        candidate = resolved / part
        try:
            os.lstat(candidate)
        except FileNotFoundError:
            resolved = candidate
        except (OSError, ValueError) as exc:
            _fail("TARGET_RESOLUTION", "could not resolve target: %s" % exc)
        else:
            try:
                resolved = candidate.resolve(strict=True)
            except (OSError, ValueError) as exc:
                _fail("TARGET_RESOLUTION", "could not resolve target: %s" % exc)
    return resolved


def _path_is_at_or_below(path: Path, root: Path) -> bool:
    return path == root or root in path.parents


def _policy_from_toml(config: Dict[str, Any]) -> Tuple[str, Tuple[Path, ...], Tuple[Path, ...]]:
    policy = config.get("managed_target_policy")
    if not isinstance(policy, dict):
        _fail("MISSING_POLICY", "managed_target_policy is required")
    if set(policy) != {"revision", "allowed_roots", "denied_roots"}:
        _fail("INVALID_POLICY", "managed_target_policy has unknown or missing fields")
    revision = policy.get("revision")
    if not isinstance(revision, str) or not revision.strip():
        _fail("INVALID_POLICY", "managed_target_policy.revision must not be empty")

    def roots(name: str) -> Tuple[Path, ...]:
        values = policy.get(name)
        if not isinstance(values, list) or not values:
            _fail("INVALID_POLICY", "managed_target_policy.%s must be a nonempty list" % name)
        return tuple(_path_from_value(value, "managed_target_policy.%s" % name) for value in values)

    return revision, roots("allowed_roots"), roots("denied_roots")


def _policy_revision(revision: str, allowed: Iterable[Path], denied: Iterable[Path]) -> str:
    """Use BOI's ordered-root bytes, including its NUL delimiters."""
    digest = hashlib.sha256()
    digest.update(b"boi-managed-target-policy-v1\0")
    digest.update(revision.encode("utf-8"))
    digest.update(b"\0allowed\0")
    for root in allowed:
        digest.update(os.fsencode(_resolve_with_nearest_existing_parent(root)))
        digest.update(b"\0")
    digest.update(b"denied\0")
    for root in denied:
        digest.update(os.fsencode(_resolve_with_nearest_existing_parent(root)))
        digest.update(b"\0")
    return "%s:sha256:%s" % (revision, digest.hexdigest())


def _select_target(explicit_target: Optional[str], config: Dict[str, Any]) -> Tuple[Path, str]:
    if explicit_target is not None:
        return _path_from_value(explicit_target, "--target"), "ARGUMENT"
    if "CARGO_TARGET_DIR" in os.environ:
        return _path_from_value(os.environ["CARGO_TARGET_DIR"], "CARGO_TARGET_DIR"), "CARGO_TARGET_DIR"
    if "BOI_CARGO_TARGET_DIR" in os.environ:
        return _path_from_value(os.environ["BOI_CARGO_TARGET_DIR"], "BOI_CARGO_TARGET_DIR"), "BOI_CARGO_TARGET_DIR"
    if "cargo_target_dir" not in config:
        _fail("MISSING_TARGET", "no managed Cargo target was selected")
    return _path_from_value(config["cargo_target_dir"], "cargo_target_dir"), "DAEMON_TOML"


def _installed_selection_source(explicit_target: Optional[str]) -> str:
    """Validate only process-owned sources before delegating config to BOI."""
    if explicit_target is not None:
        _path_from_value(explicit_target, "--target")
        return "ARGUMENT"
    if "CARGO_TARGET_DIR" in os.environ:
        _path_from_value(os.environ["CARGO_TARGET_DIR"], "CARGO_TARGET_DIR")
        return "CARGO_TARGET_DIR"
    if "BOI_CARGO_TARGET_DIR" in os.environ:
        _path_from_value(os.environ["BOI_CARGO_TARGET_DIR"], "BOI_CARGO_TARGET_DIR")
        return "BOI_CARGO_TARGET_DIR"
    # BOI owns daemon.toml parsing and missing-target rejection in this path.
    return "DAEMON_TOML"


def _installed_expected_path(explicit_value: Optional[str], environment_name: str) -> Optional[str]:
    """Resolve a value this process selected, without reading BOI configuration."""
    if explicit_value is not None:
        return str(_resolve_with_nearest_existing_parent(_path_from_value(explicit_value, "argument")))
    if environment_name in os.environ:
        return str(_resolve_with_nearest_existing_parent(_path_from_value(os.environ[environment_name], environment_name)))
    return None


def validate_same_root_build_dir(build_dir: str, resolved_target: str) -> None:
    """Reject a supported Cargo build-dir override unless it resolves to the accepted root.

    This is deliberately a pure helper, not a second receipt field or CLI flag.
    A caller must apply it before launching Cargo, then bind both Cargo output
    variables to the receipt's resolved target.
    """
    candidate = _resolve_with_nearest_existing_parent(_path_from_value(build_dir, "build.build-dir"))
    accepted = _resolve_with_nearest_existing_parent(_path_from_value(resolved_target, "resolved target"))
    if candidate != accepted:
        _fail("BUILD_DIR_OVERRIDE", "build.build-dir must resolve to the accepted target")


def _attest_executable(executable: str) -> Tuple[str, str]:
    if not os.path.isabs(executable):
        _fail("INVALID_EXECUTABLE", "executable must be an absolute path")
    try:
        canonical = Path(executable).resolve(strict=True)
    except (OSError, ValueError) as exc:
        _fail("INVALID_EXECUTABLE", "could not read executable: %s" % exc)
    try:
        fd = os.open(
            os.fspath(canonical),
            os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0),
        )
    except OSError as exc:
        _fail("INVALID_EXECUTABLE", "could not open executable: %s" % exc)
    try:
        try:
            opened = os.fstat(fd)
            current = os.lstat(canonical)
        except OSError as exc:
            _fail("INVALID_EXECUTABLE", "could not inspect executable: %s" % exc)
        if not stat.S_ISREG(opened.st_mode):
            _fail("INVALID_EXECUTABLE", "executable must resolve to a regular file")
        if opened.st_mode & 0o111 == 0:
            _fail("INVALID_EXECUTABLE", "executable must be executable")
        if stat.S_ISLNK(current.st_mode) or (current.st_dev, current.st_ino) != (opened.st_dev, opened.st_ino):
            _fail("INVALID_EXECUTABLE", "executable identity changed while it was opened")
        expected_size = opened.st_size
        remaining = expected_size + 1
        total = 0
        digest = hashlib.sha256()
        try:
            while remaining:
                block = os.read(fd, min(64 * 1024, remaining))
                if not block:
                    break
                total += len(block)
                remaining -= len(block)
                digest.update(block)
        except OSError as exc:
            _fail("INVALID_EXECUTABLE", "could not hash executable: %s" % exc)
        try:
            after = os.fstat(fd)
            current = os.lstat(canonical)
        except OSError as exc:
            _fail("INVALID_EXECUTABLE", "could not recheck executable: %s" % exc)
        if (
            total != expected_size
            or (after.st_size, after.st_dev, after.st_ino, after.st_mtime_ns)
            != (opened.st_size, opened.st_dev, opened.st_ino, opened.st_mtime_ns)
            or stat.S_ISLNK(current.st_mode)
            or (current.st_dev, current.st_ino) != (opened.st_dev, opened.st_ino)
        ):
            _fail("INVALID_EXECUTABLE", "executable identity or size changed while it was hashed")
        return str(canonical), digest.hexdigest()
    finally:
        os.close(fd)


def _load_toml(path: Path) -> Dict[str, Any]:
    try:
        import tomllib  # type: ignore[import-not-found]
    except ImportError:
        _fail("TOML_PARSER_UNAVAILABLE", "bootstrap requires Python 3.11+ tomllib")
    try:
        with path.open("rb") as handle:
            value = tomllib.load(handle)
    except FileNotFoundError:
        _fail("MISSING_CONFIG", "daemon.toml is required for bootstrap validation")
    except (OSError, ValueError) as exc:
        _fail("INVALID_CONFIG", "daemon.toml is invalid: %s" % exc)
    if not isinstance(value, dict):
        _fail("INVALID_CONFIG", "daemon.toml must contain a table")
    if set(value).difference(_DAEMON_CONFIG_KEYS):
        _fail("INVALID_CONFIG", "daemon.toml has unknown top-level fields")
    _validate_daemon_config(value)
    return value


def _validate_daemon_config(config: Dict[str, Any]) -> None:
    """Match DaemonConfig deserialization and validation before target selection."""
    for name in ("phase_wall_clock_budget_secs", "goose_attempt_timeout_secs"):
        if name in config:
            value = config[name]
            # The frozen producer decoder accepts nonnegative TOML integers
            # through signed-64 maximum. tomllib normalizes valid integer
            # spellings to int, so enforce the observed decoder range.
            if type(value) is not int or not 0 <= value <= _OBSERVED_TIMEOUT_MAX:
                _fail("INVALID_CONFIG", "%s must be a supported timeout integer" % name)
    if "cargo_target_dir" in config:
        try:
            _path_from_value(config["cargo_target_dir"], "cargo_target_dir")
        except ManagedTargetCheckError as exc:
            _fail("INVALID_CONFIG", "invalid cargo_target_dir: %s" % exc.detail)
    if "worker_runtime_policy" in config:
        _validate_worker_runtime_policy(config["worker_runtime_policy"])


def _validate_worker_runtime_policy(value: Any) -> None:
    if not isinstance(value, dict):
        _fail("INVALID_CONFIG", "worker_runtime_policy must be a table")
    keys = set(value)
    if keys.difference(_WORKER_RUNTIME_POLICY_KEYS) or not {"allowed_providers", "approved_models"}.issubset(keys):
        _fail("INVALID_CONFIG", "worker_runtime_policy has unknown or missing fields")
    providers = value["allowed_providers"]
    models = value["approved_models"]
    if not isinstance(providers, list) or not all(isinstance(provider, str) for provider in providers):
        _fail("INVALID_CONFIG", "worker_runtime_policy.allowed_providers must be a string list")
    if not isinstance(models, dict) or not all(isinstance(provider, str) for provider in models):
        _fail("INVALID_CONFIG", "worker_runtime_policy.approved_models must be a string-keyed table")
    if "require_explicit_effort" in value and type(value["require_explicit_effort"]) is not bool:
        _fail("INVALID_CONFIG", "worker_runtime_policy.require_explicit_effort must be boolean")
    seen = set()
    for provider in providers:
        if not provider.strip() or provider in seen:
            _fail("INVALID_CONFIG", "worker runtime allowed providers must be nonempty and unique")
        seen.add(provider)
        approved = models.get(provider)
        if not isinstance(approved, list) or not approved or not all(isinstance(model, str) for model in approved):
            _fail("INVALID_CONFIG", "worker runtime provider must have a nonempty model list")
        if any(not model.strip() for model in approved) or len(set(approved)) != len(approved):
            _fail("INVALID_CONFIG", "worker runtime approved models must be nonempty and unique")
    if any(provider not in seen for provider in models):
        _fail("INVALID_CONFIG", "worker runtime model list names a disallowed provider")


def _unique_json_pairs(pairs: Iterable[Tuple[str, Any]]) -> Dict[str, Any]:
    result: Dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _drain_checker_streams(streams: Dict[Any, bytearray], deadline: float) -> Optional[str]:
    """Drain checker pipes during owned-group cleanup without collecting output."""
    try:
        with selectors.DefaultSelector() as selector:
            for stream in streams:
                if not stream.closed:
                    os.set_blocking(stream.fileno(), False)
                    selector.register(stream, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return "checker pipes did not close before cleanup deadline"
                for event, _ in selector.select(remaining):
                    try:
                        block = os.read(event.fileobj.fileno(), 8192)
                    except BlockingIOError:
                        continue
                    except OSError as exc:
                        return "could not drain checker pipe: %s" % exc
                    if not block:
                        selector.unregister(event.fileobj)
    except OSError as exc:
        return "could not observe checker pipes: %s" % exc
    return None


def _abort_checker_group(
    process: subprocess.Popen,
    streams: Dict[Any, bytearray],
    failure_code: str,
    failure_detail: str,
) -> None:
    """Kill the owned process group before reaping its leader or reusing its PID."""
    deadline = time.monotonic() + 1.0
    cleanup_errors = []
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        # The process group is already absent. Reap the leader below.
        pass
    except OSError as exc:
        # EPERM is not success. Keep reaping bounded and report the exact
        # signal fault after cleanup attempts finish.
        cleanup_errors.append("could not signal checker group: %s" % exc)
    drain_error = _drain_checker_streams(streams, deadline)
    if drain_error is not None:
        cleanup_errors.append(drain_error)
    try:
        process.wait(timeout=max(0.001, deadline - time.monotonic()))
    except subprocess.TimeoutExpired:
        cleanup_errors.append("checker leader did not exit before cleanup deadline")
    if cleanup_errors:
        _fail(
            "CHECKER_CLEANUP_FAILED",
            "%s; cleanup: %s" % (failure_detail, "; ".join(cleanup_errors)),
        )
    _fail(failure_code, failure_detail)


def _read_checker(checker: Path, argv: list[str], timeout_seconds: float) -> bytes:
    """Run the checker in an owned group with bounded wall time and output."""
    try:
        process = subprocess.Popen(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            close_fds=True,
            start_new_session=True,
        )
    except OSError as exc:
        _fail("CHECKER_UNAVAILABLE", "installed BOI checker could not start: %s" % exc)
    assert process.stdout is not None and process.stderr is not None
    streams = {process.stdout: bytearray(), process.stderr: bytearray()}
    selector = selectors.DefaultSelector()
    for stream in streams:
        os.set_blocking(stream.fileno(), False)
        selector.register(stream, selectors.EVENT_READ)
    deadline = time.monotonic() + timeout_seconds
    overflow = False
    timed_out = False
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                timed_out = True
                break
            try:
                events = selector.select(remaining)
            except OSError as exc:
                _abort_checker_group(process, streams, "CHECKER_READ_FAILED", "could not read checker output: %s" % exc)
            for event, _ in events:
                stream = event.fileobj
                try:
                    block = os.read(stream.fileno(), 8192)
                except BlockingIOError:
                    continue
                except OSError as exc:
                    _abort_checker_group(process, streams, "CHECKER_READ_FAILED", "could not read checker output: %s" % exc)
                if not block:
                    selector.unregister(stream)
                    continue
                if len(streams[stream]) + len(block) > MAX_CHECKER_OUTPUT_BYTES:
                    overflow = True
                    break
                streams[stream].extend(block)
            if overflow:
                break
        if timed_out:
            _abort_checker_group(process, streams, "CHECKER_TIMEOUT", "installed BOI checker exceeded its time limit")
        if overflow:
            _abort_checker_group(process, streams, "CHECKER_OUTPUT_TOO_LARGE", "installed BOI checker exceeded its output limit")
        try:
            process.wait(timeout=max(0.001, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            _abort_checker_group(process, streams, "CHECKER_TIMEOUT", "installed BOI checker exceeded its time limit")
        if process.returncode != 0:
            _fail("CHECKER_REJECTED", "installed BOI checker rejected the check")
        return bytes(streams[process.stdout])
    finally:
        selector.close()
        for stream in streams:
            stream.close()


def _validate_installed_receipt(
    payload: bytes,
    caller: str,
    executable_identity: Tuple[str, str],
    source_revision: str,
    selection_source: str,
    expected_target: Optional[str],
) -> Dict[str, Any]:
    try:
        receipt = json.loads(payload.decode("utf-8"), object_pairs_hook=_unique_json_pairs)
    except (UnicodeDecodeError, ValueError, json.JSONDecodeError) as exc:
        _fail("CHECKER_MALFORMED_RECEIPT", "installed BOI checker emitted invalid JSON: %s" % exc)
    required = {
        "schema_version", "status", "caller_identity", "executable_identity",
        "resolved_target", "policy_revision", "source_revision", "selection_source",
    }
    if not isinstance(receipt, dict) or set(receipt) != required:
        _fail("CHECKER_MALFORMED_RECEIPT", "installed BOI checker receipt has an invalid schema")
    identity = receipt["executable_identity"]
    if not isinstance(identity, dict) or set(identity) != {"canonical_path", "sha256"}:
        _fail("CHECKER_MALFORMED_RECEIPT", "installed BOI checker receipt has an invalid executable identity")
    if (
        receipt["schema_version"] != SCHEMA_VERSION
        or receipt["status"] != "accepted"
        or receipt["caller_identity"] != caller
        or receipt["source_revision"] != source_revision
        or receipt["selection_source"] != selection_source
        or receipt["selection_source"] not in _SOURCES
        or (expected_target is not None and receipt["resolved_target"] != expected_target)
        or identity["canonical_path"] != executable_identity[0]
        or identity["sha256"] != executable_identity[1]
        or not isinstance(receipt["resolved_target"], str)
        or not os.path.isabs(receipt["resolved_target"])
        or not isinstance(receipt["policy_revision"], str)
        or not _POLICY_REVISION.fullmatch(receipt["policy_revision"])
        or not isinstance(identity["sha256"], str)
        or not _SHA256.fullmatch(identity["sha256"])
    ):
        _fail("CHECKER_MISMATCHED_RECEIPT", "installed BOI checker receipt does not match this request")
    return receipt


def check(
    caller: str,
    executable: str,
    source_revision: str,
    explicit_target: Optional[str] = None,
    timeout_seconds: float = CHECKER_TIMEOUT_SECONDS,
) -> Dict[str, Any]:
    """Return a validated receipt without creating targets or loading BOI state."""
    if not isinstance(caller, str) or not caller.strip():
        _fail("EMPTY_CALLER", "caller must not be empty")
    if not isinstance(source_revision, str) or not source_revision.strip():
        _fail("EMPTY_SOURCE_REVISION", "source revision must not be empty")
    if timeout_seconds <= 0:
        _fail("INVALID_TIMEOUT", "checker timeout must be positive")
    executable_identity = _attest_executable(executable)
    home = Path(os.environ.get("HOME", ""))
    if not home.is_absolute():
        _fail("INVALID_HOME", "HOME must be an absolute path")
    checker = home / ".boi" / "bin" / "boi"
    # lexists treats a dangling symlink as present.  It must fail loud instead
    # of selecting bootstrap validation.
    if os.path.lexists(checker):
        selection_source = _installed_selection_source(explicit_target)
        if explicit_target is not None:
            expected_target = _installed_expected_path(explicit_target, "")
        elif "CARGO_TARGET_DIR" in os.environ:
            expected_target = _installed_expected_path(None, "CARGO_TARGET_DIR")
        elif "BOI_CARGO_TARGET_DIR" in os.environ:
            expected_target = _installed_expected_path(None, "BOI_CARGO_TARGET_DIR")
        else:
            expected_target = None
        payload = _read_checker(
            checker,
            [str(checker), "target", "check", "--caller", caller, "--executable", executable,
             "--source-revision", source_revision]
            + (["--target", explicit_target] if explicit_target is not None else []),
            timeout_seconds,
        )
        return _validate_installed_receipt(
            payload, caller, executable_identity, source_revision, selection_source, expected_target,
        )

    config = _load_toml(home / ".boi" / "v2" / "daemon.toml")
    revision, allowed, denied = _policy_from_toml(config)
    selected, selection_source = _select_target(explicit_target, config)
    resolved_target = _resolve_with_nearest_existing_parent(selected)
    resolved_denied = tuple(_resolve_with_nearest_existing_parent(root) for root in denied)
    if any(_path_is_at_or_below(resolved_target, root) for root in resolved_denied):
        _fail("DENIED_TARGET", "selected target is denied by policy")
    resolved_allowed = tuple(_resolve_with_nearest_existing_parent(root) for root in allowed)
    if not any(_path_is_at_or_below(resolved_target, root) for root in resolved_allowed):
        _fail("OUTSIDE_ALLOWED_ROOT", "selected target is outside allowed policy roots")
    return {
        "schema_version": SCHEMA_VERSION,
        "status": "accepted",
        "caller_identity": caller,
        "executable_identity": {"canonical_path": executable_identity[0], "sha256": executable_identity[1]},
        "resolved_target": str(resolved_target),
        "policy_revision": _policy_revision(revision, allowed, denied),
        "source_revision": source_revision,
        "selection_source": selection_source,
    }


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--caller", required=True)
    parser.add_argument("--executable", required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--target")
    args = parser.parse_args(argv)
    try:
        print(json.dumps(check(args.caller, args.executable, args.source_revision, args.target), separators=(",", ":")))
    except ManagedTargetCheckError as exc:
        print("managed-target-check: %s: %s" % (exc.code, exc.detail), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
