import fcntl
import errno
import io
import importlib.util
import json
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path


SOURCE = Path(__file__).parents[1] / "system/scripts/macos-app-install.py"
spec = importlib.util.spec_from_file_location("macos_app_install_service", SOURCE)
INSTALL = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = INSTALL
spec.loader.exec_module(INSTALL)


class FakeSigner:
    def bind_owner(self, paths, lock_fd):
        return None

    def _result(self):
        return {"identifier": "com.mrap.hex.scipd", "version": "0.1.0", "team_id": "TEAM123456", "certificate_sha1": "A" * 40, "designated_requirements": {"arm64": "anchor apple generic"}, "mach_o_uuids": {"arm64": "11111111-1111-1111-1111-111111111111"}}

    def stage(self, source, product, policy, candidate, receipt):
        executable = candidate / "Contents/MacOS/scipd"
        executable.parent.mkdir(parents=True)
        shutil.copy2(source, executable)
        executable.chmod(0o755)
        (candidate / "Contents/Info.plist").write_bytes(b"fake")
        result = self._result()
        receipt.write_text(json.dumps(result), encoding="utf-8")
        return result

    def verify_installed(self, bundle, product, policy, expected=None):
        return self._result()


class FakeLaunchctl:
    def __init__(self, paths, loaded, program, fail_restart=False, fail_bootstrap=False):
        self.paths = paths
        self.loaded = loaded
        self.program = program
        self.fail_restart = fail_restart
        self.fail_bootstrap = fail_bootstrap
        self.calls = []
        self.lock_held = []

    def __call__(self, argv):
        self.calls.append(list(argv))
        fd = os.open(self.paths.lock, os.O_RDWR)
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                self.lock_held.append(True)
            else:
                self.lock_held.append(False)
        finally:
            os.close(fd)
        if argv[0] == "print":
            if not self.loaded:
                return 1, "", "Could not find service"
            return 0, f"\tprogram = {self.program}\n", ""
        if argv[0] == "bootout":
            if self.fail_restart:
                return 1, "", "bootout failed"
            self.loaded = False
            return 0, "", ""
        if argv[0] == "bootstrap":
            if self.fail_restart or self.fail_bootstrap:
                return 1, "", "bootstrap failed"
            self.loaded = True
            self.program = str(self.paths.executable.absolute())
            return 0, "", ""
        raise AssertionError(argv)


