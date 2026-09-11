#!/usr/bin/env python3
"""Stage a verified macOS candidate without touching a live installation."""
from __future__ import annotations

import argparse
import contextlib
import ctypes
import dataclasses
import hashlib
import json
import os
import plistlib
import re
import shutil
import stat
import subprocess
import tempfile
import uuid
from pathlib import Path
from typing import Callable, Optional, Sequence
from xml.parsers.expat import ExpatError

SCHEMA_VERSION = 1
COMMAND_TIMEOUT = 30.0
SECURITY = "/usr/bin/security"
CODESIGN = "/usr/bin/codesign"
DWARFDUMP = "/usr/bin/dwarfdump"
PRODUCTS = {
    "hex": {"identifier": "com.mrap.hex", "executable": "hex", "bundle": "Hex.app", "name": "Hex"},
    "boi": {"identifier": "com.mrap.boi", "executable": "boi", "bundle": "BOI.app", "name": "BOI"},
    "code-intel-daemon": {"identifier": "com.mrap.hex.scipd", "executable": "scipd", "bundle": "SCIPD.app", "name": "SCIPD"},
    "code-intel-cli": {"identifier": "com.mrap.hex.cq", "executable": "cq", "bundle": "CQ.app", "name": "CQ"},
}
POLICY_KEYS = {"schema_version", "certificate_sha1", "team_id", "keychain"}
FINGERPRINT_RE = re.compile(r"^[0-9A-Fa-f]{40}$")
TEAM_RE = re.compile(r"^[A-Za-z0-9]{10}$")
# A deliberately narrow, valid CFBundleVersion and short-version form. Zero is
# valid as the complete major component, but leading zeroes remain rejected.
VERSION_RE = re.compile(r"^(?:0|[1-9][0-9]{0,3})\.(?:0|[1-9][0-9]?)\.(?:0|[1-9][0-9]?)$")
Run = Callable[[Sequence[str], float], "CommandResult"]


@dataclasses.dataclass(frozen=True)
class CommandResult:
    returncode: int
    stdout: str = ""
    stderr: str = ""


class SigningError(RuntimeError):
    def __init__(self, message: str, *, published: bool = False, receipt: Optional[dict] = None):
        super().__init__(message)
        self.published = published
        self.receipt = receipt


def run_command(argv: Sequence[str], timeout: float) -> CommandResult:
    try:
        result = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, check=False)
    except (subprocess.TimeoutExpired, OSError) as exc:
        raise SigningError(f"command unavailable or timed out: {argv[0]}") from exc
    return CommandResult(result.returncode, result.stdout, result.stderr)


def _checked(run: Run, argv: Sequence[str], timeout: float) -> CommandResult:
    result = run(argv, timeout)
    if result.returncode:
        raise SigningError(f"command failed ({result.returncode}): {argv[0]}: {(result.stderr or result.stdout).strip()[-500:]}")
    return result


def _unique_json_pairs(pairs: list) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise SigningError(f"duplicate signing policy key: {key}")
        result[key] = value
    return result


def _read_policy(path: Path) -> dict:
    try:
        policy = json.loads(path.read_text(), object_pairs_hook=_unique_json_pairs)
    except (OSError, ValueError) as exc:
        raise SigningError(f"invalid signing policy: {path}") from exc
    if not isinstance(policy, dict) or type(policy.get("schema_version")) is not int or policy["schema_version"] != SCHEMA_VERSION:
        raise SigningError("signing policy schema_version must be integer 1")
    if set(policy) - POLICY_KEYS:
        raise SigningError("unknown signing policy keys")
    if not isinstance(policy.get("certificate_sha1"), str) or not FINGERPRINT_RE.fullmatch(policy["certificate_sha1"]):
        raise SigningError("certificate_sha1 must be 40 hexadecimal characters")
    if not isinstance(policy.get("team_id"), str) or not TEAM_RE.fullmatch(policy["team_id"]):
        raise SigningError("team_id must be ten alphanumeric characters")
    if "keychain" in policy:
        keychain = policy["keychain"]
        if not isinstance(keychain, str) or not keychain.strip() or not Path(keychain).is_absolute():
            raise SigningError("keychain must be an explicit absolute path")
    return policy


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise SigningError("hash input is not a regular file")
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _uuids(text: str) -> dict:
    result = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        match = re.fullmatch(r"UUID:\s+([0-9A-Fa-f]{8}(?:-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12})\s+\(([A-Za-z0-9_]+)\)(?:\s+.*)?", line.strip())
        if not match:
            raise SigningError("malformed Mach-O UUID output")
        value, architecture = match.groups()
        if uuid.UUID(value).int == 0 or architecture in result:
            raise SigningError("zero UUID or duplicate Mach-O architecture")
        result[architecture] = value.upper()
    if not result:
        raise SigningError("executable has no public Mach-O UUID")
    return result


