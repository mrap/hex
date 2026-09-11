#!/usr/bin/env python3
"""Install a signed macOS app bundle with an owned, recoverable transaction.

This module owns filesystem publication only. Signature creation and signature
verification stay behind the injected signer protocol.
"""
from __future__ import annotations

import argparse
import base64
import contextlib
import ctypes
import dataclasses
import errno
import fcntl
import hashlib
import json
import os
import plistlib
import re
import secrets
import selectors
import select
import signal
import socket
import struct
import threading
import types
import shutil
import stat
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Callable, Mapping, Optional, Protocol


SCHEMA_VERSION = 1
STATE_SCHEMA_VERSION = 1
JOURNAL_SCHEMA_VERSION = 1
RENAME_SWAP = 0x00000002
RENAME_EXCL = 0x00000004
POLICY_RELATIVE = Path("Library/Application Support/Hex/build-signing/policy.json")
MAX_JSON_BYTES = 64 * 1024
MAX_HELPER_BYTES = 1024 * 1024
MAX_PLIST_BYTES = 64 * 1024
MAX_LAUNCHCTL_BYTES = 64 * 1024
SCIPD_LAUNCHD_LABEL = "com.hex.scipd"


class InstallError(RuntimeError):
    """A bounded, operator-facing install or recovery failure."""

    def __init__(self, message: str, *, published: Optional[bool] = None):
        super().__init__(message)
        self.published = published


class Signer(Protocol):
    def stage(self, source: Path, product: str, policy: Path, candidate: Path, receipt: Path) -> Mapping[str, Any]: ...

    def verify_installed(self, bundle: Path, product: str, policy: Optional[Path], expected: Optional[Mapping[str, Any]] = None) -> Mapping[str, Any]: ...


class ProcessSigner:
    """Adapter for the accepted standalone signer helper CLI."""

    def __init__(self, helper: Optional[Path] = None, *, python: str = "/usr/bin/python3", timeout: float = 30.0):
        helper = helper or Path(__file__).with_name("macos-signing.py")
        if not helper.is_file() or helper.is_symlink():
            raise InstallError(f"signer helper is not a regular file: {helper}")
        self.helper = helper.absolute()
        self.python = python
        self.timeout = timeout
        self._owner = None
        self._source = (_RETAINED_SOURCES["macos-signing.py"] if self.helper == _PINNED_SIGNER_PATH
                        else _read_helper_source(self.helper))

    def bind_owner(self, paths, lock_fd: int) -> None:
        _validate_lock_fd(lock_fd, paths)
        self._owner = (paths, lock_fd)

    def _run(self, argv: list[str], *, receipt: Optional[Path] = None) -> Mapping[str, Any]:
        if receipt is not None and os.path.lexists(receipt):
            raise InstallError("signer receipt already exists")
        returncode, output, error = _run_guardian(self, argv)
        if returncode:
            raise InstallError(f"signer helper failed ({returncode}): {error.decode(errors='replace').strip()[-500:]}")
        if receipt is not None:
            # The standalone --receipt contract writes its result to this file,
            # not stdout. Read only after successful owned-child cleanup.
            if output:
                raise InstallError("signer receipt mode returned unexpected stdout")
            return _read_json(receipt, "signer receipt")
        try:
            value = json.loads(output.decode("utf-8"))
        except (ValueError, UnicodeError) as exc:
            raise InstallError("signer helper returned invalid JSON") from exc
        if not isinstance(value, dict):
            raise InstallError("signer helper returned a non-object")
        return value

    def stage(self, source: Path, product: str, policy: Path, candidate: Path, receipt: Path, *, version: str = "1.0.0") -> Mapping[str, Any]:
        return self._run([str(source), product, str(policy), str(candidate), "--version", version, "--receipt", str(receipt)], receipt=receipt)

    def verify_installed(self, bundle: Path, product: str, policy: Optional[Path], expected: Optional[Mapping[str, Any]] = None) -> Mapping[str, Any]:
        args = ["verify-installed", str(bundle), product]
        if policy is not None:
            args.append(str(policy))
        return self._run(args)


@dataclasses.dataclass(frozen=True)
class Product:
    name: str
    app_name: str
    executable: str
    bundle_identifier: str
    cli_relative: Path
    alias_relative: Optional[Path]
    state_name: str
    helper_relative: Path


PRODUCTS = {
    "hex": Product("hex", "Hex.app", "hex", "com.mrap.hex", Path("bin/hex"), Path("bin/hex-agent"), "Hex.app.install-state.json", Path("libexec")),
    "boi": Product("boi", "BOI.app", "boi", "com.mrap.boi", Path("bin/boi"), None, "BOI.app.install-state.json", Path("libexec")),
    "code-intel-daemon": Product("code-intel-daemon", "SCIPD.app", "scipd", "com.mrap.hex.scipd", Path("bin/scipd"), None, "SCIPD.app.install-state.json", Path("libexec/scipd")),
    "code-intel-cli": Product("code-intel-cli", "CQ.app", "cq", "com.mrap.hex.cq", Path("bin/cq"), None, "CQ.app.install-state.json", Path("libexec/cq")),
}


@dataclasses.dataclass(frozen=True)
class Paths:
    product: str
    root: Path
    app: Path
    executable: Path
    cli: Path
    alias: Optional[Path]
    state: Path
    lock: Path
    journal: Path
    helper_dir: Path


def central_policy_path(home: Optional[Path] = None) -> Path:
    """Return the one machine policy path. Tests pass an explicit policy."""
    base = home if home is not None else Path.home()
    return base / POLICY_RELATIVE


def product_paths(product: str, root: Path) -> Paths:
    item = PRODUCTS.get(product)
    if item is None:
        raise InstallError(f"unknown product: {product}")
    if item.helper_relative.is_absolute() or ".." in item.helper_relative.parts:
        raise InstallError(f"helper path escapes product root: {product}")
    root = root.absolute()
    app = root / item.app_name
    return Paths(
        product=product,
        root=root,
        app=app,
        executable=app / "Contents" / "MacOS" / item.executable,
        cli=root / item.cli_relative,
        alias=root / item.alias_relative if item.alias_relative else None,
        state=root / item.state_name,
        lock=root / f".{product}.app-install.lock",
        journal=root / f".{product}.app-install.journal.json",
        helper_dir=root / item.helper_relative,
    )


def _real(path: Path) -> Path:
    return path.absolute().resolve(strict=False)


def _within(path: Path, root: Path) -> bool:
    try:
        _real(path).relative_to(_real(root))
        return True
    except ValueError:
        return False


def _regular_identity(path: Path) -> dict[str, Any]:
    try:
        st = path.lstat()
    except OSError as exc:
        raise InstallError(f"cannot inspect {path}: {exc}") from exc
    return {"dev": st.st_dev, "ino": st.st_ino, "mode": stat.S_IMODE(st.st_mode), "size": st.st_size, "mtime_ns": st.st_mtime_ns}


def _entry_identity(path: Path) -> dict[str, Any]:
    if not os.path.lexists(path):
        return {"present": False}
    st = path.lstat()
    result: dict[str, Any] = {"present": True, "dev": st.st_dev, "ino": st.st_ino, "mode": stat.S_IMODE(st.st_mode), "kind": stat.filemode(st.st_mode)[0]}
    if stat.S_ISREG(st.st_mode):
        result["sha256"] = _sha256(path)
    elif stat.S_ISLNK(st.st_mode):
        result["target"] = os.readlink(path)
    return result


def _revalidate_parent(fd: int, path: Path) -> None:
    actual = os.fstat(fd)
    expected = path.stat()
    if not stat.S_ISDIR(actual.st_mode) or (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino):
        raise InstallError(f"parent directory changed: {path}")


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _read_helper_source(path: Path, expected_sha256: Optional[str] = None) -> bytes:
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise InstallError(f"helper source is not a regular file: {path}")
        chunks = []
        total = 0
        while True:
            chunk = os.read(fd, 65536)
            if not chunk:
                break
            total += len(chunk)
            if total > MAX_HELPER_BYTES:
                raise InstallError(f"helper source is too large: {path}")
            chunks.append(chunk)
    finally:
        os.close(fd)
    content = b"".join(chunks)
    if expected_sha256 is not None and hashlib.sha256(content).hexdigest() != expected_sha256:
        raise InstallError(f"helper source changed during staging: {path}")
    return content


def _tree_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    if not path.is_dir() or path.is_symlink():
        raise InstallError(f"app candidate is not a directory: {path}")
    for child in sorted(path.rglob("*")):
        relative = child.relative_to(path).as_posix().encode()
        st = child.lstat()
        digest.update(relative + b"\0" + str(stat.S_IFMT(st.st_mode)).encode() + b"\0")
        if stat.S_ISREG(st.st_mode):
            digest.update(_sha256(child).encode())
        elif stat.S_ISLNK(st.st_mode):
            digest.update(os.readlink(child).encode())
    return digest.hexdigest()


def _json_digest(value: Any) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def _fsync_dir(fd: int) -> None:
    os.fsync(fd)


@contextlib.contextmanager
def _open_dir(path: Path, *, create: bool = False):
    if create:
        path.mkdir(parents=True, exist_ok=True)
    elif not path.is_dir() or path.is_symlink():
        raise InstallError(f"directory is missing or aliased: {path}")
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    try:
        yield fd
    finally:
        os.close(fd)


@contextlib.contextmanager
def _product_lock(paths: Paths):
    paths.root.mkdir(parents=True, exist_ok=True)
    flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(paths.lock, flags, 0o600)
    except OSError as exc:
        raise InstallError(f"cannot open lock {paths.lock}: {exc}") from exc
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise InstallError(f"lock is not a regular file: {paths.lock}")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as exc:
            if exc.errno in (errno.EACCES, errno.EAGAIN):
                raise InstallError(_busy_message(paths)) from exc
            raise InstallError(f"cannot lock {paths.lock}: {exc}") from exc
        yield fd
    finally:
        os.close(fd)


def _libc_renameatx(source_fd: int, source: str, destination_fd: int, destination: str, flags: int) -> None:
    if sys.platform != "darwin":
        raise InstallError("atomic app publication requires macOS renameatx_np")
    libc = ctypes.CDLL(None, use_errno=True)
    function = getattr(libc, "renameatx_np", None)
    if function is None:
        raise InstallError("macOS renameatx_np is unavailable")
    function.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    function.restype = ctypes.c_int
    if function(source_fd, os.fsencode(source), destination_fd, os.fsencode(destination), flags) != 0:
        error = ctypes.get_errno()
        raise InstallError(f"renameatx_np {source} -> {destination} failed: {os.strerror(error)}")


def _atomic_new(parent_fd: int, source: Path, destination: Path) -> None:
    if source.parent != destination.parent:
        raise InstallError("atomic publication requires one parent")
    _libc_renameatx(parent_fd, source.name, parent_fd, destination.name, RENAME_EXCL)


def _atomic_swap(parent_fd: int, source: Path, destination: Path) -> None:
    if source.parent != destination.parent:
        raise InstallError("atomic swap requires one parent")
    _libc_renameatx(parent_fd, source.name, parent_fd, destination.name, RENAME_SWAP)


def _atomic_move(source_fd: int, source: Path, destination_fd: int, destination: Path) -> None:
    if destination.exists() or destination.is_symlink():
        raise InstallError(f"rollback destination already exists: {destination}")
    _libc_renameatx(source_fd, source.name, destination_fd, destination.name, RENAME_EXCL)


def _write_private(path: Path, payload: bytes) -> None:
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        offset = 0
        while offset < len(payload):
            written = os.write(fd, payload[offset:])
            if written <= 0:
                raise InstallError(f"short write to {path}")
            offset += written
        os.fsync(fd)
    finally:
        os.close(fd)


def _read_json(path: Path, label: str) -> dict[str, Any]:
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise InstallError(f"duplicate key in {label}: {key}")
            result[key] = value
        return result
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0))
        try:
            if not stat.S_ISREG(os.fstat(fd).st_mode):
                raise InstallError(f"{label} is not a regular file: {path}")
            chunks = []
            total = 0
            while True:
                chunk = os.read(fd, 8192)
                if not chunk:
                    break
                total += len(chunk)
                if total > MAX_JSON_BYTES:
                    raise InstallError(f"{label} is too large: {path}")
                chunks.append(chunk)
        finally:
            os.close(fd)
        value = json.loads(b"".join(chunks).decode("utf-8"), object_pairs_hook=pairs)
    except (OSError, ValueError, UnicodeError) as exc:
        raise InstallError(f"invalid {label}: {path}") from exc
    if not isinstance(value, dict):
        raise InstallError(f"invalid {label}: {path}")
    return value


