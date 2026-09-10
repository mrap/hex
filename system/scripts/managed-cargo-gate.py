#!/usr/bin/env python3
"""Run one supported local Cargo operation with a managed output target.

This is deliberately not a general command runner. It accepts a small Cargo
grammar, resolves the output target through the adjacent managed-target adapter,
and executes Cargo with an argument array.
"""

from __future__ import print_function

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import uuid


_BARE_FLAGS = {
    "--all-targets", "--locked", "--offline", "--frozen", "--release",
    "--workspace", "--all-features", "--no-default-features", "--lib",
    "--bins", "--examples", "--tests", "--benches",
}
_VALUE_FLAGS = {
    "--manifest-path", "--package", "-p", "--target", "--features",
    "--profile", "--jobs", "--message-format",
}
_OPERATIONS = {"build", "test", "clippy"}


class GateError(RuntimeError):
    def __init__(self, code, detail):
        RuntimeError.__init__(self, detail)
        self.code = code
        self.detail = detail


def _fail(code, detail):
    raise GateError(code, detail)


def _adapter_path():
    path = Path(__file__).resolve().with_name("managed-target-check.py")
    try:
        value = os.lstat(str(path))
    except OSError as exc:
        _fail("ADAPTER_UNAVAILABLE", "managed target adapter is unavailable: %s" % exc)
    if stat.S_ISLNK(value.st_mode) or not stat.S_ISREG(value.st_mode):
        _fail("ADAPTER_UNAVAILABLE", "managed target adapter must be a regular non-symlink file")
    return path


def _adapter_module(path):
    spec = importlib.util.spec_from_file_location("managed_target_check_for_gate", str(path))
    if spec is None or spec.loader is None:
        _fail("ADAPTER_UNAVAILABLE", "managed target adapter cannot be loaded")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _parse_value(tokens, index, flag):
    value = tokens[index]
    if value.startswith(flag + "="):
        selected = value[len(flag) + 1:]
        if not selected:
            _fail("UNSUPPORTED_CARGO_ARGUMENT", "%s requires a value" % flag)
        return selected, index + 1
    if index + 1 >= len(tokens) or tokens[index + 1] == "--":
        _fail("UNSUPPORTED_CARGO_ARGUMENT", "%s requires a value" % flag)
    return tokens[index + 1], index + 2


def _parse_cargo(operation, tokens):
    if operation not in _OPERATIONS:
        _fail("UNSUPPORTED_CARGO_OPERATION", "unsupported Cargo operation: %s" % operation)
    result = []
    target_dir = None
    build_dirs = []
    profile = {"all_targets": False, "locked": False, "offline": False, "strict_warnings": False}
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token == "--":
            _fail("UNSUPPORTED_CARGO_ARGUMENT", "raw Cargo pass-through is not supported")
        if token == "--deny-warnings":
            if operation != "clippy":
                _fail("UNSUPPORTED_CARGO_ARGUMENT", "--deny-warnings is only valid for clippy")
            profile["strict_warnings"] = True
            index += 1
            continue
        if token == "--target-dir" or token.startswith("--target-dir="):
            value, index = _parse_value(tokens, index, "--target-dir")
            if target_dir is not None:
                _fail("UNSUPPORTED_CARGO_ARGUMENT", "multiple --target-dir values are not supported")
            target_dir = value
            continue
        if token == "--config" or token.startswith("--config="):
            value, index = _parse_value(tokens, index, "--config")
            if not value.startswith("build.build-dir=") or not value[len("build.build-dir="):]:
                _fail("UNSUPPORTED_CARGO_ARGUMENT", "only --config build.build-dir=PATH is supported")
            if build_dirs:
                _fail("UNSUPPORTED_CARGO_ARGUMENT", "multiple build.build-dir values are not supported")
            build_dirs.append(value[len("build.build-dir="):])
            continue
        if token in _BARE_FLAGS:
            result.append(token)
            if token == "--all-targets":
                profile["all_targets"] = True
            elif token == "--locked":
                profile["locked"] = True
            elif token == "--offline":
                profile["offline"] = True
            index += 1
            continue
        matched = None
        for flag in _VALUE_FLAGS:
            if token == flag or (flag.startswith("--") and token.startswith(flag + "=")):
                matched = flag
                break
        if matched is not None:
            value, index = _parse_value(tokens, index, matched)
            result.extend([matched, value])
            continue
        _fail("UNSUPPORTED_CARGO_ARGUMENT", "unsupported Cargo argument: %s" % token)
    if profile["strict_warnings"]:
        result.extend(["--", "-D", "warnings"])
    return result, target_dir, build_dirs, profile


def _validate_receipt_dir(value):
    if not value or not os.path.isabs(value):
        _fail("RECEIPT_UNSAFE_DIRECTORY", "receipt directory must be absolute")
    try:
        details = os.lstat(value)
    except OSError as exc:
        _fail("RECEIPT_UNSAFE_DIRECTORY", "receipt directory is unavailable: %s" % exc)
    if stat.S_ISLNK(details.st_mode) or not stat.S_ISDIR(details.st_mode):
        _fail("RECEIPT_UNSAFE_DIRECTORY", "receipt directory must be a non-symlink directory")
    if details.st_uid != os.geteuid() or details.st_mode & 0o022:
        _fail("RECEIPT_UNSAFE_DIRECTORY", "receipt directory must be owned by this user and not group- or other-writable")
    return Path(value)