def _check_source_security(source: Path, architectures: dict, run: Run, timeout: float) -> None:
    """Only unentitled code without behavior-changing signature flags is supported."""
    for architecture in architectures:
        result = run([CODESIGN, "-d", "--verbose=4", "--architecture", architecture, str(source)], timeout)
        if result.returncode:
            if result.returncode == 1 and not result.stdout and result.stderr.strip() == f"{source}: code object is not signed at all":
                continue
            raise SigningError(f"source signature inspection failed ({result.returncode}) for {architecture}")
        matches = re.findall(r"^CodeDirectory\b[^\n]*\bflags=0x([0-9a-fA-F]+)\b", result.stdout + "\n" + result.stderr, re.MULTILINE)
        # CS_ADHOC / CS_LINKER_SIGNED describe the replaceable build signature,
        # not entitlements or runtime restrictions. All other flags are refused.
        if len(matches) != 1 or int(matches[0], 16) & ~0x20002:
            raise SigningError("source has unsupported or missing code-signing flags")
        entitlements = _checked(run, [CODESIGN, "-d", "--entitlements", "-", "--architecture", architecture, str(source)], timeout)
        if entitlements.stdout.strip():
            try:
                value = plistlib.loads(entitlements.stdout.encode("utf-8"))
            except (ValueError, plistlib.InvalidFileException, ExpatError) as exc:
                raise SigningError("source entitlements are not a valid plist") from exc
            if not isinstance(value, dict) or value:
                raise SigningError("source entitlements need an approved preservation policy")


def _identity(policy: dict, run: Run, timeout: float) -> str:
    argv = [SECURITY, "find-identity", "-v", "-p", "codesigning"]
    if policy.get("keychain"):
        argv.append(policy["keychain"])
    result = _checked(run, argv, timeout)
    wanted = policy["certificate_sha1"].upper()
    for line in result.stdout.splitlines():
        match = re.search(r'\b([0-9A-Fa-f]{40})\b\s+"[^"\n]+"', line)
        if match and match.group(1).upper() == wanted:
            return wanted
    raise SigningError("configured certificate fingerprint is unavailable")


def _metadata(text: str, key: str) -> str:
    values = re.findall(rf"^{re.escape(key)}=(.+)$", text, re.MULTILINE)
    if len(values) != 1:
        raise SigningError(f"missing or ambiguous signed {key}")
    return values[0].strip()


def _verify(bundle: Path, executable: Path, product: dict, policy: dict, identity: str, temp: Path, architectures: dict, run: Run, timeout: float) -> dict:
    requirement = f'anchor apple generic and certificate leaf[subject.OU] = "{policy["team_id"]}" and identifier "{product["identifier"]}"'
    _checked(run, [CODESIGN, "--verify", "--deep", "--strict", "-R", "=" + requirement, str(bundle)], timeout)
    requirements = {}
    for architecture in architectures:
        selection = ["--architecture", architecture, str(bundle)]
        dr = _checked(run, [CODESIGN, "-d", "-r-", *selection], timeout)
        lines = [line.removeprefix("designated => ").strip() for line in (dr.stdout + "\n" + dr.stderr).splitlines() if line.startswith("designated => ")]
        if len(lines) != 1 or not lines[0]:
            raise SigningError("missing or ambiguous designated requirement")
        requirements[architecture] = lines[0]
        metadata = _checked(run, [CODESIGN, "-d", "--verbose=4", *selection], timeout)
        text = metadata.stdout + "\n" + metadata.stderr
        if _metadata(text, "TeamIdentifier") != policy["team_id"] or _metadata(text, "Identifier") != product["identifier"]:
            raise SigningError("signed TeamIdentifier or identifier mismatch")
        prefix = temp / ("certificate-" + architecture + "-")
        _checked(run, [CODESIGN, "-d", "--extract-certificates=" + str(prefix), *selection], timeout)
        # codesign appends the numeric chain position, with no .cer extension.
        leaf = Path(str(prefix) + "0")
        if leaf.is_symlink() or not leaf.is_file() or not leaf.stat().st_size:
            raise SigningError("codesign produced no public leaf certificate")
        if hashlib.sha1(leaf.read_bytes()).hexdigest().upper() != identity:
            raise SigningError("signed leaf certificate does not match configured identity")
    signed_uuids = _uuids(_checked(run, [DWARFDUMP, "--uuid", str(executable)], timeout).stdout)
    if signed_uuids != architectures:
        raise SigningError("executable UUIDs or architecture set changed during verification")
    return {"team_id": policy["team_id"], "designated_requirements": requirements, "mach_o_uuids": signed_uuids, "certificate_sha1": identity}