def _write_journal(paths: Paths, journal: Mapping[str, Any]) -> None:
    required = {"schema_version", "transaction_id", "product", "phase", "root", "candidate", "rollback", "expected_old_app", "expected_old_cli"}
    if journal.get("schema_version") != JOURNAL_SCHEMA_VERSION or not required.issubset(journal):
        raise InstallError("invalid journal schema")
    if journal["product"] != paths.product or journal["root"] != str(paths.root):
        raise InstallError("journal product or root does not match fixed paths")
    for key in ("candidate", "rollback"):
        candidate_path = Path(str(journal[key]))
        if not _within(candidate_path, paths.root) or candidate_path.parent != paths.root:
            raise InstallError(f"journal {key} escaped fixed root")
    temporary = paths.journal.with_name(paths.journal.name + f".tmp-{journal['transaction_id']}")
    if os.path.lexists(temporary):
        raise InstallError(f"journal temporary path exists: {temporary}")
    if os.path.lexists(paths.journal):
        current = _read_json(paths.journal, "transaction journal")
        if current.get("transaction_id") != journal.get("transaction_id"):
            raise InstallError("journal ownership changed")
    _write_private(temporary, (json.dumps(journal, sort_keys=True, indent=2) + "\n").encode())
    if os.path.lexists(paths.journal):
        os.replace(temporary, paths.journal)
    else:
        fd = os.open(paths.root, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0))
        try:
            _atomic_new(fd, temporary, paths.journal)
        finally:
            os.close(fd)
    fd = os.open(paths.root, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0))
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _path_guard(paths: Paths) -> None:
    if paths.root.is_symlink() or not _within(paths.app, paths.root) or not _within(paths.cli, paths.root) or paths.app.parent != paths.root or paths.cli.parent != paths.root / "bin":
        raise InstallError("product path escaped fixed root")


def _policy_mode(policy: Path) -> str:
    if policy.is_symlink():
        raise InstallError(f"central signing policy must not be a symlink: {policy}")
    if not policy.exists():
        return "missing"
    if not policy.is_file():
        raise InstallError(f"central signing policy is not a regular file: {policy}")
    # Use the exact retained sibling source, never a second policy validator.
    signer_path = Path(__file__).with_name("macos-signing.py")
    module_name = "_macos_install_policy_" + secrets.token_hex(8)
    module = types.ModuleType(module_name)
    module.__file__ = str(signer_path)
    sys.modules[module_name] = module
    try:
        content = _RETAINED_SOURCES["macos-signing.py"]
        exec(compile(content, str(signer_path), "exec"), module.__dict__)
        reader = getattr(module, "_read_policy", None)
        if not callable(reader):
            raise InstallError("accepted signer policy reader is unavailable")
        reader(policy)
    except Exception as exc:
        raise InstallError(f"invalid central signing policy: {policy}: {exc}") from exc
    finally:
        del sys.modules[module_name]
    return "configured"


def _valid_revision(value: Any) -> bool:
    return isinstance(value, str) and bool(re.fullmatch(r"(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})", value))


def _validate_state(paths: Paths, product: Product, state: Mapping[str, Any]) -> None:
    required = {"schema_version", "product", "mode", "bundle_identifier", "bundle_path", "executable_path", "compatibility_path", "generation", "transaction_id", "team_id", "certificate_sha1", "designated_requirements", "mach_o_uuids", "bundle_sha256", "executable_sha256", "helpers", "source_revision", "version"}
    if state.get("schema_version") != STATE_SCHEMA_VERSION or not required.issubset(state):
        raise InstallError(f"invalid state record: {paths.state}")
    if state["product"] != product.name or state["mode"] != "signed-current" or state["bundle_identifier"] != product.bundle_identifier or not _valid_revision(state["source_revision"]) or not isinstance(state["version"], str) or not state["version"]:
        raise InstallError("state record does not match fixed product map")
    for field in ("bundle_path", "executable_path", "compatibility_path"):
        if not _within(Path(state[field]), paths.root):
            raise InstallError("state record path escaped fixed root")
    if _real(Path(state["bundle_path"])) != _real(paths.app) or _real(Path(state["executable_path"])) != _real(paths.executable) or _real(Path(state["compatibility_path"])) != _real(paths.cli):
        raise InstallError("state record path mismatch")
    helpers = state["helpers"]
    if not isinstance(helpers, dict) or set(helpers) != {"macos-signing.py", "macos-app-install.py"}:
        raise InstallError("state helper provenance is incomplete")
    for helper in helpers.values():
        if not isinstance(helper, dict) or set(helper) != {"sha256", "source_revision"} or len(helper["sha256"]) != 64 or not re.fullmatch(r"[0-9a-fA-F]{64}", helper["sha256"]) or not _valid_revision(helper["source_revision"]):
            raise InstallError("invalid state helper provenance")


def _validate_current_helpers(paths: Paths, helpers: Mapping[str, Any]) -> None:
    for name, helper in helpers.items():
        path = paths.helper_dir / name
        if not path.is_file() or path.is_symlink() or _sha256(path) != helper["sha256"]:
            raise InstallError(f"installed helper does not match state: {name}")


def _validate_helper_dir(paths: Paths) -> None:
    relative = paths.helper_dir.relative_to(paths.root)
    current = paths.root
    for component in relative.parts:
        current /= component
        if os.path.lexists(current) and (current.is_symlink() or not current.is_dir()):
            raise InstallError(f"helper directory is missing or aliased: {current}")


def _verify_matches_state(state: Mapping[str, Any], verified: Mapping[str, Any], product: Product) -> None:
    if verified.get("identifier", verified.get("bundle_identifier")) != product.bundle_identifier:
        raise InstallError("verifier returned the wrong product identifier")
    for state_key, verified_key in (("team_id", "team_id"), ("certificate_sha1", "certificate_sha1"), ("designated_requirements", "designated_requirements"), ("mach_o_uuids", "mach_o_uuids")):
        if state.get(state_key) != verified.get(verified_key):
            raise InstallError(f"installed verifier result does not match state: {state_key}")
    if state.get("bundle_sha256") != verified.get("bundle_sha256", state.get("bundle_sha256")) or state.get("executable_sha256") != verified.get("executable_sha256", state.get("executable_sha256")):
        raise InstallError("installed verifier result does not match current hashes")


def detect_mode(product: str, root: Path, policy_path: Path, signer: Signer) -> str:
    item = PRODUCTS.get(product)
    if item is None:
        raise InstallError(f"unknown product: {product}")
    paths = product_paths(product, root)
    _path_guard(paths)
    if os.path.lexists(paths.journal):
        journal = _read_json(paths.journal, "transaction journal")
        if journal.get("schema_version") != JOURNAL_SCHEMA_VERSION or journal.get("product") != product or journal.get("root") != str(paths.root):
            raise InstallError(f"invalid or foreign install journal: {paths.journal}")
        raise InstallError(f"open install journal requires recovery: {paths.journal}")
    app = os.path.lexists(paths.app)
    cli = os.path.lexists(paths.cli)
    state_exists = os.path.lexists(paths.state)
    policy = _policy_mode(policy_path)
    if state_exists:
        state = _read_json(paths.state, "state record")
        _validate_state(paths, item, state)
        if not app or not cli or (paths.alias is not None and not os.path.lexists(paths.alias)):
            raise InstallError("signed state is missing its app or compatibility path")
        if policy != "configured":
            return "signed-policy-missing"
        verified = signer.verify_installed(paths.app, product, policy_path, state)
        _verify_matches_state(state, verified, item)
        return "signed-current"
    if paths.alias is not None and paths.alias.is_symlink() and _within(paths.alias, paths.app):
        raise InstallError("orphan signed agent alias exists without signed state record")
    if app:
        raise InstallError("app exists without signed state record")
    if cli:
        if not paths.cli.is_file() or paths.cli.is_symlink():
            raise InstallError("legacy compatibility path is not a regular file")
        return "configured-legacy" if policy == "configured" else "legacy-raw"
    return "empty"


def service_owner(product: str, root: Path, signer: Signer, *, policy_path: Optional[Path] = None, lock_held: bool = False) -> dict[str, Any]:
    """Return verified public owner state for a caller holding the product lock."""
    if not lock_held:
        raise InstallError("service_owner requires the inherited product lock")
    item = PRODUCTS.get(product)
    if item is None:
        raise InstallError(f"unknown product: {product}")
    paths = product_paths(product, root)
    _path_guard(paths)
    if not paths.root.is_dir() or not paths.cli.parent.is_dir():
        raise InstallError("product root or compatibility parent is absent")
    if os.path.lexists(paths.journal):
        raise InstallError(f"open install journal blocks service ownership: {paths.journal}")
    if not paths.state.is_file() or not paths.app.is_dir() or not paths.executable.is_file() or not paths.cli.exists():
        raise InstallError("signed service owner is not installed")
    if not paths.cli.is_symlink() or os.readlink(paths.cli) != _make_relative_cli_target(paths):
        raise InstallError("compatibility path does not point to the installed executable")
    if paths.alias is not None and (not paths.alias.is_symlink() or os.readlink(paths.alias) != _make_relative_cli_target(paths)):
        raise InstallError("agent compatibility path does not point to the installed executable")
    state = _read_json(paths.state, "state record")
    _validate_state(paths, item, state)
    _validate_helper_dir(paths)
    _validate_current_helpers(paths, state["helpers"])
    policy = policy_path or central_policy_path()
    if _policy_mode(policy) != "configured":
        raise InstallError("central signing policy is required to verify service ownership")
    verified = dict(signer.verify_installed(paths.app, product, policy, state))
    _verify_matches_state(state, verified, item)
    if verified.get("version") != state["version"]:
        raise InstallError("installed verifier result does not match state: version")
    if _tree_sha256(paths.app) != state["bundle_sha256"] or _sha256(paths.executable) != state["executable_sha256"]:
        raise InstallError("installed app hashes differ from state")
    return {"schema_version": STATE_SCHEMA_VERSION, "product": product, "mode": "signed-current", "policy_available": True, "bundle_path": str(paths.app.absolute()), "executable_path": str(paths.executable.absolute()), "compatibility_path": str(paths.cli.absolute()), "helper_path": str(paths.helper_dir.absolute()), "bundle_identifier": item.bundle_identifier, "generation": state["generation"], "version": state["version"], "team_id": state["team_id"], "certificate_sha1": state["certificate_sha1"], "designated_requirements": state["designated_requirements"], "mach_o_uuids": state["mach_o_uuids"], "bundle_sha256": state["bundle_sha256"], "executable_sha256": state["executable_sha256"], "helpers": state["helpers"], "source_revision": state["source_revision"]}


def _service_plist_path(path: Optional[Path]) -> Path:
    return path or (Path.home() / "Library/LaunchAgents" / f"{SCIPD_LAUNCHD_LABEL}.plist")


def _read_service_plist(path: Path) -> tuple[Optional[dict[str, Any]], dict[str, Any], Optional[str]]:
    if not os.path.lexists(path):
        return None, {"present": False}, None
    if path.is_symlink():
        raise InstallError(f"service plist must not be a symlink: {path}")
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode):
                raise InstallError(f"service plist is not a regular file: {path}")
            if info.st_size > MAX_PLIST_BYTES:
                raise InstallError(f"service plist is too large: {path}")
            payload = os.read(fd, MAX_PLIST_BYTES + 1)
        finally:
            os.close(fd)
        if len(payload) > MAX_PLIST_BYTES:
            raise InstallError(f"service plist is too large: {path}")
        value = plistlib.loads(payload)
    except InstallError:
        raise
    except (OSError, ValueError, plistlib.InvalidFileException) as exc:
        raise InstallError(f"invalid service plist: {path}") from exc
    if not isinstance(value, dict):
        raise InstallError(f"service plist is not a dictionary: {path}")
    identity = {"present": True, "dev": info.st_dev, "ino": info.st_ino, "mode": stat.S_IMODE(info.st_mode), "kind": stat.filemode(info.st_mode)[0], "sha256": hashlib.sha256(payload).hexdigest()}
    return value, identity, identity["sha256"]


