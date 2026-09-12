import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SOURCE = Path(__file__).parents[1] / "system/scripts/macos-app-install.py"
spec = importlib.util.spec_from_file_location("macos_app_install", SOURCE)
INSTALL = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = INSTALL
spec.loader.exec_module(INSTALL)


def run_owned(helper, argv=None, timeout=30):
    root = helper.parent / "runner-owner"
    root.mkdir(exist_ok=True)
    paths = INSTALL.product_paths("boi", root)
    with INSTALL._product_lock(paths) as fd:
        runner = INSTALL.ProcessSigner(helper, timeout=timeout)
        runner.bind_owner(paths, fd)
        try:
            return runner._run(argv or [])
        except BaseException:
            receipt = INSTALL._failure_path(paths)
            if receipt.exists():
                try:
                    error = INSTALL._read_json(receipt, "fixture cleanup receipt").get("error")
                    print(json.dumps({"cleanup_error": error}), file=sys.stderr)
                except (INSTALL.InstallError, OSError):
                    print("fixture cleanup diagnostic unavailable", file=sys.stderr)
            raise


class FakeSigner:
    def __init__(self):
        self.stage_calls = []
        self.verify_calls = []
        self.fail_stage = False

    def result(self, product):
        return {
            "identifier": INSTALL.PRODUCTS[product].bundle_identifier,
            "version": "1.0.0",
            "team_id": "TEAM123456",
            "certificate_sha1": "A" * 40,
            "designated_requirements": {"arm64": "anchor apple generic"},
            "mach_o_uuids": {"arm64": "11111111-1111-1111-1111-111111111111"},
        }

    def stage(self, source, product, policy, candidate, receipt):
        self.stage_calls.append((source, product, policy, candidate, receipt))
        if self.fail_stage:
            raise INSTALL.InstallError("injected stage failure")
        item = INSTALL.PRODUCTS[product]
        executable = candidate / "Contents/MacOS" / item.executable
        executable.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, executable)
        executable.chmod(0o755)
        (candidate / "Contents/Info.plist").write_bytes(b"public fake plist")
        result = self.result(product)
        receipt.write_text(json.dumps(result), encoding="utf-8")
        return result

    def verify_installed(self, bundle, product, policy, expected=None):
        self.verify_calls.append((bundle, product, policy, expected))
        return self.result(product)