@unittest.skipUnless(sys.platform == "darwin", "requires macOS renameatx_np")
class MacAppServiceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.base = Path(self.temp.name)
        self.root = self.base / ".codeintel"
        self.policy = self.base / "policy.json"
        self.policy.write_text(json.dumps({"schema_version": 1, "certificate_sha1": "A" * 40, "team_id": "TEAM123456"}), encoding="utf-8")
        self.source = self.base / "scipd"
        self.source.write_bytes(b"scipd")
        self.helper_sources = {}
        self.helper_provenance = {}
        for name, data in (("macos-signing.py", b"signer"), ("macos-app-install.py", b"installer")):
            path = self.base / name
            path.write_bytes(data)
            self.helper_sources[name] = path
            self.helper_provenance[name] = {"sha256": INSTALL._sha256(path), "source_revision": "f" * 40}
        self.signer = FakeSigner()
        self.paths = INSTALL.product_paths("code-intel-daemon", self.root)
        self.plist = self.base / "Library/LaunchAgents/com.hex.scipd.plist"
        self.plist.parent.mkdir(parents=True)
        self.addCleanup(self.temp.cleanup)

    def seed(self):
        return INSTALL.install("code-intel-daemon", self.root, self.source, self.signer, policy_path=self.policy, helper_provenance=self.helper_provenance, helper_sources=self.helper_sources, source_revision="e" * 40, version="0.1.0")

    def write_plist(self, arguments=None, associated=None, **extra):
        value = {"Label": INSTALL.SCIPD_LAUNCHD_LABEL, "ProgramArguments": arguments or [str(self.paths.executable.absolute())]}
        if associated is not None:
            value["AssociatedBundleIdentifiers"] = associated
        value.update(extra)
        self.plist.write_bytes(plistlib.dumps(value, sort_keys=False))

    def test_loaded_service_updates_and_restarts_under_product_lock(self):
        self.seed()
        self.write_plist([str(self.paths.executable.absolute())], ["wrong.id"], KeepAlive=True, Nice=10, EnvironmentVariables={"PATH": "/usr/bin"})
        launchctl = FakeLaunchctl(self.paths, True, str(self.paths.executable.absolute()))
        result = INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=launchctl, plist_path=self.plist)
        self.assertEqual(result["service_action"], "restarted")
        self.assertTrue(all(launchctl.lock_held))
        self.assertIn(["bootout", f"gui/{os.getuid()}/{INSTALL.SCIPD_LAUNCHD_LABEL}"], launchctl.calls)
        self.assertIn(["bootstrap", f"gui/{os.getuid()}", str(self.plist.absolute())], launchctl.calls)
        value = plistlib.loads(self.plist.read_bytes())
        self.assertEqual(value["ProgramArguments"], [str(self.paths.executable.absolute())])
        self.assertEqual(value["AssociatedBundleIdentifiers"], ["com.mrap.hex.scipd"])
        self.assertEqual(value["EnvironmentVariables"], {"PATH": "/usr/bin"})

    def test_stopped_service_updates_without_start(self):
        self.seed()
        self.write_plist([str(self.paths.executable.absolute())])
        launchctl = FakeLaunchctl(self.paths, False, "")
        result = INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=launchctl, plist_path=self.plist)
        self.assertEqual(result["service_action"], "updated-stopped")
        self.assertFalse(result["service_recovery_pending"])
        self.assertEqual([call[0] for call in launchctl.calls], ["print"])

    def test_absent_service_does_not_create_or_start(self):
        self.seed()
        launchctl = FakeLaunchctl(self.paths, False, "")
        result = INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=launchctl, plist_path=self.plist)
        self.assertEqual(result["service_action"], "absent")
        self.assertFalse(result["service_recovery_pending"])
        self.assertFalse(self.plist.exists())
        self.assertEqual([call[0] for call in launchctl.calls], ["print"])

    def test_dry_run_does_not_change_plist_or_restart(self):
        self.seed()
        self.write_plist([str(self.paths.executable.absolute())], ["wrong.id"])
        before = self.plist.read_bytes()
        launchctl = FakeLaunchctl(self.paths, True, str(self.paths.executable.absolute()))
        result = INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, dry_run=True, launchctl=launchctl, plist_path=self.plist)
        self.assertTrue(result["service_needs_change"])
        self.assertEqual(result["service_action"], "would-restart")
        self.assertEqual(before, self.plist.read_bytes())
        self.assertEqual([call[0] for call in launchctl.calls], ["print"])

    def test_invalid_arguments_fail_before_mutation(self):
        self.seed()
        self.write_plist(["/tmp/unknown-scipd"])
        before = self.plist.read_bytes()
        launchctl = FakeLaunchctl(self.paths, False, "")
        with self.assertRaisesRegex(INSTALL.InstallError, "ProgramArguments"):
            INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=launchctl, plist_path=self.plist)
        self.assertEqual(before, self.plist.read_bytes())
        self.assertEqual([call[0] for call in launchctl.calls], ["print"])

    def test_restart_failure_reports_published(self):
        self.seed()
        self.write_plist([str(self.paths.executable.absolute())])
        launchctl = FakeLaunchctl(self.paths, True, str(self.paths.executable.absolute()), fail_restart=True)
        with self.assertRaisesRegex(INSTALL.InstallError, "bootout failed") as context:
            INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=launchctl, plist_path=self.plist)
        self.assertTrue(context.exception.published)
        self.assertTrue((self.paths.root / "SCIPD.service-reconcile-pending.json").exists())

    def test_bootstrap_failure_leaves_pending_marker(self):
        self.seed()
        self.write_plist([str(self.paths.executable.absolute())], ["wrong.id"])
        launchctl = FakeLaunchctl(self.paths, True, str(self.paths.executable.absolute()), fail_bootstrap=True)
        with self.assertRaisesRegex(INSTALL.InstallError, "bootstrap failed") as context:
            INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=launchctl, plist_path=self.plist)
        self.assertTrue(context.exception.published)
        marker = json.loads((self.paths.root / "SCIPD.service-reconcile-pending.json").read_text(encoding="utf-8"))
        self.assertEqual(marker["phase"], "reload-pending")

    def test_pending_unloaded_service_is_recovered(self):
        self.seed()
        self.write_plist([str(self.paths.executable.absolute())], ["wrong.id"])
        pending = self.paths.root / "SCIPD.service-reconcile-pending.json"
        pending.write_text(json.dumps({"schema_version": 1, "product": "code-intel-daemon", "generation": json.loads(self.paths.state.read_text(encoding="utf-8"))["generation"], "plist_sha256": INSTALL._sha256(self.plist), "bundle_identifier": "com.mrap.hex.scipd", "executable_path": str(self.paths.executable.absolute()), "phase": "reload-pending", "original_loaded": True}), encoding="utf-8")
        launchctl = FakeLaunchctl(self.paths, False, "")
        dry = INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, dry_run=True, launchctl=launchctl, plist_path=self.plist)
        self.assertTrue(dry["service_recovery_pending"])
        result = INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=launchctl, plist_path=self.plist)
        self.assertEqual(result["service_action"], "recovered")
        self.assertTrue(result["service_recovery_pending"])
        self.assertFalse(pending.exists())
        self.assertEqual([call[0] for call in launchctl.calls], ["print", "print", "bootstrap", "print"])
        state = json.loads(self.paths.state.read_text(encoding="utf-8"))
        state["generation"] = "new-generation"
        self.paths.state.write_text(json.dumps(state), encoding="utf-8")
        next_result = INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=launchctl, plist_path=self.plist)
        self.assertEqual(next_result["service_action"], "restarted")
        self.assertFalse(next_result["service_recovery_pending"])

    def test_conflicting_program_is_rejected(self):
        self.seed()
        self.write_plist([str(self.paths.executable.absolute())], ["com.mrap.hex.scipd"], Program="/tmp/not-scipd")
        with self.assertRaisesRegex(INSTALL.InstallError, "unsupported Program"):
            INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=FakeLaunchctl(self.paths, False, ""), plist_path=self.plist)

    def test_invalid_pending_marker_fails_before_mutation(self):
        self.seed()
        self.write_plist([str(self.paths.executable.absolute())])
        pending = self.paths.root / "SCIPD.service-reconcile-pending.json"
        pending.write_text(json.dumps({"schema_version": 1, "phase": "reload-pending"}), encoding="utf-8")
        before = self.plist.read_bytes()
        with self.assertRaisesRegex(INSTALL.InstallError, "invalid service reconcile pending"):
            INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=FakeLaunchctl(self.paths, False, ""), plist_path=self.plist)
        self.assertEqual(before, self.plist.read_bytes())

    def test_pending_without_plist_fails_closed(self):
        self.seed()
        pending = self.paths.root / "SCIPD.service-reconcile-pending.json"
        pending.write_text("{}", encoding="utf-8")
        with self.assertRaisesRegex(INSTALL.InstallError, "no readable plist"):
            INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=FakeLaunchctl(self.paths, False, ""), plist_path=self.plist)

    def test_generation_change_forces_reload(self):
        self.seed()
        self.write_plist([str(self.paths.executable.absolute())], ["com.mrap.hex.scipd"])
        first = FakeLaunchctl(self.paths, True, str(self.paths.executable.absolute()))
        INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=first, plist_path=self.plist)
        second = FakeLaunchctl(self.paths, True, str(self.paths.executable.absolute()))
        unchanged = INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=second, plist_path=self.plist)
        self.assertEqual(unchanged["service_action"], "loaded")
        state = json.loads(self.paths.state.read_text(encoding="utf-8"))
        state["generation"] = "new-generation"
        self.paths.state.write_text(json.dumps(state), encoding="utf-8")
        third = FakeLaunchctl(self.paths, True, str(self.paths.executable.absolute()))
        changed = INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=third, plist_path=self.plist)
        self.assertEqual(changed["service_action"], "restarted")
        self.assertIn(["bootout", f"gui/{os.getuid()}/{INSTALL.SCIPD_LAUNCHD_LABEL}"], third.calls)

    def test_oversized_and_symlink_plists_fail_closed(self):
        self.seed()
        self.plist.write_bytes(b"x" * (INSTALL.MAX_PLIST_BYTES + 1))
        with self.assertRaisesRegex(INSTALL.InstallError, "too large"):
            INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=FakeLaunchctl(self.paths, False, ""), plist_path=self.plist)
        self.plist.unlink()
        outside = self.base / "outside.plist"
        outside.write_bytes(b"not owned")
        self.plist.symlink_to(outside)
        with self.assertRaisesRegex(INSTALL.InstallError, "symlink"):
            INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=FakeLaunchctl(self.paths, False, ""), plist_path=self.plist)

    def check_private_write_failure(self, prefix):
        self.seed()
        self.write_plist([str(self.paths.cli.absolute())], ["wrong.id"])
        previous = self.plist.read_bytes()
        launchctl = FakeLaunchctl(self.paths, True, str(self.paths.cli.absolute()))
        write_private = INSTALL._write_private

        def disk_full(path, data):
            if path.name.startswith(prefix):
                raise OSError(errno.ENOSPC, "fixture private write disk full")
            return write_private(path, data)

        with patch.object(INSTALL, "_write_private", side_effect=disk_full):
            with self.assertRaisesRegex(INSTALL.InstallError, "fixture private write disk full") as error:
                INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=launchctl, plist_path=self.plist)
        self.assertIs(error.exception.published, True)
        self.assertNotEqual(self.plist.read_bytes(), previous)
        self.assertEqual(plistlib.loads(self.plist.read_bytes())["ProgramArguments"], [str(self.paths.executable.absolute())])
        marker = INSTALL._service_pending_path(self.paths)
        if "pending" in prefix:
            self.assertFalse(marker.exists())
            self.assertEqual([call[0] for call in launchctl.calls], ["print"])
        else:
            self.assertTrue(marker.exists())
            self.assertEqual(json.loads(marker.read_text())["generation"], json.loads(self.paths.state.read_text())["generation"])
            self.assertEqual([call[0] for call in launchctl.calls], ["print", "bootout", "bootstrap", "print"])
            self.assertFalse(INSTALL._service_receipt_path(self.paths).exists())

    def test_pending_private_write_oserror_reports_publication(self):
        self.check_private_write_failure("SCIPD.service-reconcile-pending.json.tmp-")

    def test_receipt_private_write_oserror_reports_publication(self):
        self.check_private_write_failure("SCIPD.service-state.json.tmp-")

    def test_postpublication_plist_read_oserror_reports_publication(self):
        self.seed()
        self.write_plist([str(self.paths.cli.absolute())], ["wrong.id"])
        launchctl = FakeLaunchctl(self.paths, True, str(self.paths.cli.absolute()))
        with patch.object(INSTALL, "_service_plist_sha256", side_effect=OSError(errno.EACCES, "fixture published plist read denied")):
            with self.assertRaisesRegex(INSTALL.InstallError, "fixture published plist read denied") as error:
                INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=launchctl, plist_path=self.plist)
        self.assertIs(error.exception.published, True)
        self.assertEqual(plistlib.loads(self.plist.read_bytes())["ProgramArguments"], [str(self.paths.executable.absolute())])
        self.assertEqual([call[0] for call in launchctl.calls], ["print"])

    def check_cli_private_write_failure(self, prefix):
        self.seed()
        self.write_plist([str(self.paths.cli.absolute())], ["wrong.id"])
        central = self.base / "Library/Application Support/Hex/build-signing/policy.json"
        central.parent.mkdir(parents=True)
        shutil.copy2(self.policy, central)
        # Actual CLI parser, service implementation and error serializer in a
        # fresh process. Only crypto, launchctl and the failed write are fixtures.
        bootstrap = """import importlib.util,sys,errno
from pathlib import Path
spec=importlib.util.spec_from_file_location('service_cli_fixture',sys.argv[1])
m=importlib.util.module_from_spec(spec);sys.modules[spec.name]=m;spec.loader.exec_module(m)
a=m.INSTALL;root=Path(sys.argv[2]);prefix=sys.argv[3]
paths=a.product_paths('code-intel-daemon',root)
a.ProcessSigner=m.FakeSigner
launchctl=m.FakeLaunchctl(paths,True,str(paths.cli.absolute()))
a._launchctl_default=lambda argv,lock_fd=None:launchctl(argv)
original=a._write_private
def disk_full(path,data):
    if path.name.startswith(prefix):raise OSError(errno.ENOSPC,'fixture private write disk full')
    return original(path,data)
a._write_private=disk_full
raise SystemExit(a.main(['service-reconcile','code-intel-daemon','--root',str(root)]))
"""
        result = subprocess.run([sys.executable, "-I", "-B", "-c", bootstrap, str(Path(__file__).resolve()), str(self.root), prefix], capture_output=True, text=True, timeout=10, env={"HOME": str(self.base), "PATH": "/usr/bin:/bin"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        payload = json.loads(result.stderr)
        self.assertIs(payload["published"], True)
        self.assertIn("fixture private write disk full", payload["error"])
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(plistlib.loads(self.plist.read_bytes())["ProgramArguments"], [str(self.paths.executable.absolute())])
        self.assertEqual(INSTALL._service_pending_path(self.paths).exists(), "pending" not in prefix)

    def test_actual_cli_pending_write_oserror_has_partial_json(self):
        self.check_cli_private_write_failure("SCIPD.service-reconcile-pending.json.tmp-")

    def test_actual_cli_receipt_write_oserror_has_partial_json(self):
        self.check_cli_private_write_failure("SCIPD.service-state.json.tmp-")

    def test_receipt_failure_reports_published(self):
        self.seed()
        self.write_plist([str(self.paths.executable.absolute())], ["wrong.id"])
        launchctl = FakeLaunchctl(self.paths, True, str(self.paths.executable.absolute()))
        with patch.object(INSTALL, "_write_service_receipt", side_effect=INSTALL.InstallError("receipt disk full", published=True)):
            with self.assertRaisesRegex(INSTALL.InstallError, "receipt disk full") as context:
                INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=launchctl, plist_path=self.plist)
        self.assertTrue(context.exception.published)
        self.assertEqual(plistlib.loads(self.plist.read_bytes())["ProgramArguments"], [str(self.paths.executable.absolute())])

    def test_launchctl_exception_reports_published(self):
        self.seed()
        self.write_plist([str(self.paths.executable.absolute())], ["wrong.id"])

        def broken_launchctl(argv):
            if argv[0] == "print":
                return 0, f"\tprogram = {self.paths.executable.absolute()}\n", ""
            raise INSTALL.InstallError("launchctl transport failed")

        with self.assertRaisesRegex(INSTALL.InstallError, "launchctl transport failed") as context:
            INSTALL.service_reconcile("code-intel-daemon", self.root, self.signer, policy_path=self.policy, launchctl=broken_launchctl, plist_path=self.plist)
        self.assertTrue(context.exception.published)

    def test_actual_copied_cli_uses_injected_boundaries(self):
        fixture = self.base / "fixture"
        fixture.mkdir()
        copied_app = fixture / "macos-app-install.py"
        copied_signer = fixture / "macos-signing.py"
        shutil.copy2(SOURCE, copied_app)
        shutil.copy2(Path(__file__).parents[1] / "system/scripts/macos-signing.py", copied_signer)
        copied_spec = importlib.util.spec_from_file_location("copied_app_install_cli", copied_app)
        copied = importlib.util.module_from_spec(copied_spec)
        sys.modules[copied_spec.name] = copied
        copied_spec.loader.exec_module(copied)
        home = self.base / "home"
        policy = home / "Library/Application Support/Hex/build-signing/policy.json"
        policy.parent.mkdir(parents=True)
        policy.write_text(json.dumps({"schema_version": 1, "certificate_sha1": "A" * 40, "team_id": "TEAM123456"}), encoding="utf-8")
        root = home / ".codeintel"
        helper_sources = {}
        helper_provenance = {}
        for name in ("macos-signing.py", "macos-app-install.py"):
            path = fixture / name
            helper_sources[name] = path
            helper_provenance[name] = {"sha256": copied._sha256(path), "source_revision": "f" * 40}
        source = fixture / "scipd"
        source.write_bytes(b"scipd")
        copied.install("code-intel-daemon", root, source, self.signer, policy_path=policy, helper_provenance=helper_provenance, helper_sources=helper_sources, source_revision="e" * 40, version="0.1.0")
        paths = copied.product_paths("code-intel-daemon", root)
        plist = home / "Library/LaunchAgents/com.hex.scipd.plist"
        plist.parent.mkdir(parents=True)
        plist.write_bytes(plistlib.dumps({"Label": copied.SCIPD_LAUNCHD_LABEL, "ProgramArguments": [str(paths.executable.absolute())]}, sort_keys=False))
        fake_signer = """import json\nimport sys\nfrom pathlib import Path\ndef _read_policy(path):\n    json.loads(path.read_text(encoding='utf-8'))\nif len(sys.argv) > 1 and sys.argv[1] == 'verify-installed':\n    Path(%r).write_text(json.dumps(sys.argv))\n    state = Path(sys.argv[2]).parent / 'SCIPD.app.install-state.json'\n    print(json.dumps(json.loads(state.read_text(encoding='utf-8'))))\n""" % str(self.base / "signer-argv.json")
        copied_signer.write_text(fake_signer, encoding="utf-8")
        (paths.helper_dir / "macos-signing.py").write_text(fake_signer, encoding="utf-8")
        state_path = paths.state
        state = json.loads(state_path.read_text(encoding="utf-8"))
        state["helpers"]["macos-signing.py"]["sha256"] = copied._sha256(copied_signer)
        state_path.write_text(json.dumps(state), encoding="utf-8")
        state_file = self.base / "launchctl-state.json"
        state_file.write_text(json.dumps({"loaded": True, "program": str(paths.cli.absolute())}), encoding="utf-8")
        fake_launchctl = self.base / "launchctl"
        fake_launchctl.write_text("""#!/usr/bin/python3\nimport json\nimport plistlib\nimport sys\nfrom pathlib import Path\nstate = Path(%r)\ndata = json.loads(state.read_text())\nif sys.argv[1] == 'print':\n    if not data['loaded']:\n        print('Could not find service', file=sys.stderr)\n        raise SystemExit(1)\n    print('\\tprogram = ' + data['program'])\nelif sys.argv[1] == 'bootout':\n    data['loaded'] = False\n    state.write_text(json.dumps(data))\nelif sys.argv[1] == 'bootstrap':\n    plist = plistlib.loads(Path(sys.argv[-1]).read_bytes())\n    data.update(loaded=True, program=plist['ProgramArguments'][0])\n    state.write_text(json.dumps(data))\nelse:\n    raise SystemExit(2)\n""" % str(state_file), encoding="utf-8")
        fake_launchctl.chmod(0o755)
        copied_app.write_text(copied_app.read_text(encoding="utf-8").replace('command = ["/bin/launchctl", *argv]', f'command = [{str(fake_launchctl)!r}, *argv]'), encoding="utf-8")
        subprocess_result = subprocess.run([sys.executable, "-I", "-B", str(copied_app), "service-reconcile", "code-intel-daemon", "--root", str(root)], capture_output=True, text=True, env={"HOME": str(home), "PATH": "/usr/bin:/bin"})
        signer_log = (self.base / "signer-argv.json").read_text(encoding="utf-8") if (self.base / "signer-argv.json").exists() else "missing"
        self.assertEqual(subprocess_result.returncode, 0, repr((subprocess_result.returncode, subprocess_result.stdout, subprocess_result.stderr, signer_log)))
        subprocess_payload = json.loads(subprocess_result.stdout)
        self.assertEqual(subprocess_payload["service_action"], "restarted")
        self.assertEqual(json.loads(state_file.read_text(encoding="utf-8"))["program"], str(paths.executable.absolute()))
        self.assertEqual(json.loads((self.base / "signer-argv.json").read_text(encoding="utf-8"))[1], "verify-installed")
        launchctl = FakeLaunchctl(paths, False, "")
        class InjectedSigner(FakeSigner):
            pass
        copied.ProcessSigner = InjectedSigner
        copied._launchctl_default = lambda argv, lock_fd=None: launchctl(argv)
        output = io.StringIO()
        with patch.dict(os.environ, {"HOME": str(home)}), patch("sys.stdout", output):
            self.assertEqual(copied.main(["service-reconcile", "code-intel-daemon", "--root", str(root)]), 0)
        result = json.loads(output.getvalue())
        self.assertEqual(result["product"], "code-intel-daemon")
        self.assertEqual(result["service_action"], "stopped")

        completed = subprocess.run([sys.executable, "-I", "-B", str(copied_app), "service-reconcile", "code-intel-cli", "--root", str(root)], capture_output=True, text=True, env={"HOME": str(home), "PATH": "/usr/bin:/bin"})
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("only code-intel-daemon", completed.stderr)
        service_cli = subprocess.run([sys.executable, "-I", "-B", str(copied_app), "service-reconcile", "code-intel-daemon", "--root", str(home / "missing-codeintel")], capture_output=True, text=True, env={"HOME": str(home), "PATH": "/usr/bin:/bin"})
        self.assertNotEqual(service_cli.returncode, 0)
        self.assertIn("product root", service_cli.stderr)

    def test_cli_product_is_rejected(self):
        with self.assertRaisesRegex(INSTALL.InstallError, "only code-intel-daemon"):
            INSTALL.service_reconcile("code-intel-cli", self.root, self.signer, policy_path=self.policy, launchctl=FakeLaunchctl(self.paths, False, ""), plist_path=self.plist)


if __name__ == "__main__":
    unittest.main()