def _launchctl_default(argv: list[str], timeout: float = 5.0, lock_fd: Optional[int] = None) -> tuple[int, str, str]:
    command = ["/bin/launchctl", *argv]
    try:
        pass_fds = (lock_fd,) if lock_fd is not None else ()
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, pass_fds=pass_fds)
    except OSError as exc:
        raise InstallError(f"launchctl unavailable: {exc}") from exc
    streams = (process.stdout, process.stderr)
    buffers = [bytearray(), bytearray()]
    deadline = time.monotonic() + timeout
    try:
        with selectors.DefaultSelector() as selector:
            for index, stream in enumerate(streams):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, index)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise InstallError("launchctl timed out")
                for key, _ in selector.select(remaining):
                    chunk = os.read(key.fd, min(65536, MAX_LAUNCHCTL_BYTES + 1 - len(buffers[key.data])))
                    if not chunk:
                        selector.unregister(key.fileobj)
                    else:
                        buffers[key.data].extend(chunk)
                        if len(buffers[key.data]) > MAX_LAUNCHCTL_BYTES:
                            raise InstallError("launchctl output is too large")
        process.wait(timeout=max(0.001, deadline - time.monotonic()))
    except BaseException as exc:
        if process.returncode is None:
            try:
                process.kill()
            except OSError:
                pass
            try:
                process.wait(timeout=1)
            except (OSError, subprocess.TimeoutExpired):
                pass
        if isinstance(exc, InstallError):
            raise
        raise InstallError(f"launchctl failed: {exc}") from exc
    finally:
        for stream in streams:
            stream.close()
    return process.returncode, buffers[0].decode("utf-8", "replace"), buffers[1].decode("utf-8", "replace")


def _launchctl_loaded(launchctl: Callable[[list[str]], tuple[int, str, str]], domain: str) -> tuple[bool, str]:
    returncode, stdout, stderr = launchctl(["print", domain])
    if returncode == 0:
        return True, stdout
    lowered = stderr.lower()
    if "could not find service" in lowered or "service could not be found" in lowered or "no such process" in lowered:
        return False, stdout
    raise InstallError(f"launchctl print failed ({returncode}): {stderr.strip()[-500:]}")


def _launchctl_program(output: str) -> Optional[str]:
    values = [line.lstrip()[len("program = "):] for line in output.splitlines() if line.lstrip().startswith("program = ")]
    if len(values) != 1 or not values[0]:
        raise InstallError("launchctl print lacks one exact program field")
    return values[0]


def _replace_bound(parent_fd: int, temporary: Path, destination: Path) -> None:
    os.rename(temporary.name, destination.name, src_dir_fd=parent_fd, dst_dir_fd=parent_fd)


def _atomic_service_plist(path: Path, value: Mapping[str, Any], previous: Mapping[str, Any]) -> bool:
    if _entry_identity(path) != dict(previous):
        raise InstallError("service plist changed before publication")
    parent = path.parent
    if not parent.is_dir() or parent.is_symlink():
        raise InstallError(f"service plist parent is missing or aliased: {parent}")
    parent_fd = os.open(parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0))
    temporary = parent / f".{path.name}.candidate-{secrets.token_hex(8)}"
    published = False
    try:
        _write_private(temporary, plistlib.dumps(dict(value), fmt=plistlib.FMT_XML, sort_keys=False))
        if _entry_identity(path) != dict(previous):
            raise InstallError("service plist changed before publication")
        _revalidate_parent(parent_fd, parent)
        _replace_bound(parent_fd, temporary, path)
        published = True
        os.fsync(parent_fd)
    except BaseException as exc:
        if temporary.exists():
            temporary.unlink()
        if isinstance(exc, InstallError):
            raise InstallError(str(exc), published=published) from exc
        raise InstallError(f"service plist publication failed: {exc}", published=published) from exc
    finally:
        os.close(parent_fd)
    return published


def _service_plist_sha256(path: Path) -> str:
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_PLIST_BYTES:
                raise InstallError("service plist changed or is too large")
            payload = os.read(fd, MAX_PLIST_BYTES + 1)
        finally:
            os.close(fd)
    except InstallError:
        raise
    except OSError as exc:
        raise InstallError(f"cannot hash service plist: {exc}") from exc
    if len(payload) > MAX_PLIST_BYTES:
        raise InstallError("service plist changed or is too large")
    return hashlib.sha256(payload).hexdigest()


def _service_receipt_path(paths: Paths) -> Path:
    return paths.root / "SCIPD.service-state.json"


def _service_pending_path(paths: Paths) -> Path:
    return paths.root / "SCIPD.service-reconcile-pending.json"


def _read_service_receipt(paths: Paths) -> Optional[dict[str, Any]]:
    path = _service_receipt_path(paths)
    if not os.path.lexists(path):
        return None
    return _read_json(path, "service state receipt")


def _read_service_pending(paths: Paths) -> Optional[dict[str, Any]]:
    path = _service_pending_path(paths)
    if not os.path.lexists(path):
        return None
    return _read_json(path, "service reconcile pending marker")


def _write_service_pending(paths: Paths, value: Mapping[str, Any]) -> None:
    path = _service_pending_path(paths)
    if os.path.lexists(path) and (path.is_symlink() or not path.is_file()):
        raise InstallError(f"service pending marker is not a regular file: {path}", published=True)
    temporary = path.with_name(path.name + f".tmp-{secrets.token_hex(8)}")
    _write_private(temporary, (json.dumps(dict(value), sort_keys=True) + "\n").encode())
    try:
        fd = os.open(paths.root, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0))
        try:
            _revalidate_parent(fd, paths.root)
            _replace_bound(fd, temporary, path)
            os.fsync(fd)
        finally:
            os.close(fd)
    except BaseException as exc:
        if temporary.exists():
            temporary.unlink()
        raise InstallError(f"service pending marker publication failed: {exc}", published=True) from exc


def _clear_service_pending(paths: Paths) -> None:
    path = _service_pending_path(paths)
    if not os.path.lexists(path):
        return
    if path.is_symlink() or not path.is_file():
        raise InstallError(f"service pending marker is not a regular file: {path}", published=True)
    try:
        fd = os.open(paths.root, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0))
        try:
            _revalidate_parent(fd, paths.root)
            os.unlink(path.name, dir_fd=fd)
            os.fsync(fd)
        finally:
            os.close(fd)
    except BaseException as exc:
        raise InstallError(f"service pending marker removal failed: {exc}", published=True) from exc


def _write_service_receipt(paths: Paths, value: Mapping[str, Any]) -> None:
    path = _service_receipt_path(paths)
    if os.path.lexists(path) and (path.is_symlink() or not path.is_file()):
        raise InstallError(f"service state receipt is not a regular file: {path}", published=True)
    temporary = path.with_name(path.name + f".tmp-{secrets.token_hex(8)}")
    _write_private(temporary, (json.dumps(dict(value), sort_keys=True) + "\n").encode())
    try:
        fd = os.open(paths.root, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0))
        try:
            _revalidate_parent(fd, paths.root)
            _replace_bound(fd, temporary, path)
            os.fsync(fd)
        finally:
            os.close(fd)
    except BaseException as exc:
        if temporary.exists():
            temporary.unlink()
        raise InstallError(f"service state receipt publication failed: {exc}", published=True) from exc


def _validate_service_pending(pending: Mapping[str, Any], owner: Mapping[str, Any], plist_sha256: str, product: str) -> None:
    required = {"schema_version", "product", "generation", "plist_sha256", "bundle_identifier", "executable_path", "phase", "original_loaded"}
    if set(pending) != required or pending.get("schema_version") != 1 or pending.get("product") != product or pending.get("phase") != "reload-pending" or pending.get("original_loaded") is not True:
        raise InstallError("invalid service reconcile pending marker")
    if pending.get("generation") != owner["generation"] or pending.get("plist_sha256") != plist_sha256 or pending.get("bundle_identifier") != owner["bundle_identifier"] or pending.get("executable_path") != owner["executable_path"]:
        raise InstallError("service reconcile pending marker does not match current signed service")


def service_reconcile(product: str, root: Path, signer: Signer, *, policy_path: Optional[Path] = None, dry_run: bool = False, launchctl: Optional[Callable[[list[str]], tuple[int, str, str]]] = None, plist_path: Optional[Path] = None) -> dict[str, Any]:
    if product != "code-intel-daemon":
        raise InstallError("service-reconcile supports only code-intel-daemon")
    paths = product_paths(product, root)
    _path_guard(paths)
    plist = _service_plist_path(plist_path)
    domain = f"gui/{os.getuid()}/{SCIPD_LAUNCHD_LABEL}"
    with _product_lock(paths) as lock_fd:
        if launchctl is None:
            launchctl = lambda argv: _launchctl_default(argv, lock_fd=lock_fd)
        if isinstance(signer, ProcessSigner):
            signer.bind_owner(paths, lock_fd)
        owner = service_owner(product, root, signer, policy_path=policy_path, lock_held=True)
        loaded, launch_output = _launchctl_loaded(launchctl, domain)
        live_program = _launchctl_program(launch_output) if loaded else None
        current, previous, plist_sha256 = _read_service_plist(plist)
        pending = _read_service_pending(paths)
        if current is None:
            if pending is not None:
                raise InstallError("pending service reload has no readable plist")
            if loaded:
                raise InstallError("loaded scipd service has no readable plist")
            return {"schema_version": 1, "product": product, "mode": "signed-current", "service_action": "absent", "service_needs_change": False, "service_recovery_pending": False, "published": False, "plist_path": str(plist.absolute()), "executable_path": owner["executable_path"]}
        if current.get("Label") != SCIPD_LAUNCHD_LABEL:
            raise InstallError("service plist has the wrong Label")
        arguments = current.get("ProgramArguments")
        allowed_arguments = {str(paths.cli.absolute()), str(paths.executable.absolute())}
        if not isinstance(arguments, list) or len(arguments) != 1 or not isinstance(arguments[0], str) or arguments[0] not in allowed_arguments:
            raise InstallError("service plist has unsupported ProgramArguments")
        program = current.get("Program")
        if program is not None and (not isinstance(program, str) or program not in allowed_arguments):
            raise InstallError("service plist has unsupported Program")
        associated = current.get("AssociatedBundleIdentifiers", [owner["bundle_identifier"]])
        if not isinstance(associated, list) or any(not isinstance(item, str) for item in associated):
            raise InstallError("service plist has invalid AssociatedBundleIdentifiers")
        desired = dict(current)
        desired["ProgramArguments"] = [owner["executable_path"]]
        if program is not None:
            desired["Program"] = owner["executable_path"]
        desired["AssociatedBundleIdentifiers"] = [owner["bundle_identifier"]]
        receipt = _read_service_receipt(paths)
        if pending is not None:
            _validate_service_pending(pending, owner, plist_sha256, product)
        receipt_matches = receipt == {"schema_version": 1, "product": product, "generation": owner["generation"], "plist_sha256": plist_sha256, "bundle_identifier": owner["bundle_identifier"], "executable_path": owner["executable_path"]}
        recovery_pending = pending is not None
        needs_change = desired != current or (loaded and live_program != owner["executable_path"]) or (loaded and not receipt_matches) or recovery_pending
        if dry_run or not needs_change:
            action = "would-restart" if dry_run and needs_change and (loaded or recovery_pending) else "would-update-stopped" if dry_run and needs_change else "loaded" if loaded else "stopped"
            return {"schema_version": 1, "product": product, "mode": "signed-current", "service_action": action, "service_needs_change": needs_change, "service_recovery_pending": recovery_pending, "published": False, "plist_path": str(plist.absolute()), "executable_path": owner["executable_path"]}
        published = _atomic_service_plist(plist, desired, previous)
        try:
            if not loaded and not recovery_pending:
                return {"schema_version": 1, "product": product, "mode": "signed-current", "service_action": "updated-stopped", "service_needs_change": True, "service_recovery_pending": recovery_pending, "published": published, "plist_path": str(plist.absolute()), "executable_path": owner["executable_path"]}
            planned_sha256 = hashlib.sha256(plistlib.dumps(dict(desired), fmt=plistlib.FMT_XML, sort_keys=False)).hexdigest()
            if _service_plist_sha256(plist) != planned_sha256:
                raise InstallError("published service plist differs from planned bytes", published=published)
            pending_value = {"schema_version": 1, "product": product, "generation": owner["generation"], "plist_sha256": planned_sha256, "bundle_identifier": owner["bundle_identifier"], "executable_path": owner["executable_path"], "phase": "reload-pending", "original_loaded": True}
            _write_service_pending(paths, pending_value)
            if loaded:
                returncode, _, stderr = launchctl(["bootout", domain])
                if returncode:
                    raise InstallError(f"service bootout failed after plist publication: {stderr.strip()[-500:]}", published=True)
            returncode, _, stderr = launchctl(["bootstrap", f"gui/{os.getuid()}", str(plist.absolute())])
            if returncode:
                raise InstallError(f"service bootstrap failed after plist publication: {stderr.strip()[-500:]}", published=True)
            verified_loaded, verified_output = _launchctl_loaded(launchctl, domain)
            if not verified_loaded or _launchctl_program(verified_output) != owner["executable_path"]:
                raise InstallError("service reload did not load the verified scipd executable", published=True)
            receipt_value = {"schema_version": 1, "product": product, "generation": owner["generation"], "plist_sha256": planned_sha256, "bundle_identifier": owner["bundle_identifier"], "executable_path": owner["executable_path"]}
            _write_service_receipt(paths, receipt_value)
            _clear_service_pending(paths)
        except InstallError as exc:
            if exc.published is None:
                exc.published = True
            raise
        except OSError as exc:
            raise InstallError(f"service reconciliation failed after plist publication: {exc}", published=True) from exc
        return {"schema_version": 1, "product": product, "mode": "signed-current", "service_action": "restarted" if loaded else "recovered", "service_needs_change": True, "service_recovery_pending": recovery_pending, "published": True, "plist_path": str(plist.absolute()), "executable_path": owner["executable_path"]}