def _atomic_publish(source: Path, destination: Path, parent_fd: int) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    if not hasattr(libc, "renameatx_np"):
        raise SigningError("atomic no-clobber publication requires macOS renameatx_np")
    renameatx = libc.renameatx_np
    renameatx.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    renameatx.restype = ctypes.c_int
    # RENAME_EXCL, with an already-open destination parent. No public cleanup.
    if renameatx(-2, os.fsencode(source), parent_fd, os.fsencode(destination.name), 0x4) != 0:
        raise SigningError(f"publication failed: {os.strerror(ctypes.get_errno())}")


def _overlaps(path: Path, other: Path) -> bool:
    return path == other or other in path.parents


def _destination(path: Path) -> Path:
    if os.path.lexists(path):
        raise SigningError(f"destination already exists: {path}")
    return path.absolute().parent.resolve() / path.name


def _parent_fd(path: Path, stack: contextlib.ExitStack) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    stack.callback(os.close, fd)
    return fd


def _check_parent(path: Path, fd: int) -> None:
    current = path.parent.lstat()
    original = os.fstat(fd)
    if not stat.S_ISDIR(current.st_mode) or (current.st_dev, current.st_ino) != (original.st_dev, original.st_ino):
        raise SigningError("destination parent changed during staging")


def _prepare_receipt(path: Path, receipt: dict) -> None:
    with path.open("x", encoding="utf-8") as stream:
        os.chmod(path, 0o600)
        json.dump(receipt, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def stage(source: Path, product_name: str, policy_path: Path, output: Path, version: str, receipt_path: Optional[Path] = None, run: Run = run_command, timeout: float = COMMAND_TIMEOUT) -> dict:
    published = False
    receipt = None
    try:
        product = PRODUCTS.get(product_name)
        if product is None:
            raise SigningError(f"unknown product: {product_name}")
        if not VERSION_RE.fullmatch(version):
            raise SigningError("version must be numeric major.minor.patch (major 0..9999, minor/patch 0..99), without prerelease suffix")
        if not 0 < timeout <= COMMAND_TIMEOUT:
            raise SigningError("command timeout must be positive and bounded")
        source = source.resolve(strict=True)
        policy_path = policy_path.resolve(strict=True)
        policy = _read_policy(policy_path)
        output = _destination(output)
        if receipt_path is not None:
            receipt_path = _destination(receipt_path)
            if receipt_path in (source, policy_path) or _overlaps(receipt_path, output) or _overlaps(output, receipt_path):
                raise SigningError("receipt path overlaps a protected input or candidate")
        with contextlib.ExitStack() as stack:
            output_fd = _parent_fd(output, stack)
            receipt_fd = _parent_fd(receipt_path, stack) if receipt_path is not None else None
            temp = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix=".hex-sign-", dir=output.parent)))
            bundle = temp / product["bundle"]
            executable = bundle / "Contents" / "MacOS" / product["executable"]
            executable.parent.mkdir(parents=True)
            # Pin the source inode while copying. Inspect the copied bytes,
            # then verify the original preimage again before any publication.
            fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(fd, "rb") as stream:
                before = os.fstat(stream.fileno())
                if not stat.S_ISREG(before.st_mode) or before.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX):
                    raise SigningError("source must be regular without special mode bits")
                with executable.open("xb") as copied:
                    shutil.copyfileobj(stream, copied)
                after = os.fstat(stream.fileno())
                if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_size, after.st_mtime_ns, after.st_ctime_ns):
                    raise SigningError("source changed while copying")
            executable.chmod(0o755)
            source_hash = _sha256(executable)
            architectures = _uuids(_checked(run, [DWARFDUMP, "--uuid", str(executable)], timeout).stdout)
            _check_source_security(executable, architectures, run, timeout)
            identity = _identity(policy, run, timeout)
            name = product["name"]
            info = {
                "CFBundleDevelopmentRegion": "en", "CFBundleExecutable": product["executable"],
                "CFBundleIdentifier": product["identifier"], "CFBundleInfoDictionaryVersion": "6.0",
                "CFBundleName": name, "CFBundlePackageType": "APPL",
                "CFBundleShortVersionString": version, "CFBundleVersion": version,
                "NSDesktopFolderUsageDescription": f"{name} needs Desktop access to work with files you choose.",
                "NSDownloadsFolderUsageDescription": f"{name} needs Downloads access to work with files you choose.",
                "NSLocalNetworkUsageDescription": f"{name} uses the local network only for configured local services.",
            }
            with (bundle / "Contents" / "Info.plist").open("wb") as stream:
                plistlib.dump(info, stream, sort_keys=True)
            sign = [CODESIGN, "--force", "--sign", identity, "--timestamp"]
            if policy.get("keychain"):
                sign.extend(["--keychain", policy["keychain"]])
            _checked(run, [*sign, str(bundle)], timeout)
            verification = _verify(bundle, executable, product, policy, identity, temp, architectures, run, timeout)
            current = source.stat()
            if (current.st_dev, current.st_ino, current.st_mode) != (before.st_dev, before.st_ino, before.st_mode) or _sha256(source) != source_hash:
                raise SigningError("source changed during signing")
            receipt = {
                "schema_version": 1, "published": True, "product": product_name,
                "identifier": product["identifier"], "version": version,
                "source": str(source), "source_sha256": source_hash,
                "source_mode": stat.S_IMODE(before.st_mode), "candidate_mode": 0o755,
                "candidate": str(output), "candidate_executable_sha256": _sha256(executable),
                **verification,
            }
            staged_receipt = None
            if receipt_path is not None:
                receipt_temp = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix=".hex-sign-receipt-", dir=receipt_path.parent)))
                staged_receipt = receipt_temp / "receipt.json"
                _prepare_receipt(staged_receipt, receipt)
                _check_parent(receipt_path, receipt_fd)
            _check_parent(output, output_fd)
            _atomic_publish(bundle, output, output_fd)
            published = True
            if staged_receipt is not None:
                _check_parent(receipt_path, receipt_fd)
                _atomic_publish(staged_receipt, receipt_path, receipt_fd)
        return receipt
    except (SigningError, OSError, ValueError) as exc:
        # Only private staging directories are cleaned. The public path may
        # have been replaced by someone else; never delete it on failure.
        raise SigningError(str(exc), published=published, receipt=receipt if published else None) from exc