def _write_fd(fd, payload):
    data = (json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
    offset = 0
    while offset < len(data):
        written = os.write(fd, data[offset:])
        if written <= 0:
            _fail("RECEIPT_UNWRITABLE", "short receipt write")
        offset += written
    os.fsync(fd)


def _private_regular_identity(path, expected, code, label):
    try:
        details = os.lstat(str(path))
    except OSError as exc:
        _fail(code, "could not inspect %s: %s" % (label, exc))
    actual = (details.st_dev, details.st_ino)
    if (
        stat.S_ISLNK(details.st_mode)
        or not stat.S_ISREG(details.st_mode)
        or details.st_mode & 0o077
        or actual != expected
    ):
        _fail(code, "%s identity, type, or mode changed" % label)
    return actual


def _create_receipt(directory, payload):
    path = directory / ("managed-cargo-%s.json" % uuid.uuid4().hex)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(str(path), flags, 0o600)
    except OSError as exc:
        _fail("RECEIPT_UNWRITABLE", "could not create receipt: %s" % exc)
    try:
        _write_fd(fd, payload)
        details = os.fstat(fd)
    except (OSError, GateError) as exc:
        if isinstance(exc, GateError):
            raise
        _fail("RECEIPT_UNWRITABLE", "could not write receipt: %s" % exc)
    finally:
        os.close(fd)
    if not stat.S_ISREG(details.st_mode) or details.st_mode & 0o077:
        _fail("RECEIPT_UNWRITABLE", "receipt must be a private regular file")
    identity = (details.st_dev, details.st_ino)
    _private_regular_identity(path, identity, "RECEIPT_UNWRITABLE", "new receipt")
    return path, identity


def _update_receipt(path, identity, payload):
    _private_regular_identity(path, identity, "RECEIPT_UPDATE_FAILED", "prepared receipt")
    temporary = path.with_name(".%s.%s.tmp" % (path.name, uuid.uuid4().hex))
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(str(temporary), flags, 0o600)
        try:
            _write_fd(fd, payload)
            temporary_details = os.fstat(fd)
        finally:
            os.close(fd)
        if not stat.S_ISREG(temporary_details.st_mode) or temporary_details.st_mode & 0o077:
            _fail("RECEIPT_UPDATE_FAILED", "temporary receipt must be a private regular file")
        temporary_identity = (temporary_details.st_dev, temporary_details.st_ino)
        _private_regular_identity(temporary, temporary_identity, "RECEIPT_UPDATE_FAILED", "temporary receipt")
        os.replace(str(temporary), str(path))
        _private_regular_identity(path, temporary_identity, "RECEIPT_UPDATE_FAILED", "final receipt")
    except (OSError, GateError) as exc:
        if isinstance(exc, GateError):
            raise
        _fail("RECEIPT_UPDATE_FAILED", "could not update receipt: %s" % exc)


def _run_adapter(adapter, caller, cargo, source_revision, target):
    command = [sys.executable, str(adapter), "--caller", caller, "--executable", cargo, "--source-revision", source_revision]
    if target is not None:
        command.extend(["--target", target])
    try:
        completed = subprocess.run(
            command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            universal_newlines=True, check=False,
        )
    except OSError as exc:
        _fail("TARGET_CHECK_FAILED", "managed target adapter could not start: %s" % exc)
    if completed.returncode != 0:
        _fail("TARGET_CHECK_FAILED", completed.stderr.strip() or "managed target adapter rejected the request")
    try:
        receipt = json.loads(completed.stdout)
    except ValueError as exc:
        _fail("TARGET_CHECK_FAILED", "managed target adapter emitted invalid JSON: %s" % exc)
    if (
        not isinstance(receipt, dict)
        or receipt.get("status") != "accepted"
        or not isinstance(receipt.get("resolved_target"), str)
        or not os.path.isabs(receipt["resolved_target"])
        or not isinstance(receipt.get("policy_revision"), str)
        or not receipt["policy_revision"]
    ):
        _fail("TARGET_CHECK_FAILED", "managed target adapter emitted an invalid receipt")
    return receipt


def _create_target_if_missing(path):
    if os.path.lexists(str(path)):
        if path.is_dir():
            return False
        _fail("TARGET_CREATE_FAILED", "resolved target is not a directory")
    try:
        parent = os.lstat(str(path.parent))
    except OSError as exc:
        _fail("TARGET_CREATE_FAILED", "target parent is unavailable: %s" % exc)
    if stat.S_ISLNK(parent.st_mode) or not stat.S_ISDIR(parent.st_mode):
        _fail("TARGET_CREATE_FAILED", "target parent must be an existing non-symlink directory")
    try:
        os.mkdir(str(path), 0o700)
    except OSError as exc:
        _fail("TARGET_CREATE_FAILED", "could not create accepted target leaf: %s" % exc)
    return True


def _cargo_path():
    selected = shutil.which("cargo")
    if not selected:
        _fail("CARGO_UNAVAILABLE", "cargo is not available on PATH")
    invocation = os.path.abspath(selected)
    path = Path(invocation).resolve()
    try:
        details = os.lstat(str(path))
    except OSError as exc:
        _fail("CARGO_UNAVAILABLE", "cargo is unavailable: %s" % exc)
    if stat.S_ISLNK(details.st_mode) or not stat.S_ISREG(details.st_mode) or not details.st_mode & 0o111:
        _fail("CARGO_UNAVAILABLE", "cargo must resolve to an executable regular file")
    return invocation


def run(argv):
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--caller", required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--source-state", required=True, choices=("clean", "dirty", "unavailable"))
    parser.add_argument("--receipt-dir", required=True)
    try:
        parsed, remaining = parser.parse_known_args(argv)
    except SystemExit:
        _fail("INVALID_REQUEST", "invalid managed Cargo request")
    if not remaining:
        _fail("UNSUPPORTED_CARGO_OPERATION", "a Cargo operation is required")
    operation = remaining[0]
    cargo_args, requested_target, build_dirs, profile = _parse_cargo(operation, remaining[1:])
    receipt_dir = _validate_receipt_dir(parsed.receipt_dir)
    adapter = _adapter_path()
    adapter_module = _adapter_module(adapter)
    cargo = _cargo_path()
    first = _run_adapter(adapter, parsed.caller, cargo, parsed.source_revision, requested_target)
    for value in ([os.environ["CARGO_BUILD_BUILD_DIR"]] if "CARGO_BUILD_BUILD_DIR" in os.environ else []) + build_dirs:
        try:
            adapter_module.validate_same_root_build_dir(value, first["resolved_target"])
        except Exception as exc:
            _fail("BUILD_DIR_OVERRIDE", str(exc))
    target = Path(first["resolved_target"])
    created = _create_target_if_missing(target)
    retained = "; retained created target: %s" % target if created else ""
    try:
        second = _run_adapter(adapter, parsed.caller, cargo, parsed.source_revision, str(target))
    except GateError as exc:
        _fail(
            "TARGET_RECHECK_FAILED",
            "managed target recheck refused%s: %s" % (retained, exc.detail),
        )
    if second["resolved_target"] != first["resolved_target"] or second["policy_revision"] != first["policy_revision"]:
        _fail("TARGET_RECHECK_FAILED", "managed target or policy changed after target creation%s" % retained)
    prepared = {
        "schema_version": "foundation.managed-cargo-gate.v1",
        "managed_target": second,
        "source_revision": parsed.source_revision,
        "source_state": parsed.source_state,
        "operation": operation,
        "profile": profile,
        "target_created": created,
        "outcome": {"state": "started"},
    }
    receipt_path, identity = _create_receipt(receipt_dir, prepared)
    child_env = dict(os.environ)
    for key in ("CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR", "BOI_CARGO_TARGET_DIR"):
        child_env.pop(key, None)
    child_env["CARGO_TARGET_DIR"] = second["resolved_target"]
    child_env["CARGO_BUILD_BUILD_DIR"] = second["resolved_target"]
    try:
        completed = subprocess.run([cargo, operation] + cargo_args, env=child_env, check=False)
    except OSError:
        finished = dict(prepared)
        finished["outcome"] = {"state": "launch_failed", "cargo_error": "spawn_failed"}
        try:
            _update_receipt(receipt_path, identity, finished)
        except GateError as exc:
            print("managed-cargo-gate: receipt=%s operation=%s exit_status=launch_failed receipt_update=failed" % (receipt_path, operation))
            _fail("CARGO_LAUNCH_AND_RECEIPT_FAILURE", "Cargo launch and receipt update both failed: %s" % exc.detail)
        print("managed-cargo-gate: receipt=%s operation=%s exit_status=launch_failed" % (receipt_path, operation))
        _fail("CARGO_LAUNCH_FAILED", "Cargo could not start")
    finished = dict(prepared)
    finished["outcome"] = {"state": "completed", "cargo_exit_code": completed.returncode}
    try:
        _update_receipt(receipt_path, identity, finished)
    except GateError as exc:
        print("managed-cargo-gate: receipt=%s operation=%s exit_status=%s receipt_update=failed" % (receipt_path, operation, completed.returncode))
        _fail("CARGO_RESULT_AND_RECEIPT_FAILURE", "Cargo exited %s; receipt update failed: %s" % (completed.returncode, exc.detail))
    print("managed-cargo-gate: receipt=%s operation=%s exit_status=%s" % (receipt_path, operation, completed.returncode))
    return completed.returncode


def main(argv=None):
    try:
        return run(list(sys.argv[1:] if argv is None else argv))
    except GateError as exc:
        print("managed-cargo-gate: %s: %s" % (exc.code, exc.detail), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