def preflight(product: str, root: Path, signer: Signer, *, policy_path: Optional[Path] = None, lock_held: bool = False) -> dict[str, Any]:
    """Return bounded mode and owner information without mutating the root."""
    if not lock_held:
        raise InstallError("preflight requires the inherited product lock")
    policy = policy_path or central_policy_path()
    mode = detect_mode(product, root, policy, signer)
    result = {"schema_version": STATE_SCHEMA_VERSION, "product": product, "mode": mode, "policy_available": _policy_mode(policy) == "configured", "managed": _policy_mode(policy) == "configured" or product_paths(product, root).state.exists(), "policy_path": str(policy.absolute()), "state_path": str(product_paths(product, root).state.absolute()), "lock_path": str(product_paths(product, root).lock.absolute()), "journal_path": str(product_paths(product, root).journal.absolute())}
    if mode.startswith("signed"):
        result["service_owner"] = service_owner(product, root, signer, policy_path=policy, lock_held=True)
        result["source_revision"] = result["service_owner"]["source_revision"]
    return result


def _same_identity(path: Path, expected: Mapping[str, Any]) -> bool:
    return _entry_identity(path) == dict(expected)


def _make_relative_cli_target(paths: Paths) -> str:
    return os.path.relpath(paths.executable, paths.cli.parent)


def _new_candidate(paths: Paths, transaction_id: str) -> Path:
    return paths.root / f".{paths.app.stem}.candidate-{transaction_id}.app"


def _new_cli_candidate(paths: Paths, transaction_id: str) -> Path:
    return paths.cli.parent / f".{paths.cli.name}.candidate-{transaction_id}"


def _rollback_dir(paths: Paths, transaction_id: str) -> Path:
    return paths.root / f".{paths.product}.app-install-rollback-{transaction_id}"


def _clear_journal(paths: Paths, transaction_id: str) -> None:
    if not paths.journal.is_file() or _read_json(paths.journal, "transaction journal").get("transaction_id") != transaction_id:
        raise InstallError("journal ownership changed before cleanup")
    paths.journal.unlink()
    with _open_dir(paths.root) as root_fd:
        _fsync_dir(root_fd)


def _restore_public(parent_fd: int, target: Path, rollback: Path, old_name: str, had_old: bool, transaction_id: str, expected_current: Mapping[str, Any], fallback: Optional[Path]) -> None:
    if not os.path.lexists(target):
        return
    if _entry_identity(target) != dict(expected_current):
        raise InstallError(f"refusing rollback over actor replacement: {target}")
    rollback_fd = os.open(rollback, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0))
    try:
        if had_old:
            archived = rollback / old_name
            if not os.path.lexists(archived) and fallback is not None and os.path.lexists(fallback):
                _atomic_move(parent_fd, fallback, rollback_fd, archived)
            temporary = target.parent / f".{target.name}.failed-{transaction_id}"
            if os.path.lexists(temporary):
                raise InstallError(f"rollback temporary path exists: {temporary}")
            _atomic_move(rollback_fd, rollback / old_name, parent_fd, temporary)
            _revalidate_parent(parent_fd, target.parent)
            if _entry_identity(target) != dict(expected_current):
                raise InstallError(f"refusing rollback over actor replacement: {target}")
            _atomic_swap(parent_fd, temporary, target)
            _atomic_move(parent_fd, temporary, rollback_fd, rollback / f"failed-{old_name}")
        else:
            _atomic_move(parent_fd, target, rollback_fd, rollback / f"failed-{old_name}")
    finally:
        os.close(rollback_fd)


def _staging_identity(path: Path) -> dict[str, Any]:
    identity = _entry_identity(path)
    if identity.get("kind") == "d":
        identity["tree_sha256"] = _tree_sha256(path)
    return identity


def _staging_targets(paths: Paths) -> dict[str, Path]:
    targets = {"app": paths.app, "cli": paths.cli, "state": paths.state,
               "signer": paths.helper_dir / "macos-signing.py",
               "installer": paths.helper_dir / "macos-app-install.py"}
    if paths.alias is not None:
        targets["alias"] = paths.alias
    return targets


def _staging_prestate(paths: Paths) -> dict[str, Any]:
    return {"targets": {name: _staging_identity(path) for name, path in _staging_targets(paths).items()},
            "parents": {str(path.relative_to(paths.root)): _entry_identity(path)
                        for path in (paths.root, paths.cli.parent, paths.helper_dir)}}


class StagingRecoveryError(InstallError):
    def __init__(self, detail: str, archive: Optional[Path] = None):
        super().__init__(detail, published=None)
        self.archive = str(archive) if archive is not None else None


def abandon_staging(product: str, root: Path) -> dict[str, Any]:
    """Archive a qualified, stopped prepublication attempt. Never restore public files."""
    paths = product_paths(product, root)
    _path_guard(paths)
    if not paths.root.is_dir() or paths.root.is_symlink():
        raise StagingRecoveryError("staging recovery requires an existing fixed root")
    archive = None
    try:
        with _product_lock(paths), _open_dir(paths.root) as root_fd, \
                _open_dir(paths.cli.parent) as cli_fd, _open_dir(paths.helper_dir) as helper_fd:
            journal_identity = _entry_identity(paths.journal)
            journal = _read_json(paths.journal, "staging journal")
            if _entry_identity(paths.journal) != journal_identity:
                raise InstallError("staging journal changed while read")
            transaction = journal.get("transaction_id")
            if (journal.get("schema_version") != JOURNAL_SCHEMA_VERSION or
                    journal.get("product") != product or journal.get("root") != str(paths.root) or
                    journal.get("phase") != "staging" or
                    any(key in journal for key in ("candidate_hash", "verified", "old_helpers")) or
                    not isinstance(transaction, str) or not re.fullmatch(r"[0-9a-f]{24}", transaction)):
                raise InstallError("only a matching prepublication staging journal can be abandoned")
            candidate = _new_candidate(paths, transaction)
            receipt = candidate.with_name(candidate.name + ".receipt.json")
            rollback = _rollback_dir(paths, transaction)
            if journal.get("candidate") != str(candidate) or journal.get("rollback") != str(rollback):
                raise InstallError("staging journal paths do not match fixed transaction paths")
            expected = journal.get("staging_prestate")
            owned = journal.get("staging_owned")
            artifacts = {"candidate": candidate, "receipt": receipt, "rollback": rollback}
            if not isinstance(expected, dict) or not isinstance(owned, dict) or set(owned) != set(artifacts):
                raise InstallError("staging journal lacks qualified prestate or owned evidence; manual review required")
            def unchanged():
                for fd, parent in ((root_fd, paths.root), (cli_fd, paths.cli.parent), (helper_fd, paths.helper_dir)):
                    _revalidate_parent(fd, parent)
                if _staging_prestate(paths) != expected:
                    raise InstallError("public staging prestate changed; refusing abandonment")
                if _entry_identity(paths.journal) != journal_identity:
                    raise InstallError("staging journal changed during recovery")
            unchanged()
            for name, path in artifacts.items():
                if _staging_identity(path) != owned[name]:
                    raise InstallError("owned staging evidence changed: " + name)
                if owned[name].get("present"):
                    required_kind = "-" if name == "receipt" else "d"
                    if owned[name].get("kind") != required_kind:
                        raise InstallError("unexpected staging evidence type: " + name)
            if rollback.exists() and any(rollback.iterdir()):
                raise InstallError("nonempty rollback is not prepublication staging")
            archive = paths.root / f".{product}.abandoned-staging-{transaction}"
            archive.mkdir(mode=0o700)
            archive_identity = _entry_identity(archive)
            with _open_dir(archive) as archive_fd:
                manifest = {"schema_version": 1, "product": product, "transaction_id": transaction,
                            "journal_identity": journal_identity, "staging_owned": owned,
                            "staging_prestate": expected}
                _write_private(archive / "recovery-manifest.json", (json.dumps(manifest, sort_keys=True, indent=2) + "\n").encode())
                _fsync_dir(archive_fd)
                _fsync_dir(root_fd)
                for name, path in artifacts.items():
                    unchanged()
                    _revalidate_parent(archive_fd, archive)
                    if _entry_identity(archive) != archive_identity or _staging_identity(path) != owned[name]:
                        raise InstallError("staging evidence or archive changed before move")
                    if owned[name].get("present"):
                        _atomic_move(root_fd, path, archive_fd, archive / path.name)
                        if _staging_identity(archive / path.name) != owned[name]:
                            raise InstallError("staging evidence changed while archived")
                _fsync_dir(archive_fd)
                _fsync_dir(root_fd)
                unchanged()
                _revalidate_parent(archive_fd, archive)
                # The journal remains the fail-closed blocker until all evidence is archived.
                _atomic_move(root_fd, paths.journal, archive_fd, archive / paths.journal.name)
                _fsync_dir(archive_fd)
                _fsync_dir(root_fd)
            return {"schema_version": 1, "product": product, "action": "abandoned-staging",
                    "transaction_id": transaction, "archive_path": str(archive), "published": False}
    except (InstallError, OSError) as exc:
        raise StagingRecoveryError(str(exc), archive) from exc