@unittest.skipUnless(sys.platform == "darwin", "requires macOS renameatx_np")
class MacAppInstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name) / ".boi"
        self.root.mkdir()
        (self.root / "libexec").mkdir()
        self.helpers = {}
        for name in ("macos-signing.py", "macos-app-install.py"):
            path = self.root / "libexec" / name
            path.write_text(name, encoding="utf-8")
            self.helpers[name] = {"sha256": INSTALL._sha256(path), "source_revision": "f" * 40}
        self.policy = Path(self.temp.name) / "policy.json"
        self.policy.write_text(json.dumps({"schema_version": 1, "certificate_sha1": "A" * 40, "team_id": "TEAM123456"}), encoding="utf-8")
        self.source = Path(self.temp.name) / "boi-source"
        self.source.write_bytes(b"candidate bytes")
        self.signer = FakeSigner()
        # FakeSigner.stage() copies opaque bytes into the candidate executable, so a
        # real self-check subprocess would always fail here. Default it to a clean
        # pass; tests of the self-check itself override this patch.
        from unittest.mock import patch
        self._self_check_patch = patch.object(
            INSTALL, "_run_self_check",
            return_value=subprocess.CompletedProcess(args=["cli", "--version"], returncode=0, stdout="ok\n", stderr=""))
        self._self_check_patch.start()
        self.addCleanup(self._self_check_patch.stop)
        self.addCleanup(self.temp.cleanup)

    def install(self):
        sources = {name: self.root / "libexec" / name for name in self.helpers}
        return INSTALL.install("boi", self.root, self.source, self.signer, policy_path=self.policy, helper_provenance=self.helpers, helper_sources=sources, source_revision="e" * 40)

    def _hex_fixture(self, workspace_name, *, with_module=False):
        # install.sh convention: root = <workspace>/.hex, workspace holds CLAUDE.md
        # and .hex/modules -- see _module_verify_workspace's docstring.
        workspace = Path(self.temp.name) / workspace_name
        root = workspace / ".hex"
        root.mkdir(parents=True)
        (root / "libexec").mkdir()
        helpers = {}
        sources = {}
        for name in ("macos-signing.py", "macos-app-install.py"):
            path = root / "libexec" / name
            path.write_text(name, encoding="utf-8")
            helpers[name] = {"sha256": INSTALL._sha256(path), "source_revision": "f" * 40}
            sources[name] = path
        if with_module:
            (root / "modules").mkdir()
            (root / "modules" / "orbstack_prune.worker.rs").write_text("// w", encoding="utf-8")
        install_kwargs = dict(policy_path=self.policy, helper_provenance=helpers, helper_sources=sources, source_revision="e" * 40)
        return workspace, root, install_kwargs

    def test_empty_publication_uses_fixed_paths_and_state(self):
        result = self.install()
        paths = INSTALL.product_paths("boi", self.root)
        self.assertTrue(paths.app.is_dir())
        self.assertTrue(paths.executable.is_file())
        self.assertTrue(paths.cli.is_symlink())
        self.assertEqual(paths.cli.resolve(), paths.executable.resolve())
        self.assertEqual(result["bundle_identifier"], "com.mrap.boi")
        self.assertEqual(json.loads(paths.state.read_text())["mode"], "signed-current")
        self.assertFalse(paths.journal.exists())

    def test_legacy_raw_is_retained_in_rollback(self):
        paths = INSTALL.product_paths("boi", self.root)
        paths.cli.parent.mkdir()
        paths.cli.write_bytes(b"old raw")
        paths.cli.chmod(0o755)
        result = self.install()
        rollback = self.root / f".boi.app-install-rollback-{result['transaction_id']}" / "previous-cli"
        self.assertTrue(rollback.is_file())
        self.assertEqual(rollback.read_bytes(), b"old raw")
        self.assertTrue(paths.cli.is_symlink())

    def test_existing_app_uses_directory_swap(self):
        self.install()
        old_hash = INSTALL._tree_sha256(INSTALL.product_paths("boi", self.root).app)
        self.source.write_bytes(b"second candidate")
        result = self.install()
        paths = INSTALL.product_paths("boi", self.root)
        rollback = self.root / f".boi.app-install-rollback-{result['transaction_id']}" / "previous-app"
        self.assertEqual(INSTALL._tree_sha256(rollback), old_hash)
        self.assertNotEqual(INSTALL._tree_sha256(paths.app), old_hash)

    def test_compatibility_swap_failure_restores_previous_app(self):
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        old_app_hash = INSTALL._tree_sha256(paths.app)
        old_cli_target = os.readlink(paths.cli)
        original_swap = INSTALL._atomic_swap
        calls = {"count": 0}

        def fail_cli(parent_fd, source, destination):
            calls["count"] += 1
            if calls["count"] == 4:
                raise INSTALL.InstallError("injected compatibility swap failure")
            return original_swap(parent_fd, source, destination)

        INSTALL._atomic_swap = fail_cli
        try:
            with self.assertRaisesRegex(INSTALL.InstallError, "compatibility swap failure"):
                self.install()
        finally:
            INSTALL._atomic_swap = original_swap
        self.assertEqual(INSTALL._tree_sha256(paths.app), old_app_hash)
        self.assertEqual(os.readlink(paths.cli), old_cli_target)

    def test_self_check_happy_path_reports_not_failed(self):
        result = self.install()
        self.assertFalse(result["self_check_failed"])
        paths = INSTALL.product_paths("boi", self.root)
        self.assertFalse(json.loads(paths.state.read_text())["self_check_failed"])

    def test_self_check_nonzero_exit_restores_previous_app_and_cli(self):
        from unittest.mock import patch
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        old_app_hash = INSTALL._tree_sha256(paths.app)
        old_cli_target = os.readlink(paths.cli)
        failing = subprocess.CompletedProcess(args=["cli", "--version"], returncode=1, stdout="", stderr="boom")
        with patch.object(INSTALL, "_run_self_check", return_value=failing):
            with self.assertRaisesRegex(INSTALL.InstallError, "self-check failed") as caught:
                self.install()
        # Self-check runs after every swap (KTD1), so unlike a staging-time failure
        # this transaction did touch public state before rolling back -- published
        # stays true, matching every other post-swap failure path in this function.
        self.assertTrue(caught.exception.self_check_failed)
        self.assertTrue(caught.exception.published)
        self.assertEqual(INSTALL._tree_sha256(paths.app), old_app_hash)
        self.assertEqual(os.readlink(paths.cli), old_cli_target)

    def test_self_check_failure_clears_journal_so_next_install_succeeds(self):
        from unittest.mock import patch
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        failing = subprocess.CompletedProcess(args=["cli", "--version"], returncode=1, stdout="", stderr="boom")
        with patch.object(INSTALL, "_run_self_check", return_value=failing):
            with self.assertRaisesRegex(INSTALL.InstallError, "self-check failed"):
                self.install()
        # The rollback above restored public state cleanly (no rollback
        # errors), so the journal must not be left behind blocking the next
        # install -- previously it was, and a later install() refused with
        # "open install journal requires recovery" until an operator ran
        # abandon-staging by hand.
        self.assertFalse(paths.journal.exists())
        # A second, good install succeeds with no manual recovery step.
        result = self.install()
        self.assertEqual(result["bundle_identifier"], "com.mrap.boi")

    def test_self_check_times_out_twice_restores_previous_app(self):
        from unittest.mock import patch
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        old_app_hash = INSTALL._tree_sha256(paths.app)
        old_cli_target = os.readlink(paths.cli)
        timeout_exc = subprocess.TimeoutExpired(cmd=["cli", "--version"], timeout=10)
        with patch.object(INSTALL, "_run_self_check", side_effect=timeout_exc), \
                patch.object(INSTALL, "_self_check_sleep") as sleep_mock:
            with self.assertRaisesRegex(INSTALL.InstallError, "timed out twice") as caught:
                self.install()
        sleep_mock.assert_called_once()
        self.assertTrue(caught.exception.self_check_failed)
        self.assertEqual(INSTALL._tree_sha256(paths.app), old_app_hash)
        self.assertEqual(os.readlink(paths.cli), old_cli_target)

    def test_self_check_times_out_once_then_succeeds(self):
        from unittest.mock import patch
        timeout_exc = subprocess.TimeoutExpired(cmd=["cli", "--version"], timeout=10)
        ok = subprocess.CompletedProcess(args=["cli", "--version"], returncode=0, stdout="ok\n", stderr="")
        with patch.object(INSTALL, "_run_self_check", side_effect=[timeout_exc, ok]), \
                patch.object(INSTALL, "_self_check_sleep") as sleep_mock:
            result = self.install()
        sleep_mock.assert_called_once()
        self.assertFalse(result["self_check_failed"])

    def test_self_check_skips_product_without_cli_path(self):
        # No current product configures cli_relative=None (Product.cli_relative is not
        # Optional), so this exercises the guard directly rather than through install().
        from unittest.mock import patch
        with patch.object(INSTALL, "_run_self_check") as run_mock:
            INSTALL._self_check_published_cli(None, INSTALL.PRODUCTS["boi"], self.root)
        run_mock.assert_not_called()

    def test_module_verify_skipped_without_modules_dir(self):
        # (a) No .hex/modules at all: only --version runs.
        from unittest.mock import patch
        workspace, root, _ = self._hex_fixture("no-modules")
        cli = root / "bin" / "hex"
        version_ok = subprocess.CompletedProcess(args=["hex", "--version"], returncode=0, stdout="hex 1.0.0\n", stderr="")
        with patch.object(INSTALL, "_run_self_check", return_value=version_ok) as run_mock:
            INSTALL._self_check_published_cli(cli, INSTALL.PRODUCTS["hex"], root)
        run_mock.assert_called_once_with(cli, ["--version"], INSTALL.SELF_CHECK_DEFAULT_TIMEOUT_SECS)

    def test_module_verify_runs_after_version_and_commits_install(self):
        # (b) modules dir with one worker file, verify returns 0: install commits,
        # both commands ran in order with HEX_DIR pointed at the workspace (not root).
        from unittest.mock import patch
        workspace, root, install_kwargs = self._hex_fixture("has-modules", with_module=True)
        version_ok = subprocess.CompletedProcess(args=["hex", "--version"], returncode=0, stdout="hex 1.0.0\n", stderr="")
        verify_ok = subprocess.CompletedProcess(args=["hex", "module", "verify"], returncode=0, stdout="module verify: OK\n", stderr="")

        def fake_run(cli, args, timeout, env=None):
            return verify_ok if args == ["module", "verify"] else version_ok

        with patch.object(INSTALL, "_run_self_check", side_effect=fake_run) as run_mock:
            result = INSTALL.install("hex", root, self.source, self.signer, **install_kwargs)

        self.assertEqual(result["product"], "hex")
        self.assertFalse(result["self_check_failed"])
        paths = INSTALL.product_paths("hex", root)
        self.assertEqual(run_mock.call_count, 2)
        first_call, second_call = run_mock.call_args_list
        self.assertEqual(first_call.args, (paths.cli, ["--version"], INSTALL.SELF_CHECK_DEFAULT_TIMEOUT_SECS))
        self.assertEqual(first_call.kwargs, {})
        self.assertEqual(second_call.args, (paths.cli, ["module", "verify"], INSTALL.SELF_CHECK_DEFAULT_TIMEOUT_SECS))
        self.assertEqual(second_call.kwargs, {"env": {"HEX_DIR": str(workspace)}})

    def test_module_verify_failure_rolls_back_install(self):
        # (c) verify returns 1 with stderr naming the missing module: rollback,
        # InstallError text names "module verify", self_check_failed True.
        from unittest.mock import patch
        workspace, root, install_kwargs = self._hex_fixture("modules-missing", with_module=True)
        version_ok = subprocess.CompletedProcess(args=["hex", "--version"], returncode=0, stdout="hex 1.0.0\n", stderr="")
        verify_fail = subprocess.CompletedProcess(
            args=["hex", "module", "verify"], returncode=1, stdout="",
            stderr="module verify: MISSING orbstack_prune.worker.rs — on disk but not "
                   "compiled into this binary; build with --features personal")

        def fake_run(cli, args, timeout, env=None):
            return verify_fail if args == ["module", "verify"] else version_ok

        with patch.object(INSTALL, "_run_self_check", side_effect=fake_run):
            with self.assertRaisesRegex(INSTALL.InstallError, "module verify") as caught:
                INSTALL.install("hex", root, self.source, self.signer, **install_kwargs)

        self.assertTrue(caught.exception.self_check_failed)
        self.assertIn("MISSING", str(caught.exception))
        paths = INSTALL.product_paths("hex", root)
        self.assertFalse(paths.app.exists())
        self.assertFalse(paths.cli.exists())
        self.assertFalse(paths.journal.exists())

    def test_module_verify_never_runs_for_non_hex_product(self):
        # (d) product "boi" with a modules dir present: verify never runs.
        from unittest.mock import patch
        modules_dir = self.root.parent / ".hex" / "modules"
        modules_dir.mkdir(parents=True)
        (modules_dir / "orbstack_prune.worker.rs").write_text("// w", encoding="utf-8")
        version_ok = subprocess.CompletedProcess(args=["boi", "--version"], returncode=0, stdout="boi 1.0.0\n", stderr="")
        with patch.object(INSTALL, "_run_self_check", return_value=version_ok) as run_mock:
            result = self.install()
        self.assertFalse(result["self_check_failed"])
        run_mock.assert_called_once_with(
            INSTALL.product_paths("boi", self.root).cli, ["--version"], INSTALL.SELF_CHECK_DEFAULT_TIMEOUT_SECS)

    def test_self_check_oserror_on_first_attempt_restores_previous_app_and_cli(self):
        from unittest.mock import patch
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        old_app_hash = INSTALL._tree_sha256(paths.app)
        old_cli_target = os.readlink(paths.cli)
        with patch.object(INSTALL, "_run_self_check", side_effect=OSError("no such file")):
            with self.assertRaisesRegex(INSTALL.InstallError, "could not execute") as caught:
                self.install()
        self.assertTrue(caught.exception.self_check_failed)
        self.assertEqual(INSTALL._tree_sha256(paths.app), old_app_hash)
        self.assertEqual(os.readlink(paths.cli), old_cli_target)

    def test_self_check_oserror_after_retry_restores_previous_app_and_cli(self):
        from unittest.mock import patch
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        old_app_hash = INSTALL._tree_sha256(paths.app)
        old_cli_target = os.readlink(paths.cli)
        timeout_exc = subprocess.TimeoutExpired(cmd=["cli", "--version"], timeout=10)
        with patch.object(INSTALL, "_run_self_check", side_effect=[timeout_exc, OSError("no such file")]), \
                patch.object(INSTALL, "_self_check_sleep") as sleep_mock:
            with self.assertRaisesRegex(INSTALL.InstallError, "could not execute") as caught:
                self.install()
        sleep_mock.assert_called_once()
        self.assertTrue(caught.exception.self_check_failed)
        self.assertEqual(INSTALL._tree_sha256(paths.app), old_app_hash)
        self.assertEqual(os.readlink(paths.cli), old_cli_target)

    def test_self_check_timeout_secs_env_parsing(self):
        from unittest.mock import patch
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop(INSTALL.SELF_CHECK_TIMEOUT_ENV, None)
            self.assertEqual(
                INSTALL._self_check_timeout_secs(), INSTALL.SELF_CHECK_DEFAULT_TIMEOUT_SECS)
        with patch.dict(os.environ, {INSTALL.SELF_CHECK_TIMEOUT_ENV: "garbage"}):
            self.assertEqual(
                INSTALL._self_check_timeout_secs(), INSTALL.SELF_CHECK_DEFAULT_TIMEOUT_SECS)
        with patch.dict(os.environ, {INSTALL.SELF_CHECK_TIMEOUT_ENV: "3.5"}):
            self.assertEqual(INSTALL._self_check_timeout_secs(), 3.5)

    def test_install_error_payload_reports_self_check_failed(self):
        from unittest.mock import patch
        self.install()
        failing = subprocess.CompletedProcess(args=["cli", "--version"], returncode=1, stdout="", stderr="boom")
        with patch.object(INSTALL, "_run_self_check", return_value=failing):
            with self.assertRaises(INSTALL.InstallError) as caught:
                self.install()
        self.assertTrue(INSTALL._install_error_payload(caught.exception)["self_check_failed"])

        # An unrelated rollback failure (not a self-check) must report False.
        original_swap = INSTALL._atomic_swap
        calls = {"count": 0}

        def fail_cli(parent_fd, source, destination):
            calls["count"] += 1
            if calls["count"] == 4:
                raise INSTALL.InstallError("injected compatibility swap failure")
            return original_swap(parent_fd, source, destination)

        INSTALL._atomic_swap = fail_cli
        try:
            with self.assertRaises(INSTALL.InstallError) as unrelated:
                self.install()
        finally:
            INSTALL._atomic_swap = original_swap
        self.assertFalse(INSTALL._install_error_payload(unrelated.exception)["self_check_failed"])

    def test_rollback_refuses_actor_replacement(self):
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        original_swap = INSTALL._atomic_swap
        calls = {"count": 0}

        def actor_then_fail(parent_fd, source, destination):
            calls["count"] += 1
            if calls["count"] == 4:
                actor = paths.root / "BOI.app.actor"
                paths.app.rename(actor)
                paths.app.mkdir()
                (paths.app / "actor-owned").write_text("actor", encoding="utf-8")
                raise INSTALL.InstallError("injected compatibility swap failure")
            return original_swap(parent_fd, source, destination)

        INSTALL._atomic_swap = actor_then_fail
        try:
            with self.assertRaisesRegex(INSTALL.InstallError, "rollback failed"):
                self.install()
        finally:
            INSTALL._atomic_swap = original_swap
        self.assertTrue((paths.app / "actor-owned").is_file())

    def test_archive_move_failure_recovers_from_candidate(self):
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        old_hash = INSTALL._tree_sha256(paths.app)
        original_move = INSTALL._atomic_move
        failed = {"value": False}

        def fail_archive(source_fd, source, destination_fd, destination):
            if destination.name == "previous-app" and not failed["value"]:
                failed["value"] = True
                raise INSTALL.InstallError("injected archive move failure")
            return original_move(source_fd, source, destination_fd, destination)

        INSTALL._atomic_move = fail_archive
        try:
            with self.assertRaisesRegex(INSTALL.InstallError, "archive move failure"):
                self.install()
        finally:
            INSTALL._atomic_move = original_move
        self.assertEqual(INSTALL._tree_sha256(paths.app), old_hash)

    def interrupted_stage(self):
        original = self.signer.stage
        def fail_after_files(*args):
            original(*args)
            raise INSTALL.InstallError("stage interrupted after owned files")
        self.signer.stage = fail_after_files
        with self.assertRaisesRegex(INSTALL.InstallError, "stage interrupted"):
            self.install()
        paths = INSTALL.product_paths("boi", self.root)
        return paths, json.loads(paths.journal.read_text())

    def test_abandon_actual_cli_preserves_owned_staging_evidence(self):
        paths, journal = self.interrupted_stage()
        original_journal = paths.journal.read_bytes()
        candidate = Path(journal["candidate"])
        candidate_hash = INSTALL._tree_sha256(candidate)
        receipt = candidate.with_name(candidate.name + ".receipt.json")
        receipt_bytes = receipt.read_bytes()
        result = subprocess.run([sys.executable, "-I", "-B", str(SOURCE),
                                 "abandon-staging", "boi", "--root", str(self.root)],
                                capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        value = json.loads(result.stdout)
        archive = Path(value["archive_path"])
        self.assertEqual(value["action"], "abandoned-staging")
        self.assertFalse(value["published"])
        self.assertEqual((archive / paths.journal.name).read_bytes(), original_journal)
        self.assertEqual(INSTALL._tree_sha256(archive / candidate.name), candidate_hash)
        self.assertEqual((archive / receipt.name).read_bytes(), receipt_bytes)
        self.assertFalse(paths.journal.exists())
        self.assertFalse(candidate.exists())
        self.assertFalse(paths.app.exists())
        self.assertEqual(INSTALL.detect_mode("boi", self.root, self.policy, self.signer), "empty")
        self.signer = FakeSigner()
        self.install()
        self.assertTrue(paths.app.is_dir())

    def test_abandon_refuses_old_journal_and_public_actor_changes(self):
        paths, journal = self.interrupted_stage()
        saved = paths.journal.read_bytes()
        for field in ("staging_prestate", "staging_owned"):
            with self.subTest(missing=field):
                edited = dict(journal); edited.pop(field, None)
                paths.journal.write_text(json.dumps(edited))
                with self.assertRaises(INSTALL.InstallError):
                    INSTALL.abandon_staging("boi", self.root)
                self.assertTrue(Path(journal["candidate"]).exists())
                paths.journal.write_bytes(saved)
        for target in (paths.cli, paths.state, paths.helper_dir / "macos-signing.py"):
            with self.subTest(actor=target.name):
                before = target.read_bytes() if target.exists() else None
                target.write_bytes(b"operator replacement")
                with self.assertRaisesRegex(INSTALL.InstallError, "prestate"):
                    INSTALL.abandon_staging("boi", self.root)
                self.assertEqual(target.read_bytes(), b"operator replacement")
                self.assertTrue(paths.journal.exists())
                if before is None: target.unlink()
                else: target.write_bytes(before)

    def test_abandon_refuses_candidate_replacement_and_nonstaging_phase(self):
        paths, journal = self.interrupted_stage()
        candidate = Path(journal["candidate"])
        executable = candidate / "Contents/MacOS/boi"
        executable.write_bytes(b"actor candidate")
        with self.assertRaisesRegex(INSTALL.InstallError, "staging evidence"):
            INSTALL.abandon_staging("boi", self.root)
        self.assertEqual(executable.read_bytes(), b"actor candidate")
        journal["phase"] = "app-swap"
        paths.journal.write_text(json.dumps(journal))
        with self.assertRaisesRegex(INSTALL.InstallError, "staging"):
            INSTALL.abandon_staging("boi", self.root)
        self.assertTrue(paths.journal.exists())

    def test_abandon_archive_failure_keeps_journal_blocker(self):
        paths, journal = self.interrupted_stage()
        original = INSTALL._atomic_move
        def fail_move(*args):
            raise OSError("injected archive filesystem error")
        INSTALL._atomic_move = fail_move
        try:
            with self.assertRaises(INSTALL.InstallError):
                INSTALL.abandon_staging("boi", self.root)
        finally:
            INSTALL._atomic_move = original
        self.assertTrue(paths.journal.exists())
        self.assertTrue(Path(journal["candidate"]).exists())

    def test_abandon_retains_actor_archive_and_changed_existing_app(self):
        self.install()
        paths, journal = self.interrupted_stage()
        archive = self.root / (".boi.abandoned-staging-" + journal["transaction_id"])
        archive.mkdir(); (archive / "actor").write_bytes(b"keep")
        with self.assertRaises(INSTALL.InstallError):
            INSTALL.abandon_staging("boi", self.root)
        self.assertEqual((archive / "actor").read_bytes(), b"keep")
        paths.executable.write_bytes(b"operator edit inside installed app")
        with self.assertRaisesRegex(INSTALL.InstallError, "prestate"):
            INSTALL.abandon_staging("boi", self.root)
        self.assertTrue(paths.journal.exists())
        self.assertEqual(paths.executable.read_bytes(), b"operator edit inside installed app")

    def test_abandon_partial_archive_keeps_blocker_and_all_bytes(self):
        paths, journal = self.interrupted_stage()
        original = INSTALL._atomic_move
        calls = []
        def second_move_fails(*args):
            calls.append(args)
            if len(calls) == 2: raise OSError("injected second move failure")
            return original(*args)
        INSTALL._atomic_move = second_move_fails
        try:
            with self.assertRaises(INSTALL.StagingRecoveryError) as error:
                INSTALL.abandon_staging("boi", self.root)
        finally:
            INSTALL._atomic_move = original
        archive = Path(error.exception.archive)
        candidate = Path(journal["candidate"])
        self.assertTrue((archive / candidate.name / "Contents/MacOS/boi").exists())
        self.assertTrue(candidate.with_name(candidate.name + ".receipt.json").exists())
        self.assertTrue(paths.journal.exists())
        with self.assertRaises(INSTALL.InstallError):
            INSTALL.abandon_staging("boi", self.root)

    def test_abandon_checks_hex_alias_prestate(self):
        self.signer.fail_stage = True
        with self.assertRaises(INSTALL.InstallError):
            INSTALL.install("hex", self.root, self.source, self.signer, policy_path=self.policy,
                            helper_provenance=self.helpers,
                            helper_sources={name: self.root / "libexec" / name for name in self.helpers},
                            source_revision="e" * 40)
        paths = INSTALL.product_paths("hex", self.root)
        self.assertIsNotNone(paths.alias)
        paths.alias.symlink_to("actor-target")
        with self.assertRaisesRegex(INSTALL.InstallError, "prestate"):
            INSTALL.abandon_staging("hex", self.root)
        self.assertEqual(os.readlink(paths.alias), "actor-target")
        self.assertTrue(paths.journal.exists())

    def test_staging_evidence_write_failure_does_not_claim_publication(self):
        original = INSTALL._write_journal
        def fail_owned_record(paths, journal):
            if "staging_owned" in journal:
                raise OSError("injected evidence write failure")
            return original(paths, journal)
        self.signer.fail_stage = True
        INSTALL._write_journal = fail_owned_record
        try:
            with self.assertRaisesRegex(INSTALL.InstallError, "evidence write failure") as error:
                self.install()
        finally:
            INSTALL._write_journal = original
        self.assertFalse(error.exception.published)
        with self.assertRaises(INSTALL.InstallError):
            INSTALL.abandon_staging("boi", self.root)
        self.assertTrue(INSTALL.product_paths("boi", self.root).journal.exists())

    def test_stage_failure_leaves_journal_and_no_public_app(self):
        self.signer.fail_stage = True
        with self.assertRaises(INSTALL.InstallError):
            self.install()
        paths = INSTALL.product_paths("boi", self.root)
        self.assertTrue(paths.journal.is_file())
        self.assertFalse(paths.app.exists())

    def test_actor_change_during_staging_is_not_overwritten(self):
        paths = INSTALL.product_paths("boi", self.root)
        paths.cli.parent.mkdir()
        paths.cli.write_bytes(b"operator replacement")
        original_stage = self.signer.stage

        def stage_and_replace(*args):
            result = original_stage(*args)
            paths.cli.write_bytes(b"actor replacement")
            return result

        self.signer.stage = stage_and_replace
        with self.assertRaisesRegex(INSTALL.InstallError, "changed during staging"):
            INSTALL.install("boi", self.root, self.source, self.signer, policy_path=self.policy, helper_provenance=self.helpers, helper_sources={name: self.root / "libexec" / name for name in self.helpers}, source_revision="e" * 40)
        self.assertEqual(paths.cli.read_bytes(), b"actor replacement")

    def test_missing_policy_blocks_signed_install(self):
        paths = INSTALL.product_paths("boi", self.root)
        paths.app.mkdir()
        with self.assertRaisesRegex(INSTALL.InstallError, "policy"):
            INSTALL.install("boi", self.root, self.source, self.signer, policy_path=self.policy.with_name("missing.json"), helper_provenance=self.helpers, source_revision="e" * 40)

    def test_service_owner_requires_inherited_lock(self):
        with self.assertRaisesRegex(INSTALL.InstallError, "inherited product lock"):
            INSTALL.service_owner("boi", self.root, self.signer, policy_path=self.policy)

    def test_service_owner_returns_signed_current_and_checks_helpers(self):
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        fd = paths.lock.open("a+")
        try:
            import fcntl
            fcntl.flock(fd.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            owner = INSTALL.service_owner("boi", self.root, self.signer, policy_path=self.policy, lock_held=True)
        finally:
            fd.close()
        self.assertEqual(owner["mode"], "signed-current")
        self.assertTrue(owner["policy_available"])
        self.assertEqual(owner["source_revision"], "e" * 40)

    def test_service_owner_rejects_changed_helper(self):
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        helper = paths.root / "libexec" / "macos-app-install.py"
        helper.write_text("changed", encoding="utf-8")
        fd = paths.lock.open("a+")
        try:
            import fcntl
            fcntl.flock(fd.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(INSTALL.InstallError, "helper"):
                INSTALL.service_owner("boi", self.root, self.signer, policy_path=self.policy, lock_held=True)
        finally:
            fd.close()

    def test_helper_source_hash_mismatch_fails_before_publication(self):
        bad = dict(self.helpers)
        bad["macos-app-install.py"] = {"sha256": "0" * 64, "source_revision": "f" * 40}
        with self.assertRaisesRegex(INSTALL.InstallError, "changed during staging"):
            INSTALL.install("boi", self.root, self.source, self.signer, policy_path=self.policy, helper_provenance=bad, helper_sources={name: self.root / "libexec" / name for name in bad}, source_revision="e" * 40)

    def test_hex_agent_alias_publishes_and_points_to_hex(self):
        root = Path(self.temp.name) / ".hex"
        root.mkdir()
        (root / "libexec").mkdir()
        helpers = {}
        sources = {}
        for name in ("macos-signing.py", "macos-app-install.py"):
            path = root / "libexec" / name
            path.write_text(name, encoding="utf-8")
            helpers[name] = {"sha256": INSTALL._sha256(path), "source_revision": "f" * 40}
            sources[name] = path
        result = INSTALL.install("hex", root, self.source, self.signer, policy_path=self.policy, helper_provenance=helpers, helper_sources=sources, source_revision="e" * 40)
        paths = INSTALL.product_paths("hex", root)
        self.assertTrue(paths.alias.is_symlink())
        self.assertEqual(os.readlink(paths.alias), os.readlink(paths.cli))
        self.assertEqual(result["product"], "hex")

    def test_legacy_agent_alias_is_not_orphan_signed_evidence(self):
        root = self.root / "legacy-hex"
        paths = INSTALL.product_paths("hex", root)
        paths.cli.parent.mkdir(parents=True)
        paths.cli.write_bytes(b"legacy executable")
        paths.alias.symlink_to("hex")
        missing_policy = self.policy.with_name("missing.json")
        self.assertEqual(INSTALL.detect_mode("hex", root, missing_policy, self.signer), "legacy-raw")
        self.assertEqual(INSTALL.detect_mode("hex", root, self.policy, self.signer), "configured-legacy")

    def test_orphan_signed_agent_alias_blocks_legacy_and_empty(self):
        root = self.root / "orphan-hex"
        paths = INSTALL.product_paths("hex", root)
        paths.cli.parent.mkdir(parents=True)
        for raw_present in (False, True):
            if raw_present:
                paths.cli.write_bytes(b"legacy executable")
            for destination in ("../Hex.app/Contents/MacOS/hex", str(paths.executable)):
                paths.alias.symlink_to(destination)
                for policy in (self.policy, self.policy.with_name("missing.json")):
                    with self.assertRaisesRegex(INSTALL.InstallError, "orphan signed"):
                        INSTALL.detect_mode("hex", root, policy, self.signer)
                paths.alias.unlink()

    def test_verifier_version_missing_or_different_never_publishes(self):
        original = self.signer.verify_installed
        for value in (None, "2.0.0"):
            def verify(*args, **kwargs):
                result = original(*args, **kwargs)
                result.pop("version", None)
                if value is not None:
                    result["version"] = value
                return result
            self.signer.verify_installed = verify
            with self.assertRaisesRegex(INSTALL.InstallError, "version"):
                self.install()
            self.assertFalse(INSTALL.product_paths("boi", self.root).app.exists())
            # Each independent attempt gets a fresh root, not an erased journal.
            self.root = self.root.with_name(self.root.name + "-next")
            (self.root / "libexec").mkdir(parents=True)
            for name in self.helpers:
                (self.root / "libexec" / name).write_text(name)

    def test_mutation_directories_synced_before_committed_journal(self):
        original_sync, original_journal = INSTALL._fsync_dir, INSTALL._write_journal
        synced = set()
        def sync(fd):
            original_sync(fd)
            info = os.fstat(fd)
            synced.add((info.st_dev, info.st_ino))
        def journal(paths, value):
            if value["phase"] == "committed":
                for directory in (paths.root, paths.cli.parent, paths.root / "libexec", Path(value["rollback"])):
                    info = directory.stat()
                    self.assertIn((info.st_dev, info.st_ino), synced)
            original_journal(paths, value)
        INSTALL._fsync_dir, INSTALL._write_journal = sync, journal
        try:
            self.install()
        finally:
            INSTALL._fsync_dir, INSTALL._write_journal = original_sync, original_journal

    def test_directory_sync_failure_never_marks_committed(self):
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        old_bytes = paths.executable.read_bytes()
        self.source.write_bytes(b"new bytes must not commit")
        original = INSTALL._fsync_dir
        def fail(fd):
            raise OSError("injected directory sync error")
        INSTALL._fsync_dir = fail
        try:
            with self.assertRaisesRegex(INSTALL.InstallError, "directory sync error"):
                self.install()
        finally:
            INSTALL._fsync_dir = original
        # Rollback restores public state cleanly here, so the journal is
        # cleared (2026-09-11 fix) instead of being left behind at whatever
        # pre-committed phase it last reached -- either way, "committed" is
        # never written to disk.
        self.assertFalse(paths.journal.exists())
        self.assertEqual(paths.executable.read_bytes(), old_bytes)

    def test_inherited_lock_requires_held_lock_and_expected_inode(self):
        self.install()
        paths = INSTALL.product_paths("boi", self.root)
        unlocked = paths.lock.open("a+")
        try:
            with self.assertRaisesRegex(INSTALL.InstallError, "does not hold"):
                INSTALL._validate_lock_fd(unlocked.fileno(), paths)
        finally:
            unlocked.close()
        held = paths.lock.open("a+")
        import fcntl
        fcntl.flock(held.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        try:
            INSTALL._validate_lock_fd(held.fileno(), paths)
        finally:
            fcntl.flock(held.fileno(), fcntl.LOCK_UN)
            held.close()


@unittest.skipUnless(sys.platform == "darwin", "requires macOS renameatx_np")
class MacAppInstallCLITests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        base = Path(self.temp.name)
        self.root = base / ".boi"
        self.root.mkdir()
        self.home = base
        self.policy = base / "Library/Application Support/Hex/build-signing/policy.json"
        self.policy.parent.mkdir(parents=True)
        self.policy.write_text(json.dumps({"schema_version": 1, "certificate_sha1": "A" * 40, "team_id": "TEAM123456"}), encoding="utf-8")
        self.source = base / "source"
        # This flows straight through the fake signing CLI into the published
        # bin/<product> symlink target below, and the real install() now runs that
        # published binary as a post-publish self-check -- so it must actually be
        # able to execute, unlike the in-process FakeSigner fixtures above.
        self.source.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.source.chmod(0o755)
        self.script = base / "macos-app-install.py"
        shutil.copy2(SOURCE, self.script)
        helper = base / "macos-signing.py"
        # Keep the exact accepted policy reader. Only the CLI crypto result is fake.
        shared = SOURCE.with_name("macos-signing.py").read_text().split('if __name__ == "__main__":')[0]
        fake_cli = "import json, pathlib, shutil, sys\nargs=sys.argv[1:]\nif args[0] == 'verify-installed':\n    bundle=pathlib.Path(args[1]); product=args[2]\n    print(json.dumps({'identifier':'com.mrap.boi','version':'1.0.0','team_id':'TEAM123456','certificate_sha1':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','designated_requirements':{'arm64':'anchor apple generic'},'mach_o_uuids':{'arm64':'11111111-1111-1111-1111-111111111111'}}))\nelse:\n    source=pathlib.Path(args[0]); candidate=pathlib.Path(args[3]); candidate.joinpath('Contents/MacOS').mkdir(parents=True); shutil.copy2(source,candidate/'Contents/MacOS/boi'); (candidate/'Contents/Info.plist').write_bytes(b'plist'); pathlib.Path(args[args.index('--receipt')+1]).write_text(json.dumps({'identifier':'com.mrap.boi','version':'1.0.0','team_id':'TEAM123456','certificate_sha1':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','designated_requirements':{'arm64':'anchor apple generic'},'mach_o_uuids':{'arm64':'11111111-1111-1111-1111-111111111111'}}))\n"
        helper.write_text(shared + "\nif __name__ == '__main__':\n" + "\n".join("    " + line for line in fake_cli.splitlines()) + "\n")
        self.helper = helper
        self.addCleanup(self.temp.cleanup)

    def run_cli(self, *args):
        return subprocess.run([sys.executable, "-I", "-B", str(self.script), *args], capture_output=True, text=True, check=False, timeout=10, env={"HOME": str(self.home), "PATH": "/usr/bin:/bin"})

    def test_signer_runner_kills_timed_out_process_group(self):
        helper = Path(self.temp.name) / "slow-signing.py"
        helper.write_text("import time; time.sleep(2)", encoding="utf-8")
        with self.assertRaisesRegex(INSTALL.InstallError, "timed out"):
            run_owned(helper, ["verify-installed", "bundle", "boi"], timeout=0.05)

    def test_cli_rejects_noncentral_policy_before_mutation(self):
        other = self.home / "other-policy.json"
        shutil.copy2(self.policy, other)
        result = self.run_cli("preflight", "boi", "--root", str(self.root), "--policy", str(other))
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("central machine path", result.stderr)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_public_install_and_preflight(self):
        install = self.run_cli("install", "boi", "--root", str(self.root), "--source", str(self.source), "--version", "1.0.0", "--source-revision", "e" * 40, "--helper-source-revision", "f" * 40, "--policy", str(self.policy))
        self.assertEqual(install.returncode, 0, install.stderr)
        installed = json.loads(install.stdout)
        self.assertEqual(installed["schema_version"], 1)
        self.assertEqual(installed["product"], "boi")
        self.assertEqual(installed["mode"], "signed-current")
        self.assertEqual(installed["source_revision"], "e" * 40)
        self.assertEqual(installed["version"], "1.0.0")
        self.assertTrue(installed["generation"])
        self.assertEqual(set(installed["helpers"]), {"macos-signing.py", "macos-app-install.py"})
        self.assertTrue(all(set(value) == {"sha256", "source_revision"} for value in installed["helpers"].values()))
        result = self.run_cli("preflight", "boi", "--root", str(self.root), "--policy", str(self.policy))
        self.assertEqual(result.returncode, 0, result.stderr)
        preflight = json.loads(result.stdout)
        self.assertEqual(preflight["schema_version"], 1)
        self.assertEqual(preflight["source_revision"], "e" * 40)
        self.assertEqual(preflight["service_owner"]["mode"], "signed-current")


    def _install_guardian_fixture(self):
        result = self.run_cli('install', 'boi', '--root', str(self.root), '--source', str(self.source),
                              '--version', '1.0.0', '--source-revision', 'e' * 40,
                              '--helper-source-revision', 'f' * 40)
        self.assertEqual(result.returncode, 0, result.stderr)

    def _refresh_fixture_provenance(self):
        # These are explicit fake-crypto fixtures, not a claim about real trust.
        state_path = self.root / 'BOI.app.install-state.json'
        state = json.loads(state_path.read_text())
        for name in state['helpers']:
            state['helpers'][name]['sha256'] = INSTALL._sha256(self.root / 'libexec' / name)
        state_path.write_text(json.dumps(state))

    def test_actual_cli_parent_sigkill_keeps_lock_until_work_is_gone(self):
        import fcntl, signal, socket, time
        self._install_guardian_fixture()
        listener_path = Path('/private/tmp') / ('app-kill-' + INSTALL.secrets.token_hex(10))
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(listener_path)); listener.listen(1); listener.settimeout(5)
        helper = self.root / 'libexec/macos-signing.py'
        shared = helper.read_text().split("if __name__ == '__main__':")[0]
        helper.write_text(shared + "\nif __name__ == '__main__':\n" +
            " import socket,os,json\n c=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);c.settimeout(8)\n" +
            " c.connect(" + repr(str(listener_path)) + ")\n" +
            " c.sendall((json.dumps({'pid':os.getpid(),'pgid':os.getpgrp()})+'\\n').encode())\n" +
            " if c.recv(32)==b'after-parent-death': c.sendall(b'still-executing')\n")
        self._refresh_fixture_provenance()
        outer = subprocess.Popen([sys.executable, '-I', '-B', str(self.root / 'libexec/macos-app-install.py'),
                                  'preflight', 'boi', '--root', str(self.root)],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
                                 env={'HOME':str(self.home), 'PATH':'/usr/bin:/bin'})
        try:
            connection, _ = listener.accept()
            with connection:
                connection.settimeout(3)
                with connection.makefile('rb') as stream:
                    identity = json.loads(stream.readline())
                self.assertNotEqual(identity['pgid'], outer.pid)
                paths = INSTALL.product_paths('boi', self.root)
                with paths.lock.open('r+') as contender:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    os.killpg(outer.pid, signal.SIGKILL)
                    outer.communicate(timeout=8)
                    self.assertEqual(outer.returncode, -signal.SIGKILL)
                    deadline = time.monotonic() + 3
                    while True:
                        try:
                            fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            break
                        except BlockingIOError:
                            self.assertLess(time.monotonic(), deadline, 'guardian did not release after cleanup')
                            time.sleep(.01)
                    try:
                        connection.sendall(b'after-parent-death')
                        reply = connection.recv(128)
                    except (BrokenPipeError, ConnectionResetError):
                        reply = b''
                    self.assertEqual(reply, b'', 'work executed after cleanup released the lock')
                self.assertFalse(INSTALL._failure_path(paths).exists())
                self.assertFalse(INSTALL._control_directory(paths).exists())
        finally:
            listener.close(); listener_path.unlink()
            if outer.poll() is None:
                os.killpg(outer.pid, signal.SIGKILL); outer.communicate(timeout=8)

    def test_live_failure_recovery_preserves_lock_even_when_receipt_storage_fails(self):
        import fcntl
        self._install_guardian_fixture()
        script = self.root / 'libexec/macos-app-install.py'
        original = script.read_text()
        injection = """
_real_cleanup = _cleanup_work
_cleanup_calls = 0
def _cleanup_work(*args, **kwargs):
    global _cleanup_calls
    _cleanup_calls += 1
    if _cleanup_calls == 1:
        raise PermissionError('injected actual guardian cleanup failure')
    return _real_cleanup(*args, **kwargs)
"""
        paths = INSTALL.product_paths('boi', self.root)
        for storage_fails in (False, True):
            with self.subTest(storage_fails=storage_fails):
                fault = injection
                if storage_fails:
                    fault += "\ndef _store_failure(*args):\n    raise OSError('injected receipt storage failure')\n"
                script.write_text(original.replace('# Rust service bootstraps', fault + '\n# Rust service bootstraps'))
                self._refresh_fixture_provenance()
                outer = subprocess.run([sys.executable, '-I', '-B', str(script), 'preflight', 'boi', '--root', str(self.root)],
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=10,
                                       env={'HOME':str(self.home), 'PATH':'/usr/bin:/bin'})
                try:
                    self.assertEqual(outer.returncode, 1, outer.stdout)
                    self.assertIn('cleanup incomplete', outer.stderr)
                    # Parent has exited. The independently located live guardian
                    # still owns the same flock and the retained work anchor.
                    with self.assertRaisesRegex(INSTALL.InstallError, 'busy'):
                        with INSTALL._product_lock(paths): pass
                    status = INSTALL.cleanup_operation('boi', self.root, 'status')
                    self.assertEqual(status['status'], 'blocked')
                    self.assertFalse(status['anchor_reaped'])
                    if storage_fails:
                        self.assertIn('storage failure', status['receipt_error'])
                    else:
                        self.assertEqual(INSTALL._read_json(INSTALL._failure_path(paths), 'test')['token'], status['token'])
                    recovered = INSTALL.cleanup_operation('boi', self.root, 'retry')
                    self.assertEqual(recovered['status'], 'clean')
                    self.assertTrue(recovered['anchor_reaped'])
                    with INSTALL._product_lock(paths): pass
                finally:
                    if INSTALL._control_directory(paths).exists():
                        INSTALL.cleanup_operation('boi', self.root, 'retry')


class FinalBoundaryTests(unittest.TestCase):
    @unittest.skipUnless(sys.platform == "darwin", "actual staging rename is macOS-specific")
    def test_process_signer_stage_uses_actual_cli_receipt_protocol(self):
        import inspect
        fixture_spec = importlib.util.spec_from_file_location("signer_command_fixture", Path(__file__).with_name("test_macos_signing.py"))
        commands = importlib.util.module_from_spec(fixture_spec)
        sys.modules[fixture_spec.name] = commands
        fixture_spec.loader.exec_module(commands)
        with tempfile.TemporaryDirectory() as td:
            base = Path(td).resolve()
            helper = base / "macos-signing.py"
            # Retain the actual signer CLI and staging implementation. Inject
            # only its existing Apple-command seam, never fake stdout/receipt.
            source = SOURCE.with_name("macos-signing.py").read_text()
            injection = ("import sys\nSIGN=sys.modules[__name__]\n"
                         + "LEAF=" + repr(commands.LEAF) + "\n"
                         + "FINGERPRINT=" + repr(commands.FINGERPRINT) + "\n"
                         + "TEAM=" + repr(commands.TEAM) + "\n"
                         + "UUIDS=" + repr(commands.UUIDS) + "\n"
                         + inspect.getsource(commands.Runner)
                         + "\n_real_stage=stage\ndef stage(*args,**kwargs):\n    kwargs['run']=Runner()\n    return _real_stage(*args,**kwargs)\n")
            helper.write_text(source.replace('if __name__ == "__main__":', injection + '\nif __name__ == "__main__":'))
            artifact = base / "artifact"
            artifact.write_bytes(b"mock Apple-command-boundary executable")
            policy = base / "policy.json"
            policy.write_text(json.dumps({"schema_version": 1, "certificate_sha1": commands.FINGERPRINT, "team_id": commands.TEAM}))
            standalone_receipt = base / "standalone.json"
            standalone = subprocess.run([sys.executable, "-Werror", "-I", "-B", str(helper), str(artifact), "hex", str(policy), str(base / "Standalone.app"), "--version", "0.52.2", "--receipt", str(standalone_receipt)], capture_output=True, text=True, timeout=10)
            self.assertEqual(standalone.returncode, 0, standalone.stderr)
            self.assertEqual(standalone.stdout, "")
            self.assertEqual(json.loads(standalone_receipt.read_text())["identifier"], "com.mrap.hex")
            paths = INSTALL.product_paths("hex", base / "owner")
            receipt = base / "adapter.json"
            with INSTALL._product_lock(paths) as fd:
                runner = INSTALL.ProcessSigner(helper)
                runner.bind_owner(paths, fd)
                result = runner.stage(artifact, "hex", policy, base / "Adapter.app", receipt, version="0.52.2")
            self.assertEqual(result, json.loads(receipt.read_text()))
            self.assertEqual(result["identifier"], "com.mrap.hex")
            self.assertEqual(result["version"], "0.52.2")
            self.assertEqual((base / "Adapter.app/Contents/MacOS/hex").read_bytes(), artifact.read_bytes())

    def test_receipt_transport_rejects_untrusted_or_failed_results(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as td:
            base = Path(td)
            helper = base / "signer.py"
            helper.write_text("# transport fixture; no commands execute\n")
            runner = INSTALL.ProcessSigner(helper)
            for defect in ("preexisting", "missing", "symlink", "oversize", "nonobject", "stdout", "failed"):
                with self.subTest(defect=defect):
                    receipt = base / (defect + ".json")
                    if defect == "preexisting": receipt.write_text("{}")
                    def transport(*args):
                        if defect == "symlink": receipt.symlink_to(helper)
                        elif defect == "oversize": receipt.write_bytes(b" " * (INSTALL.MAX_JSON_BYTES + 1))
                        elif defect == "nonobject": receipt.write_text("[]")
                        elif defect not in ("missing", "preexisting"): receipt.write_text("{}")
                        return (7 if defect == "failed" else 0, b"{}" if defect == "stdout" else b"", b"fixture failure")
                    with patch.object(INSTALL, "_run_guardian", side_effect=transport) as run:
                        with self.assertRaises(INSTALL.InstallError):
                            runner.stage(base / "source", "hex", base / "policy", base / "candidate", receipt)
                    if defect == "preexisting": run.assert_not_called()
                    else: run.assert_called_once()
                    self.assertEqual(helper.read_text(), "# transport fixture; no commands execute\n")

    def test_no_missing_shared_reader_fallback(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as td:
            policy = Path(td) / "policy.json"
            policy.write_text(json.dumps({"schema_version": 1, "certificate_sha1": "A" * 40, "team_id": "TEAM123456"}))
            retained = dict(INSTALL._RETAINED_SOURCES, **{'macos-signing.py': b'pass\n'})
            with patch.object(INSTALL, '_RETAINED_SOURCES', retained):
                with self.assertRaisesRegex(INSTALL.InstallError, 'accepted signer policy reader is unavailable'):
                    INSTALL._policy_mode(policy)

    def test_actual_shared_policy_reader_rejects_bool_unknown_and_duplicate(self):
        with tempfile.TemporaryDirectory() as td:
            policy = Path(td) / "policy.json"
            valid = {"schema_version": 1, "certificate_sha1": "A" * 40, "team_id": "TEAM123456"}
            policy.write_text(json.dumps(valid))
            self.assertEqual(INSTALL._policy_mode(policy), "configured")
            payloads = [json.dumps(dict(valid, schema_version=True)), json.dumps(dict(valid, unknown=True)), '{"schema_version":1,' + json.dumps(valid)[1:]]
            for payload in payloads:
                policy.write_text(payload)
                with self.assertRaises(INSTALL.InstallError):
                    INSTALL._policy_mode(policy)

    def test_helper_limit_matches_service_consumer(self):
        with tempfile.TemporaryDirectory() as td:
            helper = Path(td) / "helper.py"
            helper.write_bytes(b"x" * (1024 * 1024 + 1))
            with self.assertRaisesRegex(INSTALL.InstallError, "too large"):
                INSTALL._read_helper_source(helper)

    def test_runner_drains_pipes_with_no_spooled_files(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as td:
            helper = Path(td) / "large.py"
            helper.write_text("import sys\nsys.stdout.buffer.write(b'x' * 1048576); sys.stdout.flush()\n")
            with patch.object(tempfile, "TemporaryFile", side_effect=AssertionError("output must not spool to disk")):
                with self.assertRaisesRegex(INSTALL.InstallError, "output is too large"):
                    run_owned(helper, timeout=2)

    def test_reaped_group_transient_permission_failure_requires_absence(self):
        import errno
        from types import SimpleNamespace
        from unittest.mock import patch, call
        process = SimpleNamespace(pid=12345, returncode=-9)
        with patch.object(INSTALL.os, "killpg", side_effect=[
                PermissionError(errno.EPERM, "fixture permission"),
                ProcessLookupError(errno.ESRCH, "fixture absent")]) as observe:
            self.assertIsNone(INSTALL._cleanup_work(process, (), 0.2))
        self.assertEqual(observe.call_args_list, [call(12345, 0), call(12345, 0)])

    def test_reaped_group_permanent_permission_failure_never_claims_clean(self):
        import errno
        import time
        from types import SimpleNamespace
        from unittest.mock import patch
        process = SimpleNamespace(pid=12345, returncode=-9)
        start = time.monotonic()
        with patch.object(INSTALL.os, "killpg", side_effect=PermissionError(errno.EPERM, "fixture permission")) as observe:
            with self.assertRaisesRegex(INSTALL.InstallError, "group remains present"):
                INSTALL._cleanup_work(process, (), 0.03)
        self.assertGreaterEqual(time.monotonic() - start, 0.03)
        self.assertGreaterEqual(observe.call_count, 2)
        self.assertTrue(all(value.args == (12345, 0) for value in observe.call_args_list))

    def test_reaped_group_unknown_observation_error_is_not_reclassified(self):
        import errno
        from types import SimpleNamespace
        from unittest.mock import patch
        process = SimpleNamespace(pid=12345, returncode=-9)
        with patch.object(INSTALL.os, "killpg", side_effect=OSError(errno.EIO, "fixture unexpected")) as observe:
            with self.assertRaises(OSError) as error:
                INSTALL._cleanup_work(process, (), 0.2)
        self.assertEqual(error.exception.errno, errno.EIO)
        observe.assert_called_once_with(12345, 0)

    def _assert_pipe_holding_child_cleaned(self, name, cancellation):
        import fcntl
        import signal
        base = Path(tempfile.mkdtemp(prefix=f"hex-final-boundary-{name}-")).resolve()
        evidence_lock = base / "child.lock"
        release_lock = base / "release.lock"
        helper = base / f"{name}.py"
        gate = None
        gate_locked = False
        primary = None
        complete = False
        try:
            base.chmod(0o700)
            release_lock.touch(mode=0o600)
            gate = release_lock.open("r+")
            fcntl.flock(gate, fcntl.LOCK_EX)
            gate_locked = True
            after_readiness = (
                "os.kill(" + str(os.getpid()) + ",signal.SIGINT)\n"
                "g=open(" + repr(str(release_lock)) + ",'r+')\n"
                "fcntl.flock(g,fcntl.LOCK_EX)\n"
                if cancellation
                else "print('{}',flush=True)\n"
            )
            helper.write_text(
                "import os,fcntl,signal\n"
                "r,w=os.pipe()\n"
                "pid=os.fork()\n"
                "if pid == 0:\n"
                " os.close(r)\n"
                " f=open(" + repr(str(evidence_lock)) + ",'w')\n"
                " fcntl.flock(f,fcntl.LOCK_EX)\n"
                " os.write(w,b'1');os.close(w)\n"
                " g=open(" + repr(str(release_lock)) + ",'r+')\n"
                " fcntl.flock(g,fcntl.LOCK_EX)\n"
                " os._exit(0)\n"
                "os.close(w)\n"
                "ready=os.read(r,1);os.close(r)\n"
                "if ready != b'1': raise RuntimeError('[FINAL-BOUNDARY-SETUP] child readiness failed')\n"
                + after_readiness,
                encoding="utf-8",
            )
            if cancellation:
                previous = signal.signal(signal.SIGINT, signal.default_int_handler)
                operation_primary = None
                try:
                    with self.assertRaises(KeyboardInterrupt):
                        run_owned(helper)
                except BaseException as error:
                    operation_primary = error
                    raise
                finally:
                    try:
                        signal.signal(signal.SIGINT, previous)
                    except BaseException as error:
                        if operation_primary is None:
                            raise
                        print(
                            f"secondary signal restoration failure: {error!r}",
                            file=sys.stderr,
                        )
            else:
                self.assertEqual(run_owned(helper), {})
            with evidence_lock.open("r+") as stream:
                try:
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError as error:
                    raise AssertionError(
                        "[FINAL-BOUNDARY-CLEANUP] child still holds evidence lock"
                    ) from error
            complete = True
        except BaseException as error:
            primary = error
            raise
        finally:
            secondary = []
            if gate_locked:
                try:
                    fcntl.flock(gate, fcntl.LOCK_UN)
                except BaseException as error:
                    secondary.append(error)
            if gate is not None:
                try:
                    gate.close()
                except BaseException as error:
                    secondary.append(error)
            if primary is not None or secondary or not complete:
                print(f"final-boundary fixture retained: {base}", file=sys.stderr)
            if primary is not None:
                for error in secondary:
                    print(
                        f"secondary fixture gate release failure: {error!r}",
                        file=sys.stderr,
                    )
            elif secondary:
                for error in secondary[1:]:
                    print(
                        f"secondary fixture gate release failure: {error!r}",
                        file=sys.stderr,
                    )
                raise secondary[0]
            elif complete:
                try:
                    shutil.rmtree(base)
                except BaseException:
                    print(f"final-boundary fixture retained: {base}", file=sys.stderr)
                    raise

    def test_normal_leader_exit_does_not_leave_held_pipe_descendant(self):
        self._assert_pipe_holding_child_cleaned("normal", cancellation=False)


    def test_cancellation_cleans_actual_pipe_holding_child(self):
        self._assert_pipe_holding_child_cleaned("cancellation", cancellation=True)

    def test_stderr_bound_and_secret_environment_removal(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as td:
            helper = Path(td) / "environment.py"
            helper.write_text("import json,os\nprint(json.dumps({'leaked': 'INSTALL_TEST_SECRET' in os.environ}))\n")
            with patch.dict(os.environ, {"INSTALL_TEST_SECRET": "fixture-only-not-a-secret"}):
                self.assertEqual(run_owned(helper), {"leaked": False})
            helper.write_text("import sys\nsys.stderr.buffer.write(b'x' * 1048576);sys.stderr.flush()\n")
            with self.assertRaisesRegex(INSTALL.InstallError, "output is too large"):
                run_owned(helper, timeout=2)



class GuardianBoundaryTests(unittest.TestCase):
    def test_clean_quarantine_exits_when_recovery_client_disconnects(self):
        from types import SimpleNamespace
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as td:
            directory = Path(td) / "control"
            directory.mkdir()
            (directory / "control.sock").write_bytes(b"fixture endpoint")
            paths = INSTALL.product_paths("boi", Path(td) / ".boi")
            process = SimpleNamespace(pid=123, returncode=None)
            closed = []
            records = []
            test = self

            class Connection:
                def __enter__(self): return self
                def __exit__(self, *args): return False
                def sendall(self, data):
                    test.assertEqual(json.loads(data)["status"], "clean")
                    test.assertTrue(server.closed)
                    test.assertFalse(directory.exists())
                    test.assertIn(991, closed)
                    raise BrokenPipeError("recovery client disconnected")

            class Server:
                closed = False
                accepts = 0
                def accept(self):
                    self.accepts += 1
                    test.assertEqual(self.accepts, 1, "must not accept after cleanup closes server")
                    return Connection(), None
                def close(self): self.closed = True

            server = Server()
            def cleanup(*args): process.returncode = 0
            with patch.object(INSTALL, "_cleanup_work", side_effect=cleanup) as clean, \
                    patch.object(INSTALL, "_record_failure", side_effect=lambda paths, value: records.append(dict(value))), \
                    patch.object(INSTALL, "_receive_control", return_value={"token": "token", "command": "retry"}), \
                    patch.object(INSTALL.os, "write"), \
                    patch.object(INSTALL.os, "close", side_effect=closed.append):
                INSTALL._quarantine(paths, process, (), 992, OSError("initial cleanup failure"), 1, (server, directory, "token"), 991)
            clean.assert_called_once_with(process, (), 1)
            self.assertEqual([record["status"] for record in records], ["blocked", "clean"])
            self.assertEqual(closed.count(991), 1)
            self.assertEqual(server.accepts, 1)

    def test_retained_sources_survive_actual_disk_replacement(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            helper = root / 'signer.py'
            helper.write_text('import json\nprint(json.dumps(dict(original=True)))\n')
            runner = INSTALL.ProcessSigner(helper)
            paths = INSTALL.product_paths('boi', root)
            helper.write_text('raise RuntimeError("replacement executed")\n')
            # The installer and signer have both been pinned before replacement.
            with INSTALL._product_lock(paths) as fd:
                runner.bind_owner(paths, fd)
                with patch.object(INSTALL, '_read_helper_source', side_effect=AssertionError('fresh source trust')):
                    self.assertEqual(runner._run([]), {'original': True})

    def test_both_initial_source_snapshots_survive_real_replacement(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            installer = root / 'macos-app-install.py'
            signer = root / 'macos-signing.py'
            installer.write_bytes(SOURCE.read_bytes())
            signer.write_text('import json\nprint(json.dumps(dict(pinned=True)))\n')
            spec = importlib.util.spec_from_file_location('pinned_installer_fixture', installer)
            module = importlib.util.module_from_spec(spec)
            sys.modules[spec.name] = module
            try:
                spec.loader.exec_module(module)
                installer.write_text('raise RuntimeError("changed installer executed")\n')
                signer.write_text('raise RuntimeError("changed signer executed")\n')
                paths = module.product_paths('boi', root)
                with module._product_lock(paths) as fd:
                    runner = module.ProcessSigner()
                    runner.bind_owner(paths, fd)
                    self.assertEqual(runner._run([]), {'pinned': True})
            finally:
                del sys.modules[spec.name]

    def test_failed_signal_with_live_pipe_retains_unreaped_owner(self):
        import errno
        from unittest.mock import patch
        process = subprocess.Popen([sys.executable, '-I', '-B', '-c', 'import time; time.sleep(5)'],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        try:
            with patch.object(INSTALL.os, 'killpg', side_effect=PermissionError(errno.EPERM, 'fixture denied')):
                with self.assertRaisesRegex(INSTALL.InstallError, 'signal error:.*fixture denied'):
                    INSTALL._cleanup_work(process, (process.stdout, process.stderr), .05)
            self.assertIsNone(process.returncode, 'failed cleanup reaped a living owner')
        finally:
            INSTALL._cleanup_work(process, (process.stdout, process.stderr), 2)
            process.stdout.close(); process.stderr.close()

    def test_internal_source_binding_rejects_missing_and_oversized(self):
        source = INSTALL._RETAINED_SOURCES['macos-app-install.py']
        for value in ({}, {'macos-app-install.py': source, 'macos-signing.py': b'x' * (INSTALL.MAX_HELPER_BYTES + 1)}):
            import types
            module = types.ModuleType('bad_retained')
            module.__file__ = str(SOURCE)
            module.contents = value
            sys.modules[module.__name__] = module
            try:
                with self.assertRaisesRegex(RuntimeError, 'invalid retained'):
                    exec(compile(source, str(SOURCE), 'exec'), module.__dict__)
            finally:
                del sys.modules[module.__name__]

    def test_early_unexpected_exit_is_reaped_without_quarantine(self):
        with tempfile.TemporaryDirectory() as td:
            helper = Path(td) / 'exit.py'
            helper.write_text('import os\nos._exit(17)\n')
            with self.assertRaisesRegex(INSTALL.InstallError, 'completion frame missing'):
                run_owned(helper, timeout=2)
            paths = INSTALL.product_paths('boi', helper.parent / 'runner-owner')
            self.assertFalse(INSTALL._failure_path(paths).exists())
            self.assertFalse(INSTALL._control_directory(paths).exists())
            with INSTALL._product_lock(paths):
                pass

    def test_cleanup_after_reap_is_read_only_and_esrch_still_reaps(self):
        import errno
        from unittest.mock import Mock, patch
        process = Mock(pid=123, returncode=None)
        with patch.object(INSTALL.os, 'killpg', side_effect=ProcessLookupError(errno.ESRCH, 'gone')) as signal_call:
            INSTALL._cleanup_work(process, (), .1)
        process.wait.assert_called_once()
        self.assertEqual(signal_call.call_args_list[0].args, (123, INSTALL.signal.SIGKILL))
        self.assertEqual(signal_call.call_args_list[1].args, (123, 0))
        process = Mock(pid=123, returncode=0)
        with patch.object(INSTALL.os, 'killpg', side_effect=ProcessLookupError(errno.ESRCH, 'gone')) as signal_call:
            INSTALL._cleanup_work(process, (), .1)
        self.assertEqual([call.args for call in signal_call.call_args_list], [(123, 0)])
        process.wait.assert_not_called()

    def test_no_signer_execution_before_ready_go_handshake(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as td:
            helper = Path(td) / 'startup.py'
            marker = Path(td) / 'executed'
            helper.write_text('from pathlib import Path\nPath(' + repr(str(marker)) + ').write_text("executed")\nprint("{}")\n')
            original = INSTALL.os.write
            seen = []
            def intercept(fd, value):
                if value == b'G':
                    seen.append(not marker.exists())
                    raise INSTALL.InstallError('injected cancellation before GO')
                return original(fd, value)
            with patch.object(INSTALL.os, 'write', side_effect=intercept):
                with self.assertRaisesRegex(INSTALL.InstallError, 'before GO'):
                    run_owned(helper, timeout=2)
            self.assertEqual(seen, [True])
            self.assertFalse(marker.exists())
            paths = INSTALL.product_paths('boi', helper.parent / 'runner-owner')
            self.assertFalse(INSTALL._control_directory(paths).exists())
            with INSTALL._product_lock(paths): pass

    def test_timeout_removes_actual_descendant_before_return(self):
        import fcntl
        with tempfile.TemporaryDirectory() as td:
            helper = Path(td) / 'timeout.py'
            lock = Path(td) / 'timeout-child.lock'
            helper.write_text('import os,fcntl,time\nr,w=os.pipe()\nif os.fork()==0:\n os.close(r)\n f=open(' + repr(str(lock)) + ', "w")\n fcntl.flock(f,fcntl.LOCK_EX)\n os.write(w,b"1");os.close(w)\n time.sleep(10)\n os._exit(0)\nos.close(w);os.read(r,1);os.close(r)\ntime.sleep(10)\n')
            with self.assertRaisesRegex(INSTALL.InstallError, 'timed out'):
                run_owned(helper, timeout=2)
            with lock.open('r') as contender:
                fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_ready_and_failure_frames_may_coalesce_without_losing_failure(self):
        self.assertEqual(INSTALL._status_transition(b'R', False), (True, False))
        self.assertEqual(INSTALL._status_transition(b'F', True), (True, True))
        self.assertEqual(INSTALL._status_transition(b'RF', False), (True, True))
        for frame, ready in ((b'', False), (b'F', False), (b'RR', False), (b'FF', True), (b'FR', True), (b'RRF', False)):
            with self.assertRaises(INSTALL.InstallError):
                INSTALL._status_transition(frame, ready)

    def test_control_frame_has_one_total_deadline(self):
        import socket, threading, time
        reader, writer = socket.socketpair()
        intervals = []
        fragments = [b'{', b'"token":', b'null,', b'"command":', b'"status"', b'}\n']
        writer.sendall(fragments[0])
        def send_slowly():
            previous = time.monotonic()
            try:
                for fragment in fragments[1:]:
                    threading.Event().wait(.25)
                    now = time.monotonic()
                    intervals.append(now - previous)
                    writer.sendall(fragment)
                    previous = now
            except (BrokenPipeError, ConnectionResetError):
                pass  # The reader closes after its total deadline.
        thread = threading.Thread(target=send_slowly)
        thread.start()
        started = time.monotonic()
        try:
            with self.assertRaises((TimeoutError, socket.timeout)):
                INSTALL._receive_control(reader, timeout=1)
            self.assertLess(time.monotonic() - started, 2)
        finally:
            reader.close()
            thread.join(timeout=1)
            writer.close()
        self.assertFalse(thread.is_alive())
        self.assertGreaterEqual(len(intervals), 2)
        self.assertTrue(all(interval < 1 for interval in intervals), intervals)

    def test_control_reader_preserves_valid_fragmented_and_coalesced_frames(self):
        import socket, threading
        expected = {'token': None, 'command': 'status'}
        for fragments in ([b'{"token":null,"command":"status"}\n'],
                          [b'{"token":', b'null,', b'"command":"status"}', b'\n']):
            reader, writer = socket.socketpair()
            def send():
                for fragment in fragments:
                    writer.sendall(fragment)
                    threading.Event().wait(.005)
            thread = threading.Thread(target=send)
            thread.start()
            try:
                self.assertEqual(INSTALL._receive_control(reader, timeout=.3), expected)
            finally:
                thread.join(timeout=1)
                reader.close(); writer.close()
            self.assertFalse(thread.is_alive())

    def test_duplicate_and_oversized_control_frames_fail(self):
        import socket
        for payload in (b'{"token":null,"token":null,"command":"status"}\n', b'x' * (INSTALL.CONTROL_LIMIT + 1)):
            reader, writer = socket.socketpair()
            try:
                writer.sendall(payload)
                with self.assertRaises(INSTALL.InstallError):
                    INSTALL._receive_control(reader)
            finally:
                reader.close(); writer.close()



if __name__ == "__main__":
    unittest.main(verbosity=2)