def _installed_state(bundle: Path, product: dict) -> tuple:
    executable = bundle / "Contents" / "MacOS" / product["executable"]
    info_path = bundle / "Contents" / "Info.plist"
    state = {}
    for path, directory in [(bundle, True), (bundle / "Contents", True),
                            (executable.parent, True), (info_path, False), (executable, False)]:
        item = path.lstat()
        if not (stat.S_ISDIR(item.st_mode) if directory else stat.S_ISREG(item.st_mode)):
            raise SigningError("bundle components must be real directories and regular files, not symlinks")
        if item.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX):
            raise SigningError("bundle component has special mode bits")
        if path == executable and not item.st_mode & stat.S_IXUSR:
            raise SigningError("bundle executable is not executable by its owner")
        state[str(path.relative_to(bundle))] = (item.st_dev, item.st_ino, item.st_mode,
                                               item.st_size, item.st_mtime_ns, item.st_ctime_ns)
    fd = os.open(info_path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise SigningError("Info.plist is not regular")
        raw = stream.read(65537)
    if len(raw) > 65536:
        raise SigningError("Info.plist exceeds the supported staging profile size")
    try:
        info = plistlib.loads(raw)
    except (ValueError, plistlib.InvalidFileException, ExpatError) as exc:
        raise SigningError("Info.plist is malformed") from exc
    if not isinstance(info, dict):
        raise SigningError("Info.plist must be a dictionary")
    version = info.get("CFBundleVersion")
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version):
        raise SigningError("installed bundle has an unsupported version")
    expected = {"CFBundleIdentifier": product["identifier"],
                "CFBundleExecutable": product["executable"], "CFBundlePackageType": "APPL",
                "CFBundleName": product["name"], "CFBundleShortVersionString": version}
    if any(info.get(key) != value for key, value in expected.items()):
        raise SigningError("installed plist does not match the fixed product/executable/version")
    for key in ["NSDesktopFolderUsageDescription", "NSDownloadsFolderUsageDescription", "NSLocalNetworkUsageDescription"]:
        value = info.get(key)
        if not isinstance(value, str) or not value.startswith(product["name"] + " ") or len(value.strip()) <= len(product["name"]):
            raise SigningError("installed plist has missing or wrong-product usage text")
    state["info_sha256"] = hashlib.sha256(raw).hexdigest()
    state["executable_sha256"] = _sha256(executable)
    return state, version, executable