def install(product: str, root: Path, source: Path, signer: Signer, *, policy_path: Optional[Path] = None, helper_provenance: Optional[Mapping[str, Any]] = None, helper_sources: Optional[Mapping[str, Path]] = None, source_revision: Optional[str] = None, version: str = "1.0.0") -> dict[str, Any]:
    """Stage and publish one complete app. The caller owns no service action."""
    item = PRODUCTS.get(product)
    if item is None:
        raise InstallError(f"unknown product: {product}")
    paths = product_paths(product, root)
    policy = policy_path or central_policy_path()
    _path_guard(paths)
    if source_revision is None or not _valid_revision(source_revision) or helper_provenance is None:
        raise InstallError("source revision and both helper provenance records are required")
    if set(helper_provenance) != {"macos-signing.py", "macos-app-install.py"}:
        raise InstallError("both helper provenance records are required")
    if not all(isinstance(value, Mapping) and set(value) == {"sha256", "source_revision"} and _valid_revision(value.get("source_revision")) and re.fullmatch(r"[0-9a-fA-F]{64}", str(value.get("sha256"))) for value in helper_provenance.values()):
        raise InstallError("helper source revisions must be full git SHA values")
    if _policy_mode(policy) != "configured":
        if os.path.lexists(paths.state) or os.path.lexists(paths.app):
            raise InstallError(f"central signing policy missing in signed mode: {policy}")
        raise InstallError(f"central signing policy required for signed publication: {policy}")
    transaction_id = secrets.token_hex(12)
    candidate = _new_candidate(paths, transaction_id)
    receipt = candidate.with_name(candidate.name + ".receipt.json")
    rollback = _rollback_dir(paths, transaction_id)
    old_app = _entry_identity(paths.app)
    old_cli = _entry_identity(paths.cli)
    old_alias = _entry_identity(paths.alias) if paths.alias is not None else {"present": False}
    old_state = _entry_identity(paths.state)
    if helper_sources is None:
        raise InstallError("both helper source files are required")
    if set(helper_sources) != set(helper_provenance):
        raise InstallError("both helper source files are required")
    for name, source_path in helper_sources.items():
        if not source_path.is_file() or source_path.is_symlink():
            raise InstallError(f"helper source is not a regular file: {source_path}")
    paths.root.mkdir(parents=True, exist_ok=True)
    _validate_helper_dir(paths)
    paths.helper_dir.mkdir(parents=True, exist_ok=True)
    with _product_lock(paths) as product_lock_fd, _open_dir(paths.root) as app_fd, _open_dir(paths.cli.parent, create=True) as cli_fd, _open_dir(paths.helper_dir) as helper_fd:
        if os.path.lexists(paths.journal):
            raise InstallError(f"open install journal requires recovery: {paths.journal}")
        if isinstance(signer, ProcessSigner):
            signer.bind_owner(paths, product_lock_fd)
        initial_mode = detect_mode(product, root, policy, signer)
        if initial_mode == "signed-policy-missing":
            raise InstallError("signed product cannot be replaced while central policy is missing")
        if initial_mode == "signed-current" and old_app != _entry_identity(paths.app):
            raise InstallError("app changed during state detection")
        journal: dict[str, Any] = {"schema_version": JOURNAL_SCHEMA_VERSION, "transaction_id": transaction_id, "product": product, "phase": "staging", "root": str(paths.root), "candidate": str(candidate), "rollback": str(rollback), "expected_old_app": old_app, "expected_old_cli": old_cli}
        journal["staging_prestate"] = _staging_prestate(paths)
        published: list[tuple[int, Path, str, bool, dict[str, Any], Optional[Path]]] = []
        _write_journal(paths, journal)
        try:
            if isinstance(signer, ProcessSigner):
                result = dict(signer.stage(source, product, policy, candidate, receipt, version=version))
            else:
                result = dict(signer.stage(source, product, policy, candidate, receipt))
            verified = dict(signer.verify_installed(candidate, product, policy, result))
            if verified.get("identifier", verified.get("bundle_identifier")) != item.bundle_identifier:
                raise InstallError("signer result has the wrong product identifier")
            if verified.get("version") != version:
                raise InstallError("verified candidate version differs from requested version")
            candidate_executable = candidate / "Contents" / "MacOS" / item.executable
            if not candidate.is_dir() or candidate.is_symlink() or not candidate_executable.is_file() or candidate_executable.is_symlink():
                raise InstallError("signer did not produce a complete app candidate")
            if _entry_identity(paths.app) != old_app or _entry_identity(paths.cli) != old_cli or (paths.alias is not None and _entry_identity(paths.alias) != old_alias):
                raise InstallError("destination changed during staging")
            rollback.mkdir()
            journal.update({"phase": "app-swap", "candidate_hash": _tree_sha256(candidate), "verified": verified})
            _write_journal(paths, journal)
            helper_candidates = {}
            old_helpers = {}
            for name, source_path in helper_sources.items():
                candidate_helper = paths.helper_dir / f".{name}.candidate-{transaction_id}"
                _write_private(candidate_helper, _read_helper_source(source_path, str(helper_provenance[name]["sha256"])))
                helper_candidates[name] = candidate_helper
                old_helpers[name] = _entry_identity(paths.helper_dir / name)
            if _entry_identity(paths.app) != old_app or _entry_identity(paths.cli) != old_cli:
                raise InstallError("destination changed before helper publication")
            journal.update({"phase": "helper-swap", "old_helpers": old_helpers})
            _write_journal(paths, journal)
            for name, candidate_helper in helper_candidates.items():
                target = paths.helper_dir / name
                _revalidate_parent(helper_fd, paths.helper_dir)
                if old_helpers[name]["present"]:
                    _atomic_swap(helper_fd, candidate_helper, target)
                    published.append((helper_fd, target, f"previous-{name}", True, _entry_identity(target), candidate_helper))
                    rollback_fd = os.open(rollback, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
                    try:
                        _atomic_move(helper_fd, candidate_helper, rollback_fd, rollback / f"previous-{name}")
                    finally:
                        os.close(rollback_fd)
                else:
                    _atomic_new(helper_fd, candidate_helper, target)
                    published.append((helper_fd, target, f"previous-{name}", False, _entry_identity(target), None))
            if _entry_identity(paths.app) != old_app or _entry_identity(paths.cli) != old_cli:
                raise InstallError("destination changed before app publication")
            if old_app["present"]:
                _revalidate_parent(app_fd, paths.root)
                _atomic_swap(app_fd, candidate, paths.app)
                published_app = _entry_identity(paths.app)
                published.append((app_fd, paths.app, "previous-app", True, published_app, candidate))
                rollback_fd = os.open(rollback, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
                try:
                    _atomic_move(app_fd, candidate, rollback_fd, rollback / "previous-app")
                finally:
                    os.close(rollback_fd)
            else:
                _revalidate_parent(app_fd, paths.root)
                _atomic_new(app_fd, candidate, paths.app)
                published_app = _entry_identity(paths.app)
                published.append((app_fd, paths.app, "previous-app", False, published_app, None))
            cli_candidate = _new_cli_candidate(paths, transaction_id)
            cli_candidate.parent.mkdir(parents=True, exist_ok=True)
            os.symlink(_make_relative_cli_target(paths), cli_candidate)
            journal["phase"] = "cli-swap"
            _write_journal(paths, journal)
            if _entry_identity(paths.app) != published_app or _entry_identity(paths.cli) != old_cli:
                raise InstallError("destination changed before compatibility publication")
            if old_cli["present"]:
                _revalidate_parent(cli_fd, paths.cli.parent)
                _atomic_swap(cli_fd, cli_candidate, paths.cli)
                published_cli = _entry_identity(paths.cli)
                published.append((cli_fd, paths.cli, "previous-cli", True, published_cli, cli_candidate))
                rollback_fd = os.open(rollback, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
                try:
                    _atomic_move(cli_fd, cli_candidate, rollback_fd, rollback / "previous-cli")
                finally:
                    os.close(rollback_fd)
            else:
                _revalidate_parent(cli_fd, paths.cli.parent)
                _atomic_new(cli_fd, cli_candidate, paths.cli)
                published_cli = _entry_identity(paths.cli)
                published.append((cli_fd, paths.cli, "previous-cli", False, published_cli, None))
            published_alias = published_cli
            if paths.alias is not None:
                alias_candidate = paths.alias.parent / f".{paths.alias.name}.candidate-{transaction_id}"
                os.symlink(_make_relative_cli_target(paths), alias_candidate)
                journal["phase"] = "alias-swap"
                _write_journal(paths, journal)
                if _entry_identity(paths.app) != published_app or _entry_identity(paths.cli) != published_cli or _entry_identity(paths.alias) != old_alias:
                    raise InstallError("destination changed before agent compatibility publication")
                if old_alias["present"]:
                    _revalidate_parent(cli_fd, paths.alias.parent)
                    _atomic_swap(cli_fd, alias_candidate, paths.alias)
                    published_alias = _entry_identity(paths.alias)
                    published.append((cli_fd, paths.alias, "previous-alias", True, published_alias, alias_candidate))
                    rollback_fd = os.open(rollback, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
                    try:
                        _atomic_move(cli_fd, alias_candidate, rollback_fd, rollback / "previous-alias")
                    finally:
                        os.close(rollback_fd)
                else:
                    _revalidate_parent(cli_fd, paths.alias.parent)
                    _atomic_new(cli_fd, alias_candidate, paths.alias)
                    published_alias = _entry_identity(paths.alias)
                    published.append((cli_fd, paths.alias, "previous-alias", False, published_alias, None))
            state = {"schema_version": STATE_SCHEMA_VERSION, "product": product, "mode": "signed-current", "bundle_identifier": item.bundle_identifier, "bundle_path": str(paths.app.absolute()), "executable_path": str(paths.executable.absolute()), "compatibility_path": str(paths.cli.absolute()), "generation": transaction_id, "transaction_id": transaction_id, "version": verified["version"], "bundle_sha256": _tree_sha256(paths.app), "executable_sha256": _sha256(paths.executable), "previous_compatibility": old_cli, "team_id": verified.get("team_id"), "certificate_sha1": verified.get("certificate_sha1"), "designated_requirements": verified.get("designated_requirements"), "mach_o_uuids": verified.get("mach_o_uuids"), "source_revision": source_revision, "signer_helper_sha256": verified.get("signer_helper_sha256"), "helpers": dict(helper_provenance or {})}
            state_temp = paths.state.with_name(paths.state.name + f".tmp-{transaction_id}")
            _write_private(state_temp, (json.dumps(state, sort_keys=True, indent=2) + "\n").encode())
            journal["phase"] = "state-swap"
            _write_journal(paths, journal)
            if _entry_identity(paths.app) != published_app or _entry_identity(paths.cli) != published_cli or (paths.alias is not None and _entry_identity(paths.alias) != published_alias) or _entry_identity(paths.state) != old_state:
                raise InstallError("destination changed before state publication")
            if os.path.lexists(paths.state):
                _revalidate_parent(app_fd, paths.root)
                _atomic_swap(app_fd, state_temp, paths.state)
                published.append((app_fd, paths.state, "previous-state.json", True, _entry_identity(paths.state), state_temp))
                rollback_fd = os.open(rollback, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
                try:
                    _atomic_move(app_fd, state_temp, rollback_fd, rollback / "previous-state.json")
                finally:
                    os.close(rollback_fd)
            else:
                _revalidate_parent(app_fd, paths.root)
                _atomic_new(app_fd, state_temp, paths.state)
                published.append((app_fd, paths.state, "previous-state.json", False, _entry_identity(paths.state), None))
            # Persist every mutated directory before marking the journal committed.
            with _open_dir(rollback) as rollback_fd:
                for directory_fd in (helper_fd, cli_fd, rollback_fd, app_fd):
                    _fsync_dir(directory_fd)
            journal["phase"] = "committed"
            _write_journal(paths, journal)
            _clear_journal(paths, transaction_id)
            return state
        except Exception as exc:
            rollback_errors = []
            staging_evidence_error = None
            if journal["phase"] == "staging" and not published:
                try:
                    # This is a cooperative, completed signer call, not evidence from
                    # a crash or a still-running guardian. Such attempts stay blocked.
                    if os.path.lexists(_failure_path(paths)):
                        raise InstallError("signer cleanup remains unresolved")
                    journal["staging_owned"] = {"candidate": _staging_identity(candidate),
                                                "receipt": _staging_identity(receipt),
                                                "rollback": _staging_identity(rollback)}
                    _write_journal(paths, journal)
                except Exception as evidence_exc:
                    staging_evidence_error = "staging evidence could not be retained: " + str(evidence_exc)
            if rollback.is_dir():
                for parent_fd, target, old_name, had_old, expected_current, fallback in reversed(published):
                    try:
                        _restore_public(parent_fd, target, rollback, old_name, had_old, transaction_id, expected_current, fallback)
                    except Exception as rollback_exc:
                        rollback_errors.append(str(rollback_exc))
            detail = str(exc)
            if staging_evidence_error:
                detail += "; " + staging_evidence_error
            if rollback_errors:
                detail += "; rollback failed: " + "; ".join(rollback_errors)
            raise InstallError(detail, published=bool(published) or bool(rollback_errors)) from exc


# Private entrypoints remain in this provenance-checked helper. No environment
# variable selects code, product, or process authority.
_INTERNAL_ENTRY = r'''import base64, hashlib, json, os, sys, types
path, expected, entry, descriptor = sys.argv[1:5]
with os.fdopen(int(descriptor), 'rb') as stream:
    payload = stream.read(2800001)
if len(payload) > 2800000 or hashlib.sha256(payload).hexdigest() != expected:
    raise ValueError('retained helper frame mismatch')
contents = {name: base64.b64decode(value, validate=True) for name, value in json.loads(payload).items()}
module = types.ModuleType('_app_owned_entry'); module.__file__ = path
module.contents = contents
sys.modules[module.__name__] = module
exec(compile(contents['macos-app-install.py'], path, 'exec'), module.__dict__)
raise SystemExit(getattr(module, entry)(sys.argv[5:]))
'''
CLEANUP_BUDGET = 5.0
CONTROL_LIMIT = 2048


def _entry_command(entry: str, arguments: list[str], signer_source: bytes):
    retained = dict(_RETAINED_SOURCES)
    retained['macos-signing.py'] = signer_source
    payload = json.dumps({name: base64.b64encode(value).decode('ascii')
                          for name, value in retained.items()}).encode()
    reader, writer = os.pipe()
    command = ['/usr/bin/python3', '-I', '-B', '-c', _INTERNAL_ENTRY,
               str(_PINNED_INSTALLER_PATH), hashlib.sha256(payload).hexdigest(),
               entry, str(reader), *arguments]
    return command, reader, writer, payload


def _send_source(fd: int, payload: bytes, deadline: float) -> None:
    os.set_blocking(fd, False)
    remaining = memoryview(payload)
    while remaining:
        budget = deadline - time.monotonic()
        if budget <= 0 or not select.select([], [fd], [], max(0, budget))[1]:
            raise InstallError('retained helper transfer timed out')
        try:
            count = os.write(fd, remaining)
        except BlockingIOError:
            continue
        remaining = remaining[count:]


def _work_entry(arguments: list[str]) -> int:
    helper, completion, argv_json = arguments
    completion_fd = int(completion)
    status = 1
    try:
        source = _RETAINED_SOURCES["macos-signing.py"]
        module = types.ModuleType('__main__')
        module.__file__ = helper
        sys.modules['__main__'] = module
        sys.argv = [helper, *json.loads(argv_json)]
        try:
            exec(compile(source, helper, 'exec'), module.__dict__)
            status = 0
        except SystemExit as exc:
            status = exc.code if isinstance(exc.code, int) else (0 if exc.code is None else 1)
    except BaseException:
        import traceback
        traceback.print_exc()
    try:
        sys.stdout.flush()
        sys.stderr.flush()
        os.write(completion_fd, struct.pack('!i', max(0, min(status, 255))))
    except BaseException:
        # The guardian still owns this unreaped anchor and detects a missing frame.
        pass
    finally:
        os.close(completion_fd)
    # Never free the work-group identity between result production and cleanup.
    while True:
        signal.pause()


def _group_present(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
        return True
    except ProcessLookupError:
        return False
    except OSError as exc:
        if exc.errno == errno.EPERM:
            # Permission denial is not proof of absence. Keep the bounded
            # observation active; only ESRCH can establish group absence.
            return True
        raise


def _cleanup_work(process: subprocess.Popen, streams: tuple, budget: float) -> Optional[str]:
    deadline = time.monotonic() + budget
    signal_failure = None
    if process.returncode is None:
        # No poll/wait happens before this signal. The direct child remains an
        # allocated group anchor, including when it exited unexpectedly.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except OSError as exc:
            if exc.errno not in (errno.ESRCH, errno.EPERM):
                raise
            # Darwin returns EPERM for some zombie-only groups. This is not
            # success: require bounded EOF, reap and subsequent group absence.
            signal_failure = exc
        with selectors.DefaultSelector() as selector:
            for stream in streams:
                if not stream.closed:
                    os.set_blocking(stream.fileno(), False)
                    selector.register(stream, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise InstallError(f'work pipes did not close during cleanup; signal error: {signal_failure}')
                for key, _ in selector.select(remaining):
                    if not os.read(key.fd, 65536):
                        selector.unregister(key.fileobj)
        try:
            process.wait(timeout=max(0.001, deadline - time.monotonic()))
        except subprocess.TimeoutExpired as exc:
            raise InstallError(f'work leader did not exit; signal error: {signal_failure}') from exc
    # After reap this is observation only. Never signal a possibly reused PGID.
    while _group_present(process.pid):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise InstallError(f'work group remains present after cleanup; signal error: {signal_failure}')
        time.sleep(min(0.01, remaining))
    return str(signal_failure) if signal_failure is not None else None


def _failure_path(paths: Paths) -> Path:
    return paths.root / f'.{paths.product}.app-cleanup-failure.json'


def _busy_message(paths: Paths) -> str:
    return (f'product install busy: {paths.lock}; inspect {_failure_path(paths)}; '
            f'run cleanup-status {paths.product} --root {paths.root}')


def _store_failure(paths: Paths, value: dict) -> None:
    destination = _failure_path(paths)
    previous = _entry_identity(destination)
    if previous['present']:
        old = _read_json(destination, 'cleanup failure receipt')
        if old.get('product') != paths.product or old.get('root') != str(paths.root) or (
            old.get('token') != value['token'] and old.get('status') != 'clean'
        ):
            raise InstallError('foreign cleanup failure receipt must be preserved')
    temporary = destination.with_name(destination.name + '.' + secrets.token_hex(8))
    _write_private(temporary, (json.dumps(value, sort_keys=True) + '\n').encode())
    with _open_dir(paths.root) as parent_fd:
        if _entry_identity(destination) != previous:
            raise InstallError('cleanup receipt changed before publication')
        if previous['present']:
            os.replace(temporary, destination)
        else:
            _atomic_new(parent_fd, temporary, destination)
        _fsync_dir(parent_fd)


def _unique_control_pairs(items):
    result = {}
    for key, value in items:
        if key in result: raise InstallError('duplicate cleanup control field')
        result[key] = value
    return result


def _receive_control(connection: socket.socket, timeout: float = 2) -> dict:
    deadline = time.monotonic() + timeout
    data = bytearray()
    while b'\n' not in data:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError('cleanup control frame deadline exceeded')
        connection.settimeout(remaining)
        part = connection.recv(min(512, CONTROL_LIMIT + 1 - len(data)))
        if not part:
            raise InstallError('incomplete cleanup request')
        data.extend(part)
        if len(data) > CONTROL_LIMIT:
            raise InstallError('cleanup request too large')
    if data.count(b'\n') != 1 or not data.endswith(b'\n'):
        raise InstallError('invalid cleanup framing')
    value = json.loads(data, object_pairs_hook=_unique_control_pairs)
    if not isinstance(value, dict):
        raise InstallError('invalid cleanup request')
    return value


def _control_directory(paths: Paths) -> Path:
    identity = hashlib.sha256((paths.product + ':' + str(paths.root)).encode()).hexdigest()[:32]
    return Path('/private/tmp') / f'hex-app-cleanup-{os.getuid()}-{identity}'


@contextlib.contextmanager
def _control_endpoint(paths: Paths):
    # Establish discoverability before work. Normal operations remove this
    # transient socket and create no diagnostic receipt or signed-state writes.
    parent = Path('/private/tmp')
    info = parent.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or stat.S_IMODE(info.st_mode) != 0o1777:
        raise InstallError('cleanup socket parent is not the system sticky directory')
    directory = _control_directory(paths)
    directory.mkdir(mode=0o700)  # Existing actor/guardian is preserved, never removed.
    endpoint = directory / 'control.sock'
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        server.bind(str(endpoint)); server.listen(1)
        yield server, directory, secrets.token_hex(16)
    finally:
        server.close()
        if endpoint.exists(): endpoint.unlink()
        if directory.exists(): directory.rmdir()


def _record_failure(paths: Paths, value: dict) -> None:
    try:
        _store_failure(paths, value)
        value.pop('receipt_error', None)
    except Exception as exc:
        # The live endpoint was established before work and is located without
        # this receipt. Storage loss must not unwind the owner or release flock.
        value['receipt_error'] = str(exc)[:250]


def _quarantine(paths: Paths, process: subprocess.Popen, streams: tuple,
                status_fd: int, failure: Exception, budget: float, recovery, lock_fd: int) -> None:
    server, directory, token = recovery
    value = {'schema_version': 1, 'product': paths.product, 'root': str(paths.root),
             'status': 'blocked', 'token': token, 'endpoint': str(directory / 'control.sock'),
             'guardian_pid': os.getpid(), 'work_pid': process.pid,
             'anchor_reaped': process.returncode is not None, 'error': str(failure)[:500]}
    _record_failure(paths, value)
    try:
        os.write(status_fd, b'F')
    except BrokenPipeError:
        pass  # The fixed live endpoint remains discoverable without this caller.
    os.close(status_fd)
    for fd in (1, 2):
        try: os.close(fd)
        except OSError: pass
    while True:
        connection, _ = server.accept()  # Requests, not a recurring scan.
        with connection:
            try:
                request = _receive_control(connection)
                if set(request) != {'token', 'command'}:
                    raise InstallError('invalid cleanup request fields')
                if request['token'] != token and not (request['token'] is None and request['command'] == 'status'):
                    raise InstallError('cleanup endpoint identity mismatch')
                if request['command'] not in ('status', 'retry'):
                    raise InstallError('unsupported cleanup operation')
                if request['command'] == 'retry':
                    try:
                        _cleanup_work(process, streams, budget)
                        value.update(status='clean', error='', anchor_reaped=True)
                    except Exception as exc:
                        value.update(error=str(exc)[:500], anchor_reaped=process.returncode is not None)
                    _record_failure(paths, value)
                if value['status'] == 'clean':
                    # Close only our inherited reference after proven cleanup,
                    # before returning the recovery result. Never LOCK_UN.
                    server.close()
                    (directory / 'control.sock').unlink()
                    directory.rmdir()
                    os.close(lock_fd)
                    try:
                        connection.sendall((json.dumps(value) + '\n').encode())
                    except OSError:
                        pass  # Cleanup is complete even if the client disconnected.
                    return
                connection.sendall((json.dumps(value) + '\n').encode())
            except (OSError, ValueError, InstallError) as exc:
                try: connection.sendall((json.dumps({'error': str(exc)[:500]}) + '\n').encode())
                except OSError: pass


def _guardian_entry(arguments: list[str]) -> int:
    config = json.loads(arguments[0])
    paths = product_paths(config['product'], Path(config['root']))
    _validate_lock_fd(config['lock_fd'], paths)
    with _control_endpoint(paths) as recovery:
        return _guardian_run(config, paths, recovery)


def _guardian_run(config, paths, recovery):
    live, ready, lock_fd = config['live_fd'], config['ready_fd'], config['lock_fd']
    _validate_lock_fd(lock_fd, paths)
    with _open_dir(paths.root):
        if paths.root.stat().st_uid != os.getuid():
            raise InstallError('product root is not owned by this user')
    os.write(ready, b'R')
    remaining = config['deadline'] - time.monotonic()
    if remaining <= 0 or not select.select([live], [], [], remaining)[0] or os.read(live, 1) != b'G':
        return 1
    completed_r, completed_w = os.pipe()
    helper = Path(config['helper'])
    argv, source_r, source_w, payload = _entry_command(
        '_work_entry', [str(helper), str(completed_w), json.dumps(config['argv'])],
        _RETAINED_SOURCES['macos-signing.py'])
    process = None
    try:
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, start_new_session=True,
                                   pass_fds=(completed_w, source_r), env=config['environment'])
        os.close(source_r); source_r = -1
        _send_source(source_w, payload, config['deadline'])
    except BaseException:
        os.close(source_w); source_w = -1
        if process is not None:
            try:
                _cleanup_work(process, (process.stdout, process.stderr), config['cleanup_budget'])
            except Exception as exc:
                _quarantine(paths, process, (process.stdout, process.stderr), ready, exc, config['cleanup_budget'], recovery, lock_fd)
        raise
    finally:
        for descriptor in (source_r, source_w, completed_w):
            if descriptor >= 0: os.close(descriptor)
    streams = (process.stdout, process.stderr)
    buffers = [bytearray(), bytearray()]
    completion = bytearray(); result = None; failure = None
    try:
        with selectors.DefaultSelector() as selector:
            for index, stream in enumerate(streams):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, index)
            selector.register(live, selectors.EVENT_READ, 'live')
            selector.register(completed_r, selectors.EVENT_READ, 'completion')
            while result is None:
                remaining = config['deadline'] - time.monotonic()
                if remaining <= 0: raise InstallError('signer helper timed out')
                for key, _ in selector.select(remaining):
                    if key.data == 'live':
                        os.read(live, 1)
                        raise InstallError('signer caller canceled or exited')
                    if key.data == 'completion':
                        part = os.read(completed_r, 5 - len(completion))
                        if not part: raise InstallError('signer completion frame missing')
                        completion.extend(part)
                        if len(completion) > 4: raise InstallError('signer completion frame too large')
                        if len(completion) == 4: result = struct.unpack('!i', completion)[0]
                    else:
                        buffer = buffers[key.data]
                        part = os.read(key.fd, min(65536, MAX_JSON_BYTES + 1 - len(buffer)))
                        if not part: selector.unregister(key.fileobj)
                        else:
                            buffer.extend(part)
                            if len(buffer) > MAX_JSON_BYTES: raise InstallError('signer helper output is too large')
        # Flush precedes completion. Drain remaining ready bytes before cleanup;
        # the anchor deliberately keeps its output descriptors open.
        for index, stream in enumerate(streams):
            while True:
                try: part = os.read(stream.fileno(), min(65536, MAX_JSON_BYTES + 1 - len(buffers[index])))
                except BlockingIOError: break
                if not part: break
                buffers[index].extend(part)
                if len(buffers[index]) > MAX_JSON_BYTES: raise InstallError('signer helper output is too large')
    except BaseException as exc:
        failure = exc
    finally:
        os.close(completed_r)
    try:
        warning = _cleanup_work(process, streams, config['cleanup_budget'])
        if warning is not None:
            print(f'cleanup signal failed before verified group absence: {warning}', file=sys.stderr)
    except Exception as exc:
        _quarantine(paths, process, streams, ready, exc, config['cleanup_budget'], recovery, lock_fd)
        return 1
    finally:
        for stream in streams: stream.close()
    if failure is not None:
        print(str(failure)[:500], file=sys.stderr)
        return 1
    for fd, buffer in zip((1, 2), buffers):
        remaining = memoryview(buffer)
        while remaining:
            remaining = remaining[os.write(fd, remaining):]
    return result


def _reap_guardian(process: subprocess.Popen) -> None:
    # One blocking wait, not a polling monitor. If the CLI exits first, the OS
    # adopts and eventually reaps the exceptional guardian after recovery.
    process.wait()


def _status_transition(frame: bytes, ready: bool) -> tuple[bool, bool]:
    if not frame or len(frame) > 2:
        raise InstallError('invalid guardian status frame')
    failed = False
    for marker in frame:
        if marker == ord('R') and not ready:
            ready = True
        elif marker == ord('F') and ready and not failed:
            failed = True
        else:
            raise InstallError('invalid guardian status frame')
    return ready, failed


def _run_guardian(signer, argv: list[str]) -> tuple[int, bytes, bytes]:
    if signer._owner is None:
        raise InstallError('signer execution requires a validated product lock')
    paths, lock_fd = signer._owner
    _validate_lock_fd(lock_fd, paths)
    live_r, live_w = os.pipe(); ready_r, ready_w = os.pipe()
    environment = {'HOME': str(Path.home()), 'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'}
    deadline = time.monotonic() + signer.timeout
    config = {'product': paths.product, 'root': str(paths.root), 'helper': str(signer.helper),
              'argv': argv, 'live_fd': live_r, 'ready_fd': ready_w, 'lock_fd': lock_fd,
              'deadline': deadline, 'cleanup_budget': CLEANUP_BUDGET, 'environment': environment}
    process = None; cancellation = None; quarantined = False
    command, source_r, source_w, payload = _entry_command(
        "_guardian_entry", [json.dumps(config)], signer._source)
    try:
        process = subprocess.Popen(command,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=True, pass_fds=(live_r, ready_w, lock_fd, source_r), env=environment)
        os.close(source_r); source_r = -1
        _send_source(source_w, payload, deadline)
        os.close(source_w); source_w = -1
        os.close(live_r); live_r = -1
        os.close(ready_w); ready_w = -1
        buffers = [bytearray(), bytearray()]; ready = False
        with selectors.DefaultSelector() as selector:
            for index, stream in enumerate((process.stdout, process.stderr)):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, index)
            selector.register(ready_r, selectors.EVENT_READ, 'status')
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    if cancellation is not None: raise InstallError('guardian cleanup did not finish; product ownership remains unresolved')
                    cancellation = InstallError('signer helper timed out')
                    os.close(live_w); live_w = -1
                    deadline = time.monotonic() + CLEANUP_BUDGET + 2
                    continue
                try:
                    events = selector.select(remaining)
                except BaseException as exc:
                    if cancellation is not None: raise
                    cancellation = exc
                    os.close(live_w); live_w = -1
                    deadline = time.monotonic() + CLEANUP_BUDGET + 2
                    continue
                for key, _ in events:
                    if key.data == 'status':
                        frame = os.read(ready_r, 2)
                        if not frame: selector.unregister(ready_r)
                        else:
                            previous = ready
                            ready, quarantined = _status_transition(frame, ready)
                            if ready and not previous and live_w >= 0:
                                os.write(live_w, b'G')
                            if quarantined:
                                raise InstallError(f'cleanup incomplete; {_busy_message(paths)}')
                    else:
                        part = os.read(key.fd, min(65536, MAX_JSON_BYTES + 1 - len(buffers[key.data])))
                        if not part: selector.unregister(key.fileobj)
                        else:
                            buffers[key.data].extend(part)
                            if len(buffers[key.data]) > MAX_JSON_BYTES:
                                raise InstallError('guardian output is too large')
        code = process.wait(timeout=max(0.001, deadline - time.monotonic()))
        if cancellation is not None: raise cancellation
        if not ready: raise InstallError('guardian did not establish ownership')
        return code, bytes(buffers[0]), bytes(buffers[1])
    except BaseException:
        if source_w >= 0:
            os.close(source_w); source_w = -1
        if live_w >= 0:
            os.close(live_w); live_w = -1
        if process is not None and process.returncode is None and not quarantined:
            # Cancellation can arrive outside selector.select too. Always request
            # cleanup and give this owner its bounded completion window.
            cancel_deadline = time.monotonic() + CLEANUP_BUDGET + 2
            with selectors.DefaultSelector() as selector:
                for stream in (process.stdout, process.stderr):
                    os.set_blocking(stream.fileno(), False)
                    selector.register(stream, selectors.EVENT_READ)
                while selector.get_map() and time.monotonic() < cancel_deadline:
                    for key, _ in selector.select(max(0, cancel_deadline - time.monotonic())):
                        if not os.read(key.fd, 65536): selector.unregister(key.fileobj)
            try:
                process.wait(timeout=max(0.001, cancel_deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                pass  # The live guardian retains the lock and recovery authority.
        raise
    finally:
        for fd in (live_r, live_w, ready_r, ready_w, source_r, source_w):
            if fd >= 0: os.close(fd)
        if process is not None:
            for stream in (process.stdout, process.stderr): stream.close()
            if process.returncode is None:
                # Never kill this cleanup owner. Closing liveness requests cleanup.
                threading.Thread(target=_reap_guardian, args=(process,), daemon=True).start()


def cleanup_operation(product: str, root: Path, operation: str) -> dict:
    paths = product_paths(product, root)
    directory = _control_directory(paths)
    if not os.path.lexists(directory):
        record = _read_json(_failure_path(paths), 'cleanup failure receipt')
        if record.get('schema_version') != 1 or record.get('product') != product or record.get('root') != str(paths.root):
            raise InstallError('cleanup failure receipt does not match product')
        if record.get('status') == 'clean': return record
        raise InstallError('cleanup guardian unavailable; recorded PIDs are not signaling authority')
    info = directory.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise InstallError('cleanup endpoint directory is not private')
    endpoint = directory / 'control.sock'
    if not stat.S_ISSOCK(endpoint.lstat().st_mode):
        raise InstallError('cleanup endpoint mismatch')

    def request(command, token):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(CLEANUP_BUDGET + 3)
            connection.connect(str(endpoint))
            connection.sendall((json.dumps({'token': token, 'command': command}) + '\n').encode())
            value = _receive_control(connection, CLEANUP_BUDGET + 3)
        if value.get('schema_version') != 1 or value.get('product') != product or value.get('root') != str(paths.root):
            raise InstallError('cleanup guardian response mismatch')
        actual = value.get('token')
        if not isinstance(actual, str) or not re.fullmatch('[0-9a-f]{32}', actual) or (token is not None and actual != token):
            raise InstallError('cleanup guardian token mismatch')
        return value
    status = request('status', None)
    return request('retry', status['token']) if operation == 'retry' else status


def _helper_provenance(helper: Path, root: Path, source_revision: str) -> dict[str, Any]:
    return {name: {"sha256": hashlib.sha256(source).hexdigest(), "source_revision": source_revision}
            for name, source in _RETAINED_SOURCES.items()}


def _emit(value: Mapping[str, Any]) -> int:
    print(json.dumps(dict(value), sort_keys=True, indent=2))
    return 0


def _validate_lock_fd(fd: int, paths: Paths) -> None:
    try:
        actual = os.fstat(fd)
        expected = paths.lock.stat()
        if not stat.S_ISREG(actual.st_mode) or (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino):
            raise InstallError("inherited fd is not the expected product lock")
        probe = os.open(paths.lock, os.O_RDWR | getattr(os, "O_NOFOLLOW", 0))
        try:
            try:
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as exc:
                if exc.errno not in (errno.EACCES, errno.EAGAIN):
                    raise InstallError("cannot validate inherited product lock") from exc
            else:
                fcntl.flock(probe, fcntl.LOCK_UN)
                raise InstallError("inherited fd does not hold the product lock")
        finally:
            os.close(probe)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as exc:
            raise InstallError("inherited fd does not hold the product lock") from exc
    except OSError as exc:
        raise InstallError("inherited lock fd is invalid") from exc


class AliasError(InstallError):
    """A compatibility publication failure with faithful partial results."""
    def __init__(self, message: str, result: dict):
        super().__init__(message, published=result['published'])
        self.result = dict(result)


def _alias_entry(parent_fd: int, name: str) -> dict:
    try:
        info = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    except FileNotFoundError:
        return {'kind': 'missing'}
    result = {'dev': info.st_dev, 'ino': info.st_ino, 'mode': stat.S_IMODE(info.st_mode),
              'size': info.st_size, 'mtime_ns': info.st_mtime_ns}
    if stat.S_ISLNK(info.st_mode):
        result.update(kind='symlink', target=os.readlink(name, dir_fd=parent_fd))
        return result
    if not stat.S_ISREG(info.st_mode):
        raise InstallError('compatibility entry is not a regular file or canonical alias')
    fd = os.open(name, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW, dir_fd=parent_fd)
    try:
        current = os.fstat(fd)
        if (current.st_dev, current.st_ino) != (info.st_dev, info.st_ino):
            raise InstallError('compatibility entry changed during inspection')
        digest = hashlib.sha256()
        remaining = info.st_size
        while remaining:
            chunk = os.read(fd, min(65536, remaining))
            if not chunk: raise InstallError('compatibility entry shrank during inspection')
            digest.update(chunk); remaining -= len(chunk)
        after = os.fstat(fd)
        if os.read(fd, 1) or (after.st_size, after.st_mtime_ns, after.st_ctime_ns) != (info.st_size, info.st_mtime_ns, info.st_ctime_ns):
            raise InstallError('compatibility entry changed during inspection')
        result.update(kind='regular', sha256=digest.hexdigest())
        return result
    finally:
        os.close(fd)


def _alias_archive(parent_fd: int, name: str, previous: dict, archive: Path, result: dict, archive_fd: int) -> None:
    source_fd = os.open(name, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW, dir_fd=parent_fd)
    destination = archive / ('previous-' + name)
    output_fd = None
    try:
        output_fd = os.open(destination.name, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=archive_fd)
        info = os.fstat(source_fd)
        if not stat.S_ISREG(info.st_mode) or (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns) != (
            previous['dev'], previous['ino'], previous['size'], previous['mtime_ns']
        ):
            raise InstallError('compatibility entry changed before archive')
        digest = hashlib.sha256(); remaining = info.st_size
        while remaining:
            chunk = os.read(source_fd, min(65536, remaining))
            if not chunk: raise InstallError('compatibility entry shrank while archiving')
            digest.update(chunk); remaining -= len(chunk)
            view = memoryview(chunk)
            while view:
                count = os.write(output_fd, view)
                if count <= 0: raise InstallError('short compatibility archive write')
                view = view[count:]
        after = os.fstat(source_fd)
        if os.read(source_fd, 1) or digest.hexdigest() != previous['sha256'] or (
            after.st_size, after.st_mtime_ns, after.st_ctime_ns
        ) != (info.st_size, info.st_mtime_ns, info.st_ctime_ns):
            raise InstallError('compatibility entry changed while archiving')
        os.fsync(output_fd)
        os.lseek(output_fd, 0, os.SEEK_SET)
        copied = hashlib.sha256(); remaining = previous['size']
        while remaining:
            chunk = os.read(output_fd, min(65536, remaining))
            if not chunk: raise InstallError('compatibility archive was truncated')
            copied.update(chunk); remaining -= len(chunk)
        if os.read(output_fd, 1) or copied.hexdigest() != previous['sha256']:
            raise InstallError('compatibility archive hash mismatch')
    finally:
        os.close(source_fd)
        if output_fd is not None: os.close(output_fd)
    record = dict(result, archive_status='prepared', previous=previous)
    receipt_fd = os.open('receipt.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=archive_fd)
    try:
        view = memoryview((json.dumps(record, sort_keys=True) + '\n').encode())
        while view:
            count = os.write(receipt_fd, view)
            if count <= 0: raise InstallError('short compatibility receipt write')
            view = view[count:]
        os.fsync(receipt_fd)
    finally:
        os.close(receipt_fd)
    _fsync_dir(archive_fd)


def compatibility_alias(product: str, root: Path, hex_workspace: Path, signer: Signer,
                        *, dry_run: bool = False) -> dict:
    """Verify one fixed code-intel product, then migrate its old Hex command."""
    if product not in ('code-intel-cli', 'code-intel-daemon'):
        raise InstallError('compatibility aliases support only code-intel products')
    expected_root = Path.home().absolute() / '.codeintel'
    if not root.is_absolute() or root != expected_root or _real(root) != root:
        raise InstallError('compatibility product root must be canonical HOME/.codeintel')
    if not hex_workspace.is_absolute() or _real(hex_workspace) != hex_workspace:
        raise InstallError('Hex workspace must be an absolute canonical directory')
    paths = product_paths(product, root)
    name = PRODUCTS[product].executable
    hex_root = hex_workspace / '.hex'
    parent = hex_root / 'bin'
    alias = parent / name
    target = paths.cli
    result = {'schema_version': 1, 'product': product, 'source_revision': None, 'generation': None,
              'alias_path': str(alias), 'target_path': str(target), 'action': 'current',
              'changed': False, 'published': False, 'archive_path': None}
    candidate = None; candidate_identity = None
    try:
        with contextlib.ExitStack() as stack:
            # A read-only negative must not create an absent product root.
            stack.enter_context(_open_dir(root))
            bound = [(path, stack.enter_context(_open_dir(path)))
                     for path in (hex_workspace, hex_root, parent)]
            for path, fd in bound:
                if os.fstat(fd).st_uid != os.getuid():
                    raise InstallError(f'compatibility parent is not owned by this user: {path}')
            parent_fd = bound[-1][1]
            lock_fd = stack.enter_context(_product_lock(paths))
            if isinstance(signer, ProcessSigner): signer.bind_owner(paths, lock_fd)
            owner = service_owner(product, root, signer, lock_held=True)
            result.update(source_revision=owner['source_revision'], generation=owner['generation'])
            previous = _alias_entry(parent_fd, name)
            # Verification can outlive a cooperating installer changing paths.
            # Validate the public parents even when this call will be a no-op.
            for path, fd in bound:
                _revalidate_parent(fd, path)
                if path.is_symlink(): raise InstallError('compatibility parent changed to a symlink')
            if previous['kind'] == 'symlink':
                if previous['target'] != str(target):
                    raise InstallError('foreign compatibility alias is preserved')
                return result
            result['action'] = 'would-migrate' if previous['kind'] == 'regular' else 'would-create'
            if dry_run: return result
            if previous['kind'] == 'regular':
                archive_parent = hex_root / '.code-intel-compat-backups'
                try: os.mkdir(archive_parent.name, mode=0o700, dir_fd=bound[1][1])
                except FileExistsError: pass
                archive_parent_fd = stack.enter_context(_open_dir(archive_parent))
                if os.fstat(archive_parent_fd).st_uid != os.getuid():
                    raise InstallError('compatibility archive parent is not owned by this user')
                archive = archive_parent / (product + '-' + secrets.token_hex(12))
                os.mkdir(archive.name, mode=0o700, dir_fd=archive_parent_fd)
                result['archive_path'] = str(archive)
                archive_fd = stack.enter_context(_open_dir(archive))
                bound.extend(((archive_parent, archive_parent_fd), (archive, archive_fd)))
                _alias_archive(parent_fd, name, previous, archive, result, archive_fd)
                backup_identity = _alias_entry(archive_fd, 'previous-' + name)
                if backup_identity.get('kind') != 'regular' or backup_identity.get('sha256') != previous['sha256']:
                    raise InstallError('prepared compatibility archive does not match old bytes')
                _revalidate_parent(archive_parent_fd, archive_parent)
                _fsync_dir(archive_parent_fd)
                _fsync_dir(bound[1][1])
            candidate = parent / ('.' + name + '.compat-candidate-' + secrets.token_hex(12))
            os.symlink(str(target), candidate.name, dir_fd=parent_fd)
            candidate_identity = _alias_entry(parent_fd, candidate.name)
            try:
                _fsync_dir(parent_fd)
                for path, fd in bound:
                    _revalidate_parent(fd, path)
                    if path.is_symlink(): raise InstallError('compatibility parent changed to a symlink')
                if _alias_entry(parent_fd, name) != previous:
                    raise InstallError('compatibility entry changed before publication')
                if previous['kind'] == 'regular' and _alias_entry(archive_fd, 'previous-' + name) != backup_identity:
                    raise InstallError('compatibility archive changed before publication')
                if previous['kind'] == 'missing':
                    _libc_renameatx(parent_fd, candidate.name, parent_fd, name, RENAME_EXCL)
                else:
                    _libc_renameatx(parent_fd, candidate.name, parent_fd, name, 0)
                result.update(action='migrated' if previous['kind'] == 'regular' else 'created',
                              changed=True, published=True)
                _fsync_dir(parent_fd)
            finally:
                current = _alias_entry(parent_fd, candidate.name)
                if current == candidate_identity:
                    os.unlink(candidate.name, dir_fd=parent_fd)
                elif current['kind'] != 'missing':
                    raise InstallError(f'changed compatibility candidate preserved: {candidate}')
            return result
    except (OSError, InstallError) as exc:
        raise AliasError(str(exc), result) from exc


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    def common(name: str):
        command = commands.add_parser(name)
        command.add_argument("product", choices=sorted(PRODUCTS))
        command.add_argument("--root", required=True, type=Path)
        command.add_argument("--policy", type=Path)
        command.add_argument("--lock-fd", type=int)
        return command

    common("mode")
    common("preflight")
    common("verify-current")
    common("service-owner")
    service_parser = common("service-reconcile")
    service_parser.add_argument("--dry-run", action="store_true")
    common("abandon-staging")
    common("cleanup-status")
    common("cleanup-retry")
    alias_parser = commands.add_parser("compatibility-alias")
    alias_parser.add_argument("product", choices=("code-intel-cli", "code-intel-daemon"))
    alias_parser.add_argument("--root", required=True, type=Path)
    alias_parser.add_argument("--hex-workspace", required=True, type=Path)
    alias_parser.add_argument("--dry-run", action="store_true")
    install_parser = commands.add_parser("install")
    install_parser.add_argument("product", choices=sorted(PRODUCTS))
    install_parser.add_argument("--root", required=True, type=Path)
    install_parser.add_argument("--source", required=True, type=Path)
    install_parser.add_argument("--version", required=True)
    install_parser.add_argument("--source-revision", required=True)
    install_parser.add_argument("--helper-source-revision", required=True)
    install_parser.add_argument("--policy", type=Path)
    args = parser.parse_args(argv)
    policy = central_policy_path()
    try:
        if args.command == "compatibility-alias":
            return _emit(compatibility_alias(args.product, args.root, args.hex_workspace,
                                             ProcessSigner(), dry_run=args.dry_run))
        if args.policy is not None and args.policy.absolute() != policy.absolute():
            raise InstallError("CLI signing policy must use the central machine path")
        if args.command == "abandon-staging":
            if args.lock_fd is not None:
                raise StagingRecoveryError("abandon-staging acquires its own product lock")
            return _emit(abandon_staging(args.product, args.root))
        if args.command in ("cleanup-status", "cleanup-retry"):
            result = cleanup_operation(args.product, args.root, args.command.removeprefix("cleanup-"))
            _emit(result)
            return 0 if result.get("status") == "clean" or args.command == "cleanup-status" else 1
        signer = ProcessSigner()
        if args.command == "mode":
            paths = product_paths(args.product, args.root)
            if paths.root.exists():
                with _product_lock(paths) as lock_fd:
                    signer.bind_owner(paths, lock_fd)
                    mode = detect_mode(args.product, args.root, policy, signer)
            else:
                mode = "empty"
            return _emit({"schema_version": STATE_SCHEMA_VERSION, "product": args.product, "mode": mode, "policy_available": _policy_mode(policy) == "configured", "managed": _policy_mode(policy) == "configured" or paths.state.exists(), "bundle_path": str(paths.app.absolute()), "executable_path": str(paths.executable.absolute()), "compatibility_path": str(paths.cli.absolute()), "helper_path": str(paths.helper_dir.absolute()), "state_path": str(paths.state.absolute()), "lock_path": str(paths.lock.absolute()), "journal_path": str(paths.journal.absolute())})
        if args.command == "preflight":
            paths = product_paths(args.product, args.root)
            if args.lock_fd is not None:
                _validate_lock_fd(args.lock_fd, paths)
                signer.bind_owner(paths, args.lock_fd)
                result = preflight(args.product, args.root, signer, policy_path=policy, lock_held=True)
            else:
                with _product_lock(paths) as lock_fd:
                    signer.bind_owner(paths, lock_fd)
                    result = preflight(args.product, args.root, signer, policy_path=policy, lock_held=True)
            return _emit(result)
        if args.command in {"verify-current", "service-owner"}:
            paths = product_paths(args.product, args.root)
            if args.lock_fd is not None:
                _validate_lock_fd(args.lock_fd, paths)
                signer.bind_owner(paths, args.lock_fd)
                result = service_owner(args.product, args.root, signer, policy_path=policy, lock_held=True)
            elif args.command == "verify-current":
                with _product_lock(paths) as lock_fd:
                    signer.bind_owner(paths, lock_fd)
                    result = service_owner(args.product, args.root, signer, policy_path=policy, lock_held=True)
            else:
                raise InstallError("service-owner requires --lock-fd")
            return _emit(result)
        if args.command == "service-reconcile":
            if args.lock_fd is not None:
                raise InstallError("service-reconcile does not accept an inherited lock")
            result = service_reconcile(args.product, args.root, signer, policy_path=policy, dry_run=args.dry_run)
            return _emit(result)
        helper = Path(__file__).with_name("macos-signing.py")
        state = install(args.product, args.root, args.source, signer, policy_path=policy, helper_provenance=_helper_provenance(helper, args.root, args.helper_source_revision), helper_sources={"macos-signing.py": helper, "macos-app-install.py": Path(__file__).absolute()}, source_revision=args.source_revision, version=args.version)
        return _emit(state)
    except StagingRecoveryError as exc:
        print(json.dumps({"schema_version": 1, "error": str(exc), "published": None,
                          "archive_path": exc.archive, "action": "abandonment-incomplete"}), file=sys.stderr)
        return 1
    except AliasError as exc:
        print(json.dumps(dict(exc.result, error=str(exc))), file=sys.stderr)
        return 1
    except InstallError as exc:
        print(json.dumps({"schema_version": STATE_SCHEMA_VERSION, "error": str(exc), "published": bool(exc.published) if exc.published is not None else False}), file=sys.stderr)
        return 1


# Rust service bootstraps supply both already verified byte strings. Internal
# hops propagate that exact binding. A standalone source CLI pins siblings once
# at initial entry, never re-accepting later on-disk replacements.
_PINNED_INSTALLER_PATH = Path(__file__).absolute()
_PINNED_SIGNER_PATH = _PINNED_INSTALLER_PATH.with_name('macos-signing.py')
if 'contents' in globals():
    _RETAINED_SOURCES = dict(contents)
    if set(_RETAINED_SOURCES) != {'macos-app-install.py', 'macos-signing.py'} or any(
        not isinstance(value, bytes) or not value or len(value) > MAX_HELPER_BYTES
        for value in _RETAINED_SOURCES.values()
    ):
        raise InstallError('invalid retained helper source binding')
else:
    _RETAINED_SOURCES = {name: _read_helper_source(_PINNED_INSTALLER_PATH.with_name(name))
                         for name in ('macos-app-install.py', 'macos-signing.py')}


if __name__ == "__main__":
    raise SystemExit(main())
