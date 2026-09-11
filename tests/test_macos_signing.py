#!/usr/bin/env python3
"""Credential-free orchestration tests, not certificate or privacy qualification."""
import hashlib
import importlib.util
import io
import json
import os
import plistlib
import re
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SOURCE = Path(__file__).parents[1] / "system/scripts/macos-signing.py"
spec = importlib.util.spec_from_file_location("macos_signing", SOURCE)
SIGN = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = SIGN
spec.loader.exec_module(SIGN)
LEAF = b"public DER bytes are mocked only at the codesign command boundary"
FINGERPRINT = hashlib.sha1(LEAF).hexdigest().upper()
TEAM = "TEAM123456"
UUIDS = {"arm64": "12345678-1234-1234-1234-1234567890AB", "x86_64": "87654321-4321-4321-4321-BA0987654321"}


def hex_cargo_version():
    manifest = Path(__file__).parents[1] / "system/harness/Cargo.toml"
    match = re.search(r'^version = "([^"]+)"$', manifest.read_text(encoding="utf-8"), re.MULTILINE)
    if match is None:
        raise AssertionError(f"package version missing from {manifest}")
    return match.group(1)


class Runner:
    """Reject every unknown invocation. Never simulate cryptographic validity."""
    def __init__(self, product="hex", keychain=None):
        self.product = product
        self.keychain = keychain
        self.calls = []
        self.fail_at = None
        self.on_sign = None
        self.signed = False
        self.source_flags = {}
        self.entitlements = {}
        self.uuid_text = None
        self.signed_uuid_text = None
        self.leaf = LEAF
        self.team = TEAM
        self.identifier = "com.mrap." + product
        self.dr = True
        self.available_identity = FINGERPRINT
        self.unsigned = False

    def __call__(self, argv, timeout):
        argv = list(argv)
        self.calls.append(argv)
        assert 0 < timeout <= SIGN.COMMAND_TIMEOUT
        assert argv[0] in (SIGN.SECURITY, SIGN.CODESIGN, SIGN.DWARFDUMP)
        if self.fail_at == len(self.calls) - 1:
            return SIGN.CommandResult(37, "", "injected command rejection")
        if argv[:2] == [SIGN.DWARFDUMP, "--uuid"]:
            assert len(argv) == 3 and Path(argv[2]).is_file()
            text = self.signed_uuid_text if self.signed and self.signed_uuid_text is not None else self.uuid_text
            if text is None:
                text = "".join(f"UUID: {value} ({arch}) {argv[2]}\n" for arch, value in UUIDS.items())
            return SIGN.CommandResult(0, text)
        if argv[:5] == [SIGN.SECURITY, "find-identity", "-v", "-p", "codesigning"]:
            assert argv[5:] == ([self.keychain] if self.keychain else [])
            return SIGN.CommandResult(0, f'1) {self.available_identity} "Mock identity"\n')
        if argv[:2] == [SIGN.CODESIGN, "--force"]:
            expected = [SIGN.CODESIGN, "--force", "--sign", FINGERPRINT, "--timestamp"]
            if self.keychain:
                expected += ["--keychain", self.keychain]
            assert argv[:-1] == expected
            assert Path(argv[-1]).is_dir()
            self.signed = True
            if self.on_sign:
                self.on_sign()
            return SIGN.CommandResult(0)
        if argv[:2] == [SIGN.CODESIGN, "--verify"]:
            requirement = f'=anchor apple generic and certificate leaf[subject.OU] = "{TEAM}" and identifier "com.mrap.{self.product}"'
            assert argv[:-1] == [SIGN.CODESIGN, "--verify", "--deep", "--strict", "-R", requirement]
            assert self.signed and Path(argv[-1]).is_dir()
            return SIGN.CommandResult(0)
        # All display operations explicitly select one architecture.
        assert argv[-3] == "--architecture" and argv[-2] in UUIDS, argv
        arch = argv[-2]
        target = Path(argv[-1])
        if argv[:3] == [SIGN.CODESIGN, "-d", "--verbose=4"]:
            assert len(argv) == 6
            if target.is_file():
                if self.unsigned:
                    return SIGN.CommandResult(1, "", f"{target}: code object is not signed at all\n")
                return SIGN.CommandResult(0, "", self.source_flags.get(arch, "CodeDirectory v=20400 flags=0x20002(adhoc,linker-signed)\n"))
            assert target.is_dir() and self.signed
            return SIGN.CommandResult(0, "", f"TeamIdentifier={self.team}\nIdentifier={self.identifier}\n")
        if argv[:4] == [SIGN.CODESIGN, "-d", "--entitlements", "-"]:
            assert len(argv) == 7 and target.is_file() and not self.signed
            return SIGN.CommandResult(0, self.entitlements.get(arch, ""), f"Executable={target}\n")
        if argv[:3] == [SIGN.CODESIGN, "-d", "-r-"]:
            assert len(argv) == 6 and target.is_dir() and self.signed
            return SIGN.CommandResult(0, f'designated => identifier "{self.identifier}" and anchor apple generic\n' if self.dr else "")
        if len(argv) == 6 and argv[:2] == [SIGN.CODESIGN, "-d"] and argv[2].startswith("--extract-certificates="):
            assert target.is_dir() and self.signed
            # Real codesign uses prefix0, not prefix0.cer. Do not fake that bug.
            Path(argv[2].split("=", 1)[1] + "0").write_bytes(self.leaf)
            return SIGN.CommandResult(0)
        raise AssertionError(f"unexpected command: {argv}")


