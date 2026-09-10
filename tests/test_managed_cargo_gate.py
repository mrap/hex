import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).parents[1]
SOURCE = ROOT / "system/scripts/managed-cargo-gate.py"
SPEC = importlib.util.spec_from_file_location("managed_cargo_gate", SOURCE)
GATE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(GATE)


class ManagedCargoGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.allowed = self.root / "managed"
        self.denied = self.allowed / "retired"
        self.allowed.mkdir()
        self.denied.mkdir()
        self.receipts = self.root / "receipts"
        self.receipts.mkdir(mode=0o700)
        self.log = self.root / "cargo-log.json"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.cargo = self.bin / "cargo"
        self.cargo.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os\n"
            "from pathlib import Path\n"
            "Path(os.environ['FAKE_CARGO_LOG']).write_text(json.dumps({'argv': __import__('sys').argv[1:], 'target': os.environ.get('CARGO_TARGET_DIR'), 'build': os.environ.get('CARGO_BUILD_BUILD_DIR'), 'boi': os.environ.get('BOI_CARGO_TARGET_DIR')}), encoding='utf-8')\n"
            "raise SystemExit(int(os.environ.get('FAKE_CARGO_EXIT', '0')))\n",
            encoding="utf-8",
        )
        self.cargo.chmod(0o755)
        self.write_config()
        self.old_env = dict(os.environ)
        os.environ.clear()
        os.environ.update({
            "HOME": str(self.home),
            "PATH": str(self.bin) + os.pathsep + "/opt/homebrew/bin" + os.pathsep + self.old_env.get("PATH", ""),
            "FAKE_CARGO_LOG": str(self.log),
            "SENTINEL_SECRET": "must-not-reach-receipt",
        })

    def tearDown(self):
        os.environ.clear()
        os.environ.update(self.old_env)
        self.temp.cleanup()

    def write_config(self, revision="test-v1"):
        config = self.home / ".boi/v2/daemon.toml"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text(
            "cargo_target_dir = %s\n[managed_target_policy]\nrevision = %s\nallowed_roots = [%s]\ndenied_roots = [%s]\n"
            % (json.dumps(str(self.allowed / "configured")), json.dumps(revision), json.dumps(str(self.allowed)), json.dumps(str(self.denied))),
            encoding="utf-8",
        )

    def argv(self, target=None, state="dirty", operation="build", extra=None):
        args = ["--caller", "repo-cleanup", "--source-revision", "deadbeef", "--source-state", state, "--receipt-dir", str(self.receipts), operation]
        if target is not None:
            args.extend(["--target-dir", str(target)])
        args.extend(extra or [])
        return args

    def receipt_paths(self):
        return list(self.receipts.glob("*.json"))

    def receipt(self):
        paths = self.receipt_paths()
        self.assertEqual(len(paths), 1)
        return json.loads(paths[0].read_text(encoding="utf-8")), paths[0]

    def assert_no_cargo(self):
        self.assertFalse(self.log.exists())

    def fake_installed_checker(self, payload="", exit_code=0):
        checker = self.home / ".boi/bin/boi"
        checker.parent.mkdir(parents=True, exist_ok=True)
        checker.write_text("#!/bin/sh\nprintf '%s' \"$FAKE_CHECKER_PAYLOAD\"\nexit \"${FAKE_CHECKER_EXIT:-0}\"\n", encoding="utf-8")
        checker.chmod(0o755)
        os.environ["FAKE_CHECKER_PAYLOAD"] = payload
        os.environ["FAKE_CHECKER_EXIT"] = str(exit_code)

    def fake_second_checker_rejection(self, first_payload):
        checker = self.home / ".boi/bin/boi"
        counter = self.root / "checker-count"
        checker.parent.mkdir(parents=True, exist_ok=True)
        checker.write_text(
            "#!/bin/sh\n"
            "count=0\n"
            "if [ -f \"$FAKE_CHECKER_COUNT\" ]; then count=$(cat \"$FAKE_CHECKER_COUNT\"); fi\n"
            "count=$((count + 1))\n"
            "printf '%s' \"$count\" > \"$FAKE_CHECKER_COUNT\"\n"
            "if [ \"$count\" -eq 1 ]; then printf '%s' \"$FAKE_CHECKER_PAYLOAD\"; exit 0; fi\n"
            "echo second-check-refusal >&2\n"
            "exit 7\n",
            encoding="utf-8",
        )
        checker.chmod(0o755)
        os.environ["FAKE_CHECKER_COUNT"] = str(counter)
        os.environ["FAKE_CHECKER_PAYLOAD"] = first_payload

    def adapter_receipt(self, target):
        return GATE._run_adapter(GATE._adapter_path(), "repo-cleanup", str(self.cargo), "deadbeef", str(target))

    def test_bootstrap_and_installed_checker_selection_are_real(self):
        target = self.allowed / "bootstrap"
        self.assertEqual(GATE.run(self.argv(target)), 0)
        bootstrap, _ = self.receipt()
        self.assertEqual(bootstrap["managed_target"]["selection_source"], "ARGUMENT")
        self.log.unlink()
        for path in self.receipt_paths():
            path.unlink()
        installed = self.adapter_receipt(target)
        self.fake_installed_checker(json.dumps(installed))
        self.assertEqual(GATE.run(self.argv(target)), 0)
        used, _ = self.receipt()
        self.assertEqual(used["managed_target"], installed)

    def test_malformed_or_rejected_installed_checker_never_falls_back(self):
        malformed_accepted = json.dumps({"status": "accepted", "resolved_target": str(self.allowed / "incomplete")})
        for payload, exit_code in (("{}", 0), (malformed_accepted, 0), ("", 7)):
            target = self.allowed / ("rejected-%s" % exit_code)
            self.fake_installed_checker(payload, exit_code)
            with self.assertRaises(GATE.GateError) as caught:
                GATE.run(self.argv(target))
            self.assertEqual(caught.exception.code, "TARGET_CHECK_FAILED")
            self.assertFalse(target.exists())
            self.assert_no_cargo()
            self.assertEqual(self.receipt_paths(), [])
            (self.home / ".boi/bin/boi").unlink()

    def test_denied_target_rejects_before_receipt_or_fake_cargo(self):
        with self.assertRaises(GATE.GateError) as caught:
            GATE.run(self.argv(self.denied / "child"))
        self.assertEqual(caught.exception.code, "TARGET_CHECK_FAILED")
        self.assert_no_cargo()
        self.assertEqual(self.receipt_paths(), [])

    def test_first_check_create_exact_leaf_recheck_then_cargo(self):
        target = self.allowed / "fresh"
        calls = []
        original = GATE._run_adapter

        def observed(*args):
            calls.append(args[-1])
            return original(*args)

        with mock.patch.object(GATE, "_run_adapter", side_effect=observed):
            self.assertEqual(GATE.run(self.argv(target)), 0)
        self.assertEqual(calls, [str(target), str(target.resolve())])
        self.assertTrue(target.is_dir())

    def test_changed_target_or_policy_recheck_retains_created_leaf_loudly(self):
        for changed_target, changed_policy in ((True, False), (False, True)):
            target = self.allowed / ("target-change" if changed_target else "policy-change")
            first = {"resolved_target": str(target), "policy_revision": "one:sha256:" + "1" * 64}
            second = dict(first)
            if changed_target:
                second["resolved_target"] = str(self.allowed / "other")
            if changed_policy:
                second["policy_revision"] = "two:sha256:" + "2" * 64
            with mock.patch.object(GATE, "_run_adapter", side_effect=[first, second]):
                with self.assertRaises(GATE.GateError) as caught:
                    GATE.run(self.argv(target))
            self.assertEqual(caught.exception.code, "TARGET_RECHECK_FAILED")
            self.assertIn(str(target), caught.exception.detail)
            self.assertTrue(target.is_dir())
            self.assert_no_cargo()

    def test_failed_second_checker_retains_created_leaf_loudly(self):
        target = self.allowed / "second-check-refused"
        first = self.adapter_receipt(target)
        self.fake_second_checker_rejection(json.dumps(first))
        with self.assertRaises(GATE.GateError) as caught:
            GATE.run(self.argv(target))
        self.assertEqual(caught.exception.code, "TARGET_RECHECK_FAILED")
        self.assertIn(str(target), caught.exception.detail)
        self.assertIn("CHECKER_REJECTED", caught.exception.detail)
        self.assertTrue(target.is_dir())
        self.assert_no_cargo()
        self.assertEqual(self.receipt_paths(), [])

    def test_dirty_and_non_git_unavailable_context_are_recorded(self):
        self.assertEqual(GATE.run(self.argv(self.allowed / "dirty", state="dirty")), 0)
        dirty, dirty_path = self.receipt()
        self.assertEqual(dirty["source_state"], "dirty")
        dirty_path.unlink()
        self.log.unlink()
        self.assertEqual(GATE.run(self.argv(self.allowed / "non-git", state="unavailable")), 0)
        unavailable, _ = self.receipt()
        self.assertEqual(unavailable["source_state"], "unavailable")

    def test_rechecked_target_binds_only_two_cargo_roots(self):
        target = self.allowed / "fresh"
        os.environ["BOI_CARGO_TARGET_DIR"] = str(self.allowed / "contradictory-nested")
        os.environ["CARGO_BUILD_BUILD_DIR"] = str(self.allowed / "same/../fresh")
        self.assertEqual(GATE.run(self.argv(target, extra=["--all-targets"])), 0)
        observed = json.loads(self.log.read_text(encoding="utf-8"))
        self.assertEqual(observed["target"], str(target.resolve()))
        self.assertEqual(observed["build"], str(target.resolve()))
        self.assertIsNone(observed["boi"])
        receipt, _ = self.receipt()
        self.assertNotIn("argv", json.dumps(receipt))
        self.assertNotIn("SENTINEL_SECRET", json.dumps(receipt))
        self.assertNotIn("contradictory-nested", json.dumps(receipt))

    def test_cargo_proxy_keeps_cargo_invocation_name_after_validation(self):
        proxy = self.bin / "rustup"
        proxy.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            "if Path(sys.argv[0]).name != 'cargo': raise SystemExit(41)\n"
            "Path(os.environ['FAKE_CARGO_LOG']).write_text(json.dumps({'argv': sys.argv[1:]}), encoding='utf-8')\n",
            encoding="utf-8",
        )
        proxy.chmod(0o755)
        self.cargo.unlink()
        os.symlink("rustup", self.cargo)
        self.assertEqual(GATE._cargo_path(), str(self.cargo))
        self.assertEqual(GATE.run(self.argv(self.allowed / "proxy")), 0)
        self.assertEqual(json.loads(self.log.read_text(encoding="utf-8"))["argv"][0], "build")

    def test_same_root_build_dir_passes_and_distinct_empty_relative_fail(self):
        target = self.allowed / "root"
        self.assertEqual(GATE.run(self.argv(target, extra=["--config", "build.build-dir=" + str(target.parent / "root")])), 0)
        for value in (str(self.allowed / "other"), "", "relative"):
            self.log.unlink(missing_ok=True)
            with self.assertRaises(GATE.GateError):
                GATE.run(self.argv(self.allowed / ("bad-" + (value or "empty")), extra=["--config", "build.build-dir=" + value]))
            self.assert_no_cargo()

    def test_target_dir_relative_and_raw_passthrough_fail_before_cargo(self):
        for extra in (
            ["--target-dir", "relative"],
            ["--config", "build.build-dir="],
            ["--config", "target-dir=/tmp/not-supported"],
            ["--", "-Zunstable-options"],
        ):
            with self.assertRaises(GATE.GateError):
                GATE.run(self.argv(self.allowed / "no-launch", extra=extra))
            self.assert_no_cargo()

    def test_strict_clippy_maps_to_fixed_arguments(self):
        self.assertEqual(GATE.run(self.argv(self.allowed / "clippy", operation="clippy", extra=["--manifest-path", "/source/Cargo.toml", "--package", "foundation", "--all-targets", "--locked", "--offline", "--deny-warnings"])), 0)
        self.assertEqual(json.loads(self.log.read_text(encoding="utf-8"))["argv"], ["clippy", "--manifest-path", "/source/Cargo.toml", "--package", "foundation", "--all-targets", "--locked", "--offline", "--", "-D", "warnings"])

    def test_receipt_unsafe_parent_owner_and_collision_prevent_cargo(self):
        with self.assertRaises(GATE.GateError):
            GATE.run(["--caller", "x", "--source-revision", "y", "--source-state", "clean", "--receipt-dir", "relative", "build"])
        original_lstat = os.lstat

        def wrong_owner(path):
            value = original_lstat(path)
            if Path(path) == self.receipts:
                return os.stat_result((value.st_mode, value.st_ino, value.st_dev, value.st_nlink, value.st_uid + 1, value.st_gid, value.st_size, value.st_atime, value.st_mtime, value.st_ctime))
            return value

        with mock.patch.object(GATE.os, "lstat", side_effect=wrong_owner):
            with self.assertRaises(GATE.GateError) as caught:
                GATE.run(self.argv(self.allowed / "owner"))
        self.assertEqual(caught.exception.code, "RECEIPT_UNSAFE_DIRECTORY")
        collision = self.receipts / "managed-cargo-fixed.json"
        collision.write_text("evidence", encoding="utf-8")
        with mock.patch.object(GATE.uuid, "uuid4", return_value=type("U", (), {"hex": "fixed"})()):
            with self.assertRaises(GATE.GateError) as caught:
                GATE.run(self.argv(self.allowed / "collision"))
        self.assertEqual(caught.exception.code, "RECEIPT_UNWRITABLE")
        self.assert_no_cargo()

    def test_prepared_receipt_write_and_outcome_update_failures(self):
        with mock.patch.object(GATE, "_write_fd", side_effect=GATE.GateError("RECEIPT_UNWRITABLE", "prepared write")):
            with self.assertRaises(GATE.GateError) as caught:
                GATE.run(self.argv(self.allowed / "prepared"))
        self.assertEqual(caught.exception.code, "RECEIPT_UNWRITABLE")
        self.assert_no_cargo()
        os.environ["FAKE_CARGO_EXIT"] = "7"
        with mock.patch.object(GATE, "_update_receipt", side_effect=GATE.GateError("RECEIPT_UPDATE_FAILED", "final replacement")):
            with self.assertRaises(GATE.GateError) as caught:
                GATE.run(self.argv(self.allowed / "after-cargo"))
        self.assertEqual(caught.exception.code, "CARGO_RESULT_AND_RECEIPT_FAILURE")
        self.assertIn("7", caught.exception.detail)
        self.assertTrue(self.log.exists())

    def test_cargo_launch_failure_updates_receipt_and_reports_dual_failure(self):
        original_run = GATE.subprocess.run

        def missing_cargo(command, *args, **kwargs):
            if command[0] == str(self.cargo):
                raise OSError("synthetic cargo launch failure")
            return original_run(command, *args, **kwargs)

        printed = io.StringIO()
        with mock.patch.object(GATE.subprocess, "run", side_effect=missing_cargo):
            with mock.patch("sys.stdout", printed):
                with self.assertRaises(GATE.GateError) as caught:
                    GATE.run(self.argv(self.allowed / "launch-failure"))
        self.assertEqual(caught.exception.code, "CARGO_LAUNCH_FAILED")
        receipt, _ = self.receipt()
        self.assertEqual(receipt["outcome"], {"state": "launch_failed", "cargo_error": "spawn_failed"})
        self.assertIn("exit_status=launch_failed", printed.getvalue())
        self.assert_no_cargo()
        printed = io.StringIO()
        with mock.patch.object(GATE.subprocess, "run", side_effect=missing_cargo):
            with mock.patch.object(GATE, "_update_receipt", side_effect=GATE.GateError("RECEIPT_UPDATE_FAILED", "synthetic final failure")):
                with mock.patch("sys.stdout", printed):
                    with self.assertRaises(GATE.GateError) as caught:
                        GATE.run(self.argv(self.allowed / "launch-dual-failure"))
        self.assertEqual(caught.exception.code, "CARGO_LAUNCH_AND_RECEIPT_FAILURE")
        self.assertIn("exit_status=launch_failed", printed.getvalue())
        self.assertIn("receipt_update=failed", printed.getvalue())
        self.assert_no_cargo()

    def test_receipt_temp_and_final_identity_type_mode_races_fail_loudly(self):
        path, identity = GATE._create_receipt(self.receipts, {"outcome": "started"})
        details = os.lstat(path)
        self.assertTrue(os.path.isfile(path))
        self.assertEqual((details.st_dev, details.st_ino), identity)
        self.assertEqual(stat.S_IMODE(details.st_mode), 0o600)
        GATE._update_receipt(path, identity, {"outcome": "completed"})
        details = os.lstat(path)
        self.assertTrue(stat.S_ISREG(details.st_mode))
        self.assertEqual(stat.S_IMODE(details.st_mode), 0o600)
        self.assertEqual(json.loads(path.read_text(encoding="utf-8"))["outcome"], "completed")
        path, identity = GATE._create_receipt(self.receipts, {"outcome": "started"})
        path.chmod(0o644)
        with self.assertRaises(GATE.GateError) as caught:
            GATE._update_receipt(path, identity, {"outcome": "done"})
        self.assertEqual(caught.exception.code, "RECEIPT_UPDATE_FAILED")
        path.chmod(0o600)
        original_replace = GATE.os.replace

        def replace_with_symlink(source, destination):
            original_replace(source, destination)
            Path(destination).unlink()
            os.symlink("missing", destination)

        with mock.patch.object(GATE.os, "replace", side_effect=replace_with_symlink):
            with self.assertRaises(GATE.GateError) as caught:
                GATE._update_receipt(path, identity, {"outcome": "done"})
        self.assertEqual(caught.exception.code, "RECEIPT_UPDATE_FAILED")
        path, identity = GATE._create_receipt(self.receipts, {"outcome": "started"})

        def replace_with_directory(source, destination):
            original_replace(source, destination)
            Path(destination).unlink()
            Path(destination).mkdir()

        with mock.patch.object(GATE.os, "replace", side_effect=replace_with_directory):
            with self.assertRaises(GATE.GateError) as caught:
                GATE._update_receipt(path, identity, {"outcome": "done"})
        self.assertEqual(caught.exception.code, "RECEIPT_UPDATE_FAILED")

    def test_success_failure_and_dual_failure_emit_reduced_summary(self):
        printed = io.StringIO()
        with mock.patch("sys.stdout", printed):
            self.assertEqual(GATE.run(self.argv(self.allowed / "success")), 0)
        self.assertIn("receipt=", printed.getvalue())
        self.assertIn("operation=build", printed.getvalue())
        self.assertIn("exit_status=0", printed.getvalue())
        os.environ["FAKE_CARGO_EXIT"] = "9"
        printed = io.StringIO()
        with mock.patch("sys.stdout", printed):
            self.assertEqual(GATE.run(self.argv(self.allowed / "failure")), 9)
        self.assertIn("exit_status=9", printed.getvalue())
        printed = io.StringIO()
        with mock.patch.object(GATE, "_update_receipt", side_effect=GATE.GateError("RECEIPT_UPDATE_FAILED", "full")):
            with mock.patch("sys.stdout", printed):
                with self.assertRaises(GATE.GateError):
                    GATE.run(self.argv(self.allowed / "dual"))
        self.assertIn("exit_status=9", printed.getvalue())
        self.assertIn("receipt_update=failed", printed.getvalue())

    def test_cleanup_guidance_and_continuation_source_oracles(self):
        verifier = (ROOT / "system/skills/repo-cleanup/scripts/verify.sh").read_text(encoding="utf-8")
        self.assertEqual(verifier.count("run_managed_cargo \"cargo "), 3)
        self.assertNotIn(" cargo build ", verifier)
        self.assertNotIn(" cargo test ", verifier)
        skill = (ROOT / "system/skills/boi-delegation/SKILL.md").read_text(encoding="utf-8")
        block = "\n".join(skill.splitlines()[285:293]) + "\n"
        self.assertEqual(__import__("hashlib").sha256(block.encode("utf-8")).hexdigest(), "0208be562ae7c650b7f6719239cceb0271320c67a4d137aafe9dba4643dfe2a2")
        agents = (ROOT / "AGENTS.md").read_text(encoding="utf-8")
        self.assertNotIn("cargo build --release -p scipd", agents)
        self.assertNotIn("target/release/cq", agents)
        self.assertIn("managed-cargo-gate.py", agents)
        docs = (ROOT / "docs/managed-build-targets.md").read_text(encoding="utf-8")
        self.assertIn("raw terminal Cargo", docs)
        self.assertIn("--deny-warnings", docs)


if __name__ == "__main__":
    unittest.main()