def verify_installed(bundle: Path, product_name: str, policy_path: Path,
                     run: Run = run_command, timeout: float = COMMAND_TIMEOUT) -> dict:
    """Verify an existing app without finding a signing identity or changing it."""
    try:
        product = PRODUCTS.get(product_name)
        if product is None:
            raise SigningError(f"unknown product: {product_name}")
        if not 0 < timeout <= COMMAND_TIMEOUT:
            raise SigningError("command timeout must be positive and bounded")
        policy = _read_policy(policy_path)
        bundle = bundle.resolve(strict=True)
        before, version, executable = _installed_state(bundle, product)
        # tempfile.gettempdir() probes candidates with writes, including TMPDIR.
        # Use an existing macOS parent without probing any caller-selected path.
        temporary_parent = Path("/private/tmp").resolve(strict=True)
        if _overlaps(temporary_parent, bundle):
            raise SigningError("verification temporary directory must be outside the bundle")
        architectures = _uuids(_checked(run, [DWARFDUMP, "--uuid", str(executable)], timeout).stdout)
        with tempfile.TemporaryDirectory(prefix=".hex-verify-", dir=temporary_parent) as temporary:
            verification = _verify(bundle, executable, product, policy,
                                   policy["certificate_sha1"].upper(), Path(temporary),
                                   architectures, run, timeout)
        after, _, _ = _installed_state(bundle, product)
        if before != after:
            raise SigningError("bundle changed during verification")
        return {"schema_version": 1, "verified": True, "product": product_name,
                "identifier": product["identifier"], "bundle": str(bundle),
                "executable": str(executable), "version": version,
                "executable_sha256": before["executable_sha256"],
                "info_plist_sha256": before["info_sha256"], **verification}
    except (SigningError, OSError, ValueError) as exc:
        raise SigningError(str(exc)) from exc


def _verify_main(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(description="Read-only installed bundle verification; no signing key needed.")
    parser.add_argument("bundle", type=Path)
    parser.add_argument("product", choices=sorted(PRODUCTS))
    parser.add_argument("policy", type=Path)
    args = parser.parse_args(argv)
    result = None
    try:
        result = verify_installed(args.bundle, args.product, args.policy)
        print(json.dumps(result, indent=2), flush=True)
        return 0
    except (SigningError, OSError) as exc:
        print(json.dumps({"error": str(exc), "verification_completed": result is not None}),
              file=os.sys.stderr, flush=True)
        return 1


def main(argv: Optional[Sequence[str]] = None) -> int:
    arguments = list(os.sys.argv[1:] if argv is None else argv)
    # --version is required by every valid old staging invocation, including
    # a relative source file literally named "verify-installed".
    if arguments and arguments[0] == "verify-installed" and not any(
        value == "--version" or value.startswith("--version=") for value in arguments
    ):
        return _verify_main(arguments[1:])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("product", choices=sorted(PRODUCTS))
    parser.add_argument("policy", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--receipt", type=Path)
    args = parser.parse_args(arguments)
    receipt = None
    try:
        receipt = stage(args.source, args.product, args.policy, args.output, args.version, args.receipt)
        if not args.receipt:
            print(json.dumps(receipt, indent=2), flush=True)
        return 0
    except (SigningError, OSError) as exc:
        published = exc.published if isinstance(exc, SigningError) else receipt is not None
        details = exc.receipt if isinstance(exc, SigningError) else receipt
        print(json.dumps({"error": str(exc), "published": published, "receipt": details}), file=os.sys.stderr, flush=True)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