@unittest.skipUnless(sys.platform == "darwin", "staging uses actual macOS no-clobber rename")
class StageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "artifact"
        self.source.write_bytes(b"mock command-boundary Mach-O fixture")
        self.source.chmod(0o600)
        self.policy = self.root / "policy.json"
        self.policy.write_text(json.dumps(dict(schema_version=1, certificate_sha1=FINGERPRINT, team_id=TEAM)))
        self.output = self.root / "Hex.app"
        self.receipt = self.root / "receipt.json"

    def stage(self, runner=None, **kwargs):
        return SIGN.stage(self.source, kwargs.pop("product", "hex"), self.policy,
                          kwargs.pop("output", self.output), kwargs.pop("version", "1.2.3"),
                          kwargs.pop("receipt", self.receipt), run=runner or Runner(), **kwargs)

    def test_stages_exact_modes_plist_receipt_and_all_architectures(self):
        before = self.source.read_bytes()
        result = self.stage()
        self.assertEqual(self.source.read_bytes(), before)
        self.assertEqual(stat.S_IMODE(self.source.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE((self.output / "Contents/MacOS/hex").stat().st_mode), 0o755)
        self.assertEqual(result["source_mode"], 0o600)
        self.assertEqual(result["mach_o_uuids"], UUIDS)
        self.assertEqual(set(result["designated_requirements"]), set(UUIDS))
        self.assertEqual(result["certificate_sha1"], FINGERPRINT)
        self.assertEqual(result["candidate_executable_sha256"], hashlib.sha256(before).hexdigest())
        self.assertEqual(json.loads(self.receipt.read_text()), result)
        info = plistlib.loads((self.output / "Contents/Info.plist").read_bytes())
        self.assertEqual(info["CFBundleVersion"], "1.2.3")
        self.assertEqual(info["CFBundleExecutable"], "hex")
        self.assertEqual(info["CFBundleIdentifier"], "com.mrap.hex")
        self.assertTrue(info["NSLocalNetworkUsageDescription"].startswith("Hex "))

    def test_boi_uses_boi_identity_and_truthful_usage(self):
        result = self.stage(Runner("boi"), product="boi")
        info = plistlib.loads((self.output / "Contents/Info.plist").read_bytes())
        self.assertEqual(result["identifier"], "com.mrap.boi")
        self.assertEqual(info["CFBundleExecutable"], "boi")
        for key in ["NSDesktopFolderUsageDescription", "NSDownloadsFolderUsageDescription", "NSLocalNetworkUsageDescription"]:
            self.assertTrue(info[key].startswith("BOI "))
            self.assertNotIn("Hex", info[key])

    def test_different_build_bytes_and_filenames_keep_identity(self):
        first = self.stage(output=self.root / "first.app", receipt=None)
        self.source = self.root / "second-name"
        self.source.write_bytes(b"different build")
        second = self.stage(output=self.root / "second.app", receipt=None)
        self.assertEqual(first["identifier"], second["identifier"])
        self.assertEqual(first["designated_requirements"], second["designated_requirements"])
        self.assertNotEqual(first["source_sha256"], second["source_sha256"])

    def test_invalid_version_is_rejected_before_any_command(self):
        for version in ["1.2.3-preview", "1.2.3+build", "00.1.0", "1.100.1"]:
            runner = Runner()
            with self.subTest(version=version), self.assertRaisesRegex(SIGN.SigningError, "version"):
                self.stage(runner, version=version)
            self.assertEqual(runner.calls, [])
            self.assertFalse(self.output.exists())

    def test_current_hex_and_zero_major_versions_are_supported(self):
        for version in dict.fromkeys([hex_cargo_version(), "0.1.0"]):
            with self.subTest(version=version):
                runner = Runner()
                bundle = self.root / ("Hex-" + version + ".app")
                result = self.stage(runner, version=version, output=bundle, receipt=None)
                verified = SIGN.verify_installed(bundle, "hex", self.policy, run=runner)
                self.assertEqual(result["version"], version)
                self.assertEqual(verified["version"], version)
                info = plistlib.loads((bundle / "Contents/Info.plist").read_bytes())
                self.assertEqual(info["CFBundleVersion"], version)
                self.assertEqual(info["CFBundleShortVersionString"], version)

    def test_invalid_later_architecture_uuid_prevents_signing(self):
        runner = Runner()
        runner.uuid_text = f"UUID: {UUIDS['arm64']} (arm64)\nUUID: 00000000-0000-0000-0000-000000000000 (x86_64)\n"
        with self.assertRaisesRegex(SIGN.SigningError, "UUID"):
            self.stage(runner)
        self.assertEqual(len(runner.calls), 1)
        self.assertEqual(runner.calls[0][:2], [SIGN.DWARFDUMP, "--uuid"])
        self.assertFalse(self.output.exists())

    def test_keychain_lookup_and_signing_use_same_explicit_path(self):
        keychain = str(self.root / "explicit.keychain-db")
        data = json.loads(self.policy.read_text()); data["keychain"] = keychain
        self.policy.write_text(json.dumps(data))
        self.stage(Runner(keychain=keychain))

    def test_every_command_failure_prevents_publication(self):
        probe = Runner(); self.stage(probe, output=self.root / "probe.app", receipt=None)
        for index in range(len(probe.calls)):
            with self.subTest(command=probe.calls[index]):
                runner = Runner(); runner.fail_at = index
                with self.assertRaises(SIGN.SigningError) as error:
                    self.stage(runner)
                self.assertFalse(error.exception.published)
                self.assertFalse(self.output.exists())
                self.assertFalse(self.receipt.exists())

    def test_flags_and_entitlements_checked_on_nondefault_architecture(self):
        for flags in ["CodeDirectory flags=0x10000(runtime)\n", "CodeDirectory flags=0x8(installer)\n", "missing flags"]:
            runner = Runner(); runner.source_flags["x86_64"] = flags
            with self.subTest(flags=flags), self.assertRaisesRegex(SIGN.SigningError, "flags"):
                self.stage(runner)
        for payload in ["<dict>", "<dict><broken/></dict>", plistlib.dumps(["not a dict"]).decode(), plistlib.dumps({"com.apple.security.app-sandbox": True}).decode()]:
            runner = Runner(); runner.entitlements["x86_64"] = payload
            with self.subTest(entitlements=payload), self.assertRaisesRegex(SIGN.SigningError, "entitlements"):
                self.stage(runner)
        self.assertFalse(self.output.exists())

    def test_unsigned_and_empty_entitlements_are_supported(self):
        runner = Runner(); runner.unsigned = True
        self.stage(runner, output=self.root / "unsigned.app", receipt=None)
        runner = Runner(); runner.entitlements["x86_64"] = plistlib.dumps({}).decode()
        self.stage(runner)

    def test_wrong_signature_metadata_leaf_and_uuid_fail(self):
        cases = [("available_identity", "A" * 40), ("team", "WRONGTEAM1"), ("identifier", "com.attacker.other"), ("dr", False), ("leaf", b"wrong leaf"), ("signed_uuid_text", "UUID: " + UUIDS["arm64"] + " (arm64)\n")]
        for key, value in cases:
            runner = Runner(); setattr(runner, key, value)
            with self.subTest(field=key), self.assertRaises(SIGN.SigningError):
                self.stage(runner)
            self.assertFalse(self.output.exists())

    def test_actual_source_change_during_signing_is_detected(self):
        runner = Runner(); runner.on_sign = lambda: self.source.write_bytes(b"changed by fixture")
        with self.assertRaisesRegex(SIGN.SigningError, "source changed"):
            self.stage(runner)
        self.assertEqual(self.source.read_bytes(), b"changed by fixture")
        self.assertFalse(self.output.exists())

    def test_special_source_mode_bits_are_rejected(self):
        for bit in [stat.S_ISUID, stat.S_ISGID, stat.S_ISVTX]:
            self.source.chmod(0o700 | bit)
            with self.subTest(bit=bit), self.assertRaisesRegex(SIGN.SigningError, "special mode"):
                self.stage()
        self.assertFalse(self.output.exists())

    def test_source_fifo_is_rejected_without_waiting(self):
        self.source.unlink(); os.mkfifo(self.source)
        with self.assertRaisesRegex(SIGN.SigningError, "regular"):
            self.stage()

    def test_existing_and_dangling_destinations_never_overwrite(self):
        self.output.mkdir(); marker = self.output / "unrelated"; marker.write_text("keep")
        with self.assertRaises(SIGN.SigningError): self.stage()
        self.assertEqual(marker.read_text(), "keep")
        other = self.root / "dangling.app"; other.symlink_to(self.root / "absent")
        with self.assertRaises(SIGN.SigningError): self.stage(output=other)
        self.assertTrue(other.is_symlink())

    def test_receipt_parent_symlink_cannot_write_into_bundle(self):
        alias = self.root / "alias"; alias.symlink_to(self.output / "Contents", target_is_directory=True)
        runner = Runner()
        with self.assertRaisesRegex(SIGN.SigningError, "overlaps"):
            self.stage(runner, receipt=alias / "receipt.json")
        self.assertEqual(runner.calls, [])
        self.assertFalse(self.output.exists())

    def test_receipt_parent_alias_retarget_does_not_change_bound_destination(self):
        real = self.root / "receipts"; real.mkdir()
        alias = self.root / "alias"; alias.symlink_to(real, target_is_directory=True)
        def change_alias():
            alias.unlink(); alias.symlink_to(self.output / "Contents", target_is_directory=True)
        runner = Runner(); runner.on_sign = change_alias
        self.stage(runner, receipt=alias / "receipt.json")
        self.assertTrue((real / "receipt.json").is_file())
        self.assertFalse((self.output / "Contents/receipt.json").exists())

    def test_output_race_no_clobber_preserves_unrelated_directory(self):
        def occupy():
            self.output.mkdir(); (self.output / "unrelated").write_text("keep")
        runner = Runner(); runner.on_sign = occupy
        with self.assertRaises(SIGN.SigningError) as error: self.stage(runner)
        self.assertFalse(error.exception.published)
        self.assertEqual((self.output / "unrelated").read_text(), "keep")

    def test_receipt_preparation_failure_publishes_nothing(self):
        with patch.object(SIGN, "_prepare_receipt", side_effect=OSError("injected write failure")):
            with self.assertRaises(SIGN.SigningError) as error: self.stage()
        self.assertFalse(error.exception.published)
        self.assertFalse(self.output.exists())
        self.assertFalse(self.receipt.exists())

    def test_receipt_publish_failure_retains_candidate_with_truthful_partial_state(self):
        original = SIGN._atomic_publish
        def publish(source, destination, parent_fd):
            if destination == self.receipt: raise SIGN.SigningError("receipt collision")
            original(source, destination, parent_fd)
        with patch.object(SIGN, "_atomic_publish", side_effect=publish):
            with self.assertRaises(SIGN.SigningError) as error: self.stage()
        self.assertTrue(error.exception.published)
        self.assertEqual(error.exception.receipt["candidate"], str(self.output))
        self.assertTrue((self.output / "Contents/MacOS/hex").is_file())
        self.assertFalse(self.receipt.exists())

    def test_failure_never_deletes_replaced_public_output(self):
        original = SIGN._atomic_publish
        def publish(source, destination, parent_fd):
            if destination == self.receipt:
                self.output.rename(self.root / "retained.app")
                self.output.mkdir(); (self.output / "unrelated").write_text("keep")
                raise SIGN.SigningError("receipt failure after replacement")
            original(source, destination, parent_fd)
        with patch.object(SIGN, "_atomic_publish", side_effect=publish):
            with self.assertRaises(SIGN.SigningError) as error: self.stage()
        self.assertTrue(error.exception.published)
        self.assertEqual((self.output / "unrelated").read_text(), "keep")
        self.assertTrue((self.root / "retained.app/Contents/MacOS/hex").is_file())

    def test_stdout_failure_reports_published_without_deletion(self):
        receipt = self.stage(receipt=None)
        class BrokenOutput:
            def write(self, _): raise BrokenPipeError("closed reader")
            def flush(self): pass
        errors = io.StringIO()
        with patch.object(SIGN, "stage", return_value=receipt), patch.object(sys, "stdout", BrokenOutput()), patch.object(sys, "stderr", errors):
            result = SIGN.main([str(self.source), "hex", str(self.policy), str(self.root / "unused.app"), "--version", "1.2.3"])
        self.assertEqual(result, 1)
        self.assertTrue(json.loads(errors.getvalue())["published"])
        self.assertTrue(self.output.is_dir())


@unittest.skipUnless(sys.platform == "darwin", "fixture creation uses macOS no-clobber rename")
class VerifyInstalledTests(unittest.TestCase):
    stage = StageTests.stage
    def setUp(self):
        StageTests.setUp(self)
        self.stage()

    def readonly_runner(self, product="hex"):
        runner = Runner(product); runner.signed = True
        def call(argv, timeout):
            self.assertNotEqual(argv[0], SIGN.SECURITY)
            self.assertNotIn("--sign", argv)
            self.assertNotIn("--force", argv)
            return runner(argv, timeout)
        return runner, call

    def snapshot(self):
        return {str(p.relative_to(self.output)): (stat.S_IMODE(p.lstat().st_mode), p.read_bytes() if p.is_file() else None)
                for p in self.output.rglob("*")}

    def test_readonly_verification_works_without_signing_key_and_preserves_bundle(self):
        data = json.loads(self.policy.read_text()); data["keychain"] = str(self.root / "missing.keychain-db")
        self.policy.write_text(json.dumps(data))
        before = self.snapshot(); runner, call = self.readonly_runner()
        result = SIGN.verify_installed(self.output, "hex", self.policy, run=call)
        self.assertTrue(result["verified"])
        self.assertEqual(result["mach_o_uuids"], UUIDS)
        self.assertEqual(result["executable"], str(self.output / "Contents/MacOS/hex"))
        self.assertEqual(result["executable_sha256"], SIGN._sha256(self.output / "Contents/MacOS/hex"))
        self.assertEqual(before, self.snapshot())
        for argv in runner.calls:
            for token in argv:
                if token.startswith("--extract-certificates="):
                    prefix = Path(token.split("=", 1)[1])
                    self.assertNotIn(self.output, prefix.parents)
                    self.assertFalse(prefix.parent.exists())

    def test_readonly_boi_and_bundle_root_alias(self):
        boi = self.root / "BOI.app"
        self.stage(Runner("boi"), product="boi", output=boi, receipt=None)
        alias = self.root / "current-app"; alias.symlink_to(boi, target_is_directory=True)
        result = SIGN.verify_installed(alias, "boi", self.policy, run=self.readonly_runner("boi")[1])
        self.assertTrue(result["verified"])
        self.assertEqual(result["bundle"], str(boi))
        self.assertEqual(result["identifier"], "com.mrap.boi")
        self.assertEqual(result["executable"], str(boi / "Contents/MacOS/boi"))

    def test_readonly_rejects_wrong_plist_and_outside_executable(self):
        info_path = self.output / "Contents/Info.plist"; original = info_path.read_bytes()
        for key, value in [("CFBundleIdentifier", "com.mrap.boi"), ("CFBundleExecutable", "other"), ("CFBundlePackageType", "BNDL"), ("CFBundleName", "Other"), ("CFBundleVersion", "1.2.3-preview"), ("NSLocalNetworkUsageDescription", "BOI needs access")]:
            info = plistlib.loads(original); info[key] = value; info_path.write_bytes(plistlib.dumps(info))
            _, call = self.readonly_runner()
            with self.subTest(key=key), self.assertRaises(SIGN.SigningError):
                SIGN.verify_installed(self.output, "hex", self.policy, run=call)
        info_path.write_bytes(b"<plist>")
        with self.assertRaisesRegex(SIGN.SigningError, "malformed"):
            SIGN.verify_installed(self.output, "hex", self.policy, run=self.readonly_runner()[1])
        info_path.write_bytes(original)
        executable = self.output / "Contents/MacOS/hex"; executable.unlink(); executable.symlink_to(self.source)
        with self.assertRaises(SIGN.SigningError):
            SIGN.verify_installed(self.output, "hex", self.policy, run=self.readonly_runner()[1])

    def test_readonly_rejects_fifo_info_without_waiting(self):
        info = self.output / "Contents/Info.plist"; info.unlink(); os.mkfifo(info)
        with self.assertRaises(SIGN.SigningError):
            SIGN.verify_installed(self.output, "hex", self.policy, run=self.readonly_runner()[1])

    def test_readonly_detects_actual_executable_change(self):
        runner, normal = self.readonly_runner()
        executable = self.output / "Contents/MacOS/hex"
        def change(argv, timeout):
            result = normal(argv, timeout)
            if "--verify" in argv: executable.write_bytes(b"changed during verification")
            return result
        with self.assertRaisesRegex(SIGN.SigningError, "changed"):
            SIGN.verify_installed(self.output, "hex", self.policy, run=change)
        self.assertEqual(executable.read_bytes(), b"changed during verification")

    def test_readonly_detects_same_bytes_info_replacement(self):
        _, normal = self.readonly_runner()
        info = self.output / "Contents/Info.plist"
        def replace(argv, timeout):
            result = normal(argv, timeout)
            if "--verify" in argv:
                data = info.read_bytes(); info.rename(self.root / "retained-info.plist"); info.write_bytes(data)
            return result
        with self.assertRaisesRegex(SIGN.SigningError, "changed"):
            SIGN.verify_installed(self.output, "hex", self.policy, run=replace)
        self.assertEqual(info.read_bytes(), (self.root / "retained-info.plist").read_bytes())

    def test_readonly_missing_or_nonexecutable_file_rejected(self):
        executable = self.output / "Contents/MacOS/hex"; executable.chmod(0o644)
        with self.assertRaisesRegex(SIGN.SigningError, "executable"):
            SIGN.verify_installed(self.output, "hex", self.policy, run=self.readonly_runner()[1])
        executable.unlink()
        with self.assertRaises(SIGN.SigningError):
            SIGN.verify_installed(self.output, "hex", self.policy, run=self.readonly_runner()[1])

    def test_readonly_ignores_tempfile_discovery(self):
        before = self.snapshot()
        with patch.object(SIGN.tempfile, "gettempdir", side_effect=AssertionError("writable discovery is forbidden")):
            result = SIGN.verify_installed(self.output, "hex", self.policy, run=self.readonly_runner()[1])
        self.assertTrue(result["verified"])
        self.assertEqual(before, self.snapshot())

    def test_readonly_uncached_in_bundle_tmpdir_never_creates_a_probe(self):
        alias = self.root / "temp-alias"; alias.symlink_to(self.output / "Contents", target_is_directory=True)
        script = r'''import importlib.util, json, os, sys, tempfile
from pathlib import Path
spec = importlib.util.spec_from_file_location("signing_tests", sys.argv[1])
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
bundle = Path(sys.argv[2]); parent = bundle / "Contents"
created = []
def audit(event, args):
    if event == "open" and isinstance(args[0], (str, bytes)) and args[2] & os.O_CREAT:
        path = Path(os.fsdecode(args[0])).resolve()
        if path == bundle or bundle in path.parents:
            created.append(str(path.relative_to(bundle)))
sys.addaudithook(audit)
tempfile.tempdir = None
before = parent.stat()
runner = module.Runner(); runner.signed = True
error = None; verified = False
try:
    verified = module.SIGN.verify_installed(bundle, "hex", Path(sys.argv[3]), run=runner)["verified"]
except module.SIGN.SigningError as exc:
    error = str(exc)
after = parent.stat()
print(json.dumps({"verified": verified, "error": error, "created": created,
                  "before": [before.st_mtime_ns, before.st_ctime_ns],
                  "after": [after.st_mtime_ns, after.st_ctime_ns]}))
'''
        for temp_parent in [self.output / "Contents", alias]:
            with self.subTest(temp_parent=temp_parent):
                result = SIGN.subprocess.run([sys.executable, "-I", "-B", "-c", script, str(Path(__file__).resolve()), str(self.output), str(self.policy)],
                                             env={"PATH": "/usr/bin:/bin", "HOME": str(self.root), "TMPDIR": str(temp_parent)},
                                             capture_output=True, text=True, timeout=3)
                self.assertEqual(result.returncode, 0, result.stderr)
                evidence = json.loads(result.stdout)
                self.assertEqual(evidence["created"], [])
                self.assertTrue(evidence["verified"], evidence["error"])
                self.assertEqual(evidence["before"], evidence["after"])

    def test_readonly_native_rejection_and_metadata_failures_leave_bundle_unchanged(self):
        before = self.snapshot()
        for key, value in [("fail_at", 1), ("team", "WRONGTEAM1"), ("identifier", "com.other.app"), ("leaf", b"wrong"), ("dr", False), ("uuid_text", "")]:
            runner, call = self.readonly_runner(); setattr(runner, key, value)
            with self.subTest(key=key), self.assertRaises(SIGN.SigningError):
                SIGN.verify_installed(self.output, "hex", self.policy, run=call)
            self.assertEqual(before, self.snapshot())

    def test_readonly_cli_routes_without_stage_or_identity_lookup(self):
        before = self.snapshot(); output = io.StringIO()
        real = SIGN.verify_installed
        with patch.object(SIGN, "stage", side_effect=AssertionError("must not stage")), patch.object(SIGN, "_identity", side_effect=AssertionError("must not read signing key")), patch.object(SIGN, "verify_installed", side_effect=lambda *a, **kw: real(*a, run=self.readonly_runner()[1], **kw)), patch.object(sys, "stdout", output):
            code = SIGN.main(["verify-installed", str(self.output), "hex", str(self.policy)])
        self.assertEqual(code, 0)
        self.assertTrue(json.loads(output.getvalue())["verified"])
        self.assertEqual(before, self.snapshot())

    def test_readonly_cli_errors_and_closed_stdout_do_not_modify_bundle(self):
        before = self.snapshot(); output = io.StringIO(); errors = io.StringIO()
        real = SIGN.verify_installed
        def verify(*args, **kwargs):
            return real(*args, run=self.readonly_runner()[1], **kwargs)
        with patch.object(SIGN, "verify_installed", side_effect=verify), patch.object(sys, "stdout", output), patch.object(sys, "stderr", errors):
            code = SIGN.main(["verify-installed", str(self.output), "boi", str(self.policy)])
        self.assertEqual(code, 1)
        self.assertEqual(output.getvalue(), "")
        self.assertFalse(json.loads(errors.getvalue())["verification_completed"])
        class BrokenOutput:
            def write(self, _): raise BrokenPipeError("closed reader")
            def flush(self): pass
        errors = io.StringIO()
        with patch.object(SIGN, "verify_installed", side_effect=verify), patch.object(sys, "stdout", BrokenOutput()), patch.object(sys, "stderr", errors):
            code = SIGN.main(["verify-installed", str(self.output), "hex", str(self.policy)])
        self.assertEqual(code, 1)
        self.assertTrue(json.loads(errors.getvalue())["verification_completed"])
        self.assertEqual(before, self.snapshot())

    def test_actual_cli_rejects_wrong_bundle_without_modifying_it(self):
        info_path = self.output / "Contents/Info.plist"
        info = plistlib.loads(info_path.read_bytes()); info["CFBundleIdentifier"] = "com.wrong.product"
        info_path.write_bytes(plistlib.dumps(info)); before = self.snapshot()
        result = SIGN.subprocess.run([sys.executable, "-I", "-B", str(SOURCE), "verify-installed", str(self.output), "hex", str(self.policy)],
                                     env={"PATH": "/usr/bin:/bin", "HOME": str(self.root)},
                                     capture_output=True, text=True, timeout=3)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertFalse(json.loads(result.stderr)["verification_completed"])
        self.assertEqual(before, self.snapshot())

    def test_stage_cli_keeps_original_argument_shape(self):
        output = io.StringIO()
        with patch.object(SIGN, "stage", return_value={"published": True}) as stage, patch.object(sys, "stdout", output):
            code = SIGN.main(["verify-installed", "hex", str(self.policy), "new.app", "--version", "1.2.3"])
        self.assertEqual(code, 0)
        self.assertEqual(stage.call_args.args[0], Path("verify-installed"))
        self.assertTrue(json.loads(output.getvalue())["published"])


class ValidationTests(unittest.TestCase):
    def test_uuid_parser_rejects_zero_garbage_and_duplicate_architecture(self):
        for text in ["", "UUID: " + "0" * 32 + " (arm64)", "UUID: 00000000-0000-0000-0000-000000000000 (arm64)", "UUID: ------------------------------------ (arm64)", f"UUID: {UUIDS['arm64']} (arm64)\nUUID: {UUIDS['x86_64']} (arm64)", f"UUID: {UUIDS['arm64']} (arm64)\nnot a UUID"]:
            with self.subTest(text=text), self.assertRaises(SIGN.SigningError): SIGN._uuids(text)

    def test_version_rejects_prerelease_and_invalid_bundle_fields(self):
        for version in ["1.2.3-preview", "1.2.3+build", "1.2", "00.1.0", "12345.1.1", "1.100.1", "1.2.03"]:
            self.assertIsNone(SIGN.VERSION_RE.fullmatch(version), version)
        self.assertIsNotNone(SIGN.VERSION_RE.fullmatch("12.34.56"))
        self.assertIsNotNone(SIGN.VERSION_RE.fullmatch("0.52.2"))
        self.assertIsNotNone(SIGN.VERSION_RE.fullmatch("0.1.0"))

    def test_real_child_timeout_and_nonzero_are_loud(self):
        with self.assertRaisesRegex(SIGN.SigningError, "timed out"):
            SIGN.run_command([sys.executable, "-I", "-c", "import time; time.sleep(1)"], 0.01)
        with self.assertRaisesRegex(SIGN.SigningError, r"failed \(7\)"):
            SIGN._checked(SIGN.run_command, [sys.executable, "-I", "-c", "raise SystemExit(7)"], 3)

    def test_policy_rejects_duplicate_unknown_boolean_and_relative_keychain(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "policy"
            valid = dict(schema_version=1, certificate_sha1=FINGERPRINT, team_id=TEAM)
            for change in [dict(schema_version=True), dict(extra=1), dict(certificate_sha1="bad"), dict(team_id="bad"), dict(keychain="relative")]:
                value = dict(valid); value.update(change); path.write_text(json.dumps(value))
                with self.subTest(change=change), self.assertRaises(SIGN.SigningError): SIGN._read_policy(path)
            path.write_text('{"schema_version":1,"schema_version":1}')
            with self.assertRaisesRegex(SIGN.SigningError, "duplicate"): SIGN._read_policy(path)


if __name__ == "__main__":
    unittest.main(verbosity=2)
