import hashlib
import importlib.util
import io
import json
import errno
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import time
import unittest
from contextlib import redirect_stderr
from unittest import mock


SOURCE = Path(__file__).parents[1] / "system/scripts/managed-target-check.py"
SPEC = importlib.util.spec_from_file_location("managed_target_check", SOURCE)
CHECK = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(CHECK)


class ManagedTargetCheckTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.allowed = self.root / "managed"
        self.denied = self.allowed / "retired"
        self.allowed.mkdir()
        self.denied.mkdir()
        self.tool = self.root / "tool"
        self.tool.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.tool.chmod(0o755)
        self.old_env = dict(os.environ)
        os.environ.clear()
        os.environ["HOME"] = str(self.home)
        self.write_config()

    def tearDown(self):
        os.environ.clear()
        os.environ.update(self.old_env)
        self.temp.cleanup()

    def write_config(self, target=None, allowed=None, denied=None, revision="operator-v1"):
        config = self.home / ".boi/v2/daemon.toml"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text(
            "cargo_target_dir = %s\n[managed_target_policy]\nrevision = %s\nallowed_roots = %s\ndenied_roots = %s\n" % (
                json.dumps(str(self.allowed / "configured" if target is None else target)), json.dumps(revision),
                json.dumps([str(x) for x in ([self.allowed] if allowed is None else allowed)]),
                json.dumps([str(x) for x in ([self.denied] if denied is None else denied)]),
            ), encoding="utf-8",
        )

    def bootstrap(self, target=None):
        return CHECK.check("foundation-install", str(self.tool), "cac82662", target)

    def assert_fails(self, code, **kwargs):
        with self.assertRaises(CHECK.ManagedTargetCheckError) as caught:
            self.bootstrap(**kwargs)
        self.assertEqual(caught.exception.code, code)

    def test_bootstrap_precedence_and_exact_policy_hash(self):
        os.environ["CARGO_TARGET_DIR"] = str(self.allowed / "cargo")
        os.environ["BOI_CARGO_TARGET_DIR"] = str(self.allowed / "boi")
        self.assertEqual(self.bootstrap()["selection_source"], "CARGO_TARGET_DIR")
        receipt = self.bootstrap(str(self.allowed / "argument"))
        self.assertEqual(receipt["selection_source"], "ARGUMENT")
        os.environ.pop("CARGO_TARGET_DIR")
        receipt = self.bootstrap()
        self.assertEqual(receipt["selection_source"], "BOI_CARGO_TARGET_DIR")
        os.environ.pop("BOI_CARGO_TARGET_DIR")
        receipt = self.bootstrap()
        self.assertEqual(receipt["selection_source"], "DAEMON_TOML")
        digest = hashlib.sha256()
        digest.update(b"boi-managed-target-policy-v1\0operator-v1\0allowed\0")
        digest.update(os.fsencode(self.allowed.resolve())); digest.update(b"\0denied\0")
        digest.update(os.fsencode(self.denied.resolve())); digest.update(b"\0")
        self.assertEqual(receipt["policy_revision"], "operator-v1:sha256:" + digest.hexdigest())

    def test_bootstrap_allows_boundaries_aliases_and_missing_leaves_without_writes(self):
        alias = self.root / "alias"
        alias.symlink_to(self.allowed, target_is_directory=True)
        target = alias / "missing/../leaf"
        before = sorted(path.relative_to(self.root) for path in self.root.rglob("*") if path.is_dir())
        receipt = self.bootstrap(str(target))
        after = sorted(path.relative_to(self.root) for path in self.root.rglob("*") if path.is_dir())
        self.assertEqual(receipt["resolved_target"], str((self.allowed / "leaf").resolve()))
        self.assertEqual(before, after)
        self.assertEqual(self.bootstrap(str(self.allowed))["resolved_target"], str(self.allowed.resolve()))

    def test_bootstrap_denies_alias_then_parent_traversal(self):
        denied_subdir = self.denied / "subdir"
        denied_subdir.mkdir()
        link = self.allowed / "link-to-denied"
        link.symlink_to(denied_subdir, target_is_directory=True)
        self.assert_fails("DENIED_TARGET", target=str(link / ".." / "leaf"))

    def test_bootstrap_denies_boundary_descendant_and_outside(self):
        self.assert_fails("DENIED_TARGET", target=str(self.denied))
        self.assert_fails("DENIED_TARGET", target=str(self.denied / "child"))
        self.assert_fails("OUTSIDE_ALLOWED_ROOT", target=str(self.root / "outside"))

    def test_build_dir_override_must_resolve_to_the_single_approved_root(self):
        target = str(self.allowed / "missing-target")
        CHECK.validate_same_root_build_dir(str(self.allowed / "same/../missing-target"), target)
        with self.assertRaises(CHECK.ManagedTargetCheckError) as caught:
            CHECK.validate_same_root_build_dir(str(self.denied / "intermediate"), target)
        self.assertEqual(caught.exception.code, "BUILD_DIR_OVERRIDE")
        with self.assertRaises(CHECK.ManagedTargetCheckError) as caught:
            CHECK.validate_same_root_build_dir("relative", target)
        self.assertEqual(caught.exception.code, "RELATIVE_TARGET")

    def test_cli_rejects_withdrawn_build_dir_argument(self):
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
            CHECK.main([
                "--caller", "foundation-install", "--executable", str(self.tool),
                "--source-revision", "cac82662", "--build-dir", str(self.allowed),
            ])
        self.assertEqual(caught.exception.code, 2)

    def test_bootstrap_rejects_empty_relative_missing_policy_and_broken_alias(self):
        self.assert_fails("EMPTY_TARGET", target="")
        self.assert_fails("RELATIVE_TARGET", target="relative")
        os.environ["CARGO_TARGET_DIR"] = ""
        self.assert_fails("EMPTY_TARGET")
        os.environ.pop("CARGO_TARGET_DIR")
        os.environ["BOI_CARGO_TARGET_DIR"] = ""
        self.assert_fails("EMPTY_TARGET")
        os.environ.pop("BOI_CARGO_TARGET_DIR")
        self.write_config(target="")
        self.assert_fails("INVALID_CONFIG")
        self.write_config()
        (self.home / ".boi/v2/daemon.toml").write_text("cargo_target_dir = '/tmp/x'\n", encoding="utf-8")
        self.assert_fails("MISSING_POLICY")
        self.write_config()
        broken = self.allowed / "broken"
        broken.symlink_to(self.root / "gone")
        self.assert_fails("TARGET_RESOLUTION", target=str(broken / "leaf"))

    def test_bootstrap_validates_policy_shape(self):
        self.write_config(allowed=[], denied=[self.denied])
        self.assert_fails("INVALID_POLICY")
        self.write_config(allowed=[Path("relative")])
        self.assert_fails("RELATIVE_TARGET")
        self.write_config(revision=" ")
        self.assert_fails("INVALID_POLICY")
        self.write_config()
        config = self.home / ".boi/v2/daemon.toml"
        config.write_text(config.read_text(encoding="utf-8") + "unknown = 'nope'\n", encoding="utf-8")
        self.assert_fails("INVALID_POLICY")

    def test_bootstrap_rejects_unknown_root_keys_and_accepts_known_daemon_keys(self):
        config = self.home / ".boi/v2/daemon.toml"
        original = config.read_text(encoding="utf-8")
        config.write_text("unexpected_root = true\n" + original, encoding="utf-8")
        self.assert_fails("INVALID_CONFIG")
        config.write_text(
            "phase_wall_clock_budget_secs = 7200\n"
            "goose_attempt_timeout_secs = 2700\n"
            "worker_runtime_policy = { allowed_providers = ['codex'], require_explicit_effort = true, approved_models = { codex = ['gpt-6'] } }\n"
            + original,
            encoding="utf-8",
        )
        self.assertEqual(self.bootstrap()["status"], "accepted")

    def test_bootstrap_matches_timeout_decoder_boundaries(self):
        config = self.home / ".boi/v2/daemon.toml"
        original = config.read_text(encoding="utf-8")
        accepted = ("0", "1", "9223372036854775807")
        rejected = (
            "-1", "true", "1.0", '"1"', "9223372036854775808",
            "18446744073709551615", "18446744073709551616",
        )
        for field in ("phase_wall_clock_budget_secs", "goose_attempt_timeout_secs"):
            for value in accepted:
                with self.subTest(field=field, value=value):
                    config.write_text("%s = %s\n%s" % (field, value, original), encoding="utf-8")
                    self.assertEqual(self.bootstrap()["status"], "accepted")
            for value in rejected:
                with self.subTest(field=field, value=value):
                    config.write_text("%s = %s\n%s" % (field, value, original), encoding="utf-8")
                    self.assert_fails("INVALID_CONFIG")

    def test_bootstrap_rejects_malformed_or_unknown_worker_runtime_policy(self):
        config = self.home / ".boi/v2/daemon.toml"
        original = config.read_text(encoding="utf-8")
        config.write_text("worker_runtime_policy = []\n" + original, encoding="utf-8")
        self.assert_fails("INVALID_CONFIG")
        config.write_text(
            "worker_runtime_policy = { allowed_providers = ['codex'], approved_models = { codex = ['gpt-6'] }, unknown = true }\n" + original,
            encoding="utf-8",
        )
        self.assert_fails("INVALID_CONFIG")

    def test_bootstrap_rejects_invalid_worker_runtime_policy_mapping(self):
        config = self.home / ".boi/v2/daemon.toml"
        original = config.read_text(encoding="utf-8")
        config.write_text(
            "worker_runtime_policy = { allowed_providers = ['codex'], approved_models = { other = ['gpt-6'] } }\n" + original,
            encoding="utf-8",
        )
        self.assert_fails("INVALID_CONFIG")

    def test_bootstrap_accepts_empty_worker_runtime_policy(self):
        config = self.home / ".boi/v2/daemon.toml"
        original = config.read_text(encoding="utf-8")
        config.write_text(
            "worker_runtime_policy = { allowed_providers = [], approved_models = {} }\n" + original,
            encoding="utf-8",
        )
        self.assertEqual(self.bootstrap()["status"], "accepted")

    def test_bootstrap_rejects_invalid_configured_target_even_with_explicit_target(self):
        self.write_config(target="relative")
        self.assert_fails("INVALID_CONFIG", target=str(self.allowed / "explicit"))

    def test_bootstrap_allows_explicit_target_when_configured_target_is_denied(self):
        self.write_config(target=self.denied / "configured")
        receipt = self.bootstrap(target=str(self.allowed / "explicit"))
        self.assertEqual(receipt["selection_source"], "ARGUMENT")

    def test_bootstrap_names_missing_toml_capability(self):
        with mock.patch.dict("sys.modules", {"tomllib": None}):
            self.assert_fails("TOML_PARSER_UNAVAILABLE")

    def fake_boi(self, body):
        checker = self.home / ".boi/bin/boi"
        checker.parent.mkdir(parents=True, exist_ok=True)
        checker.write_text("#!/usr/bin/env python3\n" + body, encoding="utf-8")
        checker.chmod(checker.stat().st_mode | stat.S_IXUSR)
        return checker

    def assert_child_not_running(self, pid_file):
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            if pid_file.exists():
                pid = pid_file.read_text(encoding="utf-8")
                state = subprocess.run(["ps", "-o", "stat=", "-p", pid], text=True, capture_output=True).stdout.strip()
                if not state or state.startswith("Z"):
                    return
            time.sleep(0.02)
        self.fail("owned checker descendant remained running after cleanup")

    def fake_boi_with_sleeping_child(self, pid_file, output=""):
        return self.fake_boi(
            "import subprocess, time\nfrom pathlib import Path\n"
            "child = subprocess.Popen(['/bin/sleep', '30'])\n"
            "Path(%r).write_text(str(child.pid), encoding='utf-8')\n%s\ntime.sleep(30)\n" % (str(pid_file), output)
        )

    def fake_shell_boi_with_sleeping_child(self, pid_file):
        checker = self.home / ".boi/bin/boi"
        checker.parent.mkdir(parents=True, exist_ok=True)
        checker.write_text(
            "#!/bin/sh\n/bin/sleep 30 &\nprintf '%s' \"$!\" > %s\n/bin/sleep 30\n" % ("%s", str(pid_file)),
            encoding="utf-8",
        )
        checker.chmod(checker.stat().st_mode | stat.S_IXUSR)
        return checker

    def installed_receipt(self, **overrides):
        canonical = str(self.tool.resolve())
        payload = {
            "schema_version": CHECK.SCHEMA_VERSION, "status": "accepted", "caller_identity": "foundation-install",
            "executable_identity": {"canonical_path": canonical, "sha256": hashlib.sha256(self.tool.read_bytes()).hexdigest()},
            "resolved_target": str(self.allowed / "checked"), "policy_revision": "operator-v1:sha256:" + "a" * 64,
            "source_revision": "cac82662", "selection_source": "DAEMON_TOML",
        }
        payload.update(overrides)
        return payload

    def test_installed_checker_receipt_is_validated_and_no_bootstrap_fallback(self):
        self.fake_boi("import json\nprint(json.dumps(%r))\n" % self.installed_receipt())
        self.assertEqual(self.bootstrap()["status"], "accepted")
        with mock.patch.dict("sys.modules", {"tomllib": None}):
            self.assertEqual(self.bootstrap()["status"], "accepted")
        self.fake_boi("print('not-json')\n")
        self.assert_fails("CHECKER_MALFORMED_RECEIPT")
        self.fake_boi("raise SystemExit(7)\n")
        self.assert_fails("CHECKER_REJECTED")
        checker = self.home / ".boi/bin/boi"
        checker.unlink()
        checker.symlink_to(self.home / "missing-boi")
        self.assert_fails("CHECKER_UNAVAILABLE")
        checker.unlink()
        checker.write_text("not executable", encoding="utf-8")
        checker.chmod(0o644)
        self.assert_fails("CHECKER_UNAVAILABLE")

    def test_installed_checker_rejects_mismatched_and_oversized_or_slow_output(self):
        self.fake_boi("import json\np=%r\np['source_revision']='wrong'\nprint(json.dumps(p))\n" % self.installed_receipt())
        self.assert_fails("CHECKER_MISMATCHED_RECEIPT")
        self.fake_boi("print('x' * (%d + 1))\n" % CHECK.MAX_CHECKER_OUTPUT_BYTES)
        self.assert_fails("CHECKER_OUTPUT_TOO_LARGE")
        self.fake_boi("import time\ntime.sleep(2)\n")
        with self.assertRaises(CHECK.ManagedTargetCheckError) as caught:
            CHECK.check("foundation-install", str(self.tool), "cac82662", timeout_seconds=0.05)
        self.assertEqual(caught.exception.code, "CHECKER_TIMEOUT")

    def test_timeout_kills_owned_checker_descendants(self):
        pid_file = self.root / "timeout-child.pid"
        self.fake_shell_boi_with_sleeping_child(pid_file)
        with self.assertRaises(CHECK.ManagedTargetCheckError) as caught:
            CHECK.check("foundation-install", str(self.tool), "cac82662", timeout_seconds=0.5)
        self.assertEqual(caught.exception.code, "CHECKER_TIMEOUT")
        self.assertTrue(pid_file.exists())
        self.assert_child_not_running(pid_file)

    def test_oversized_output_kills_owned_checker_descendants(self):
        pid_file = self.root / "output-child.pid"
        self.fake_boi_with_sleeping_child(pid_file, "print('x' * (%d + 1), flush=True)" % CHECK.MAX_CHECKER_OUTPUT_BYTES)
        with self.assertRaises(CHECK.ManagedTargetCheckError) as caught:
            CHECK.check("foundation-install", str(self.tool), "cac82662", timeout_seconds=1)
        self.assertEqual(caught.exception.code, "CHECKER_OUTPUT_TOO_LARGE")
        self.assert_child_not_running(pid_file)

    def test_read_error_kills_owned_checker_descendants_and_preserves_cleanup_error(self):
        pid_file = self.root / "read-child.pid"
        checker = self.fake_boi_with_sleeping_child(pid_file, "print('ready', flush=True)")
        process = subprocess.Popen(
            [str(checker)], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, close_fds=True, start_new_session=True,
        )
        self.assertIsNotNone(process.stdout)
        self.assertIsNotNone(process.stderr)
        streams = {process.stdout: bytearray(), process.stderr: bytearray()}
        try:
            deadline = time.monotonic() + 1
            while not pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(pid_file.exists())
            with mock.patch.object(CHECK.os, "read", side_effect=OSError(errno.EIO, "fixture read fault")):
                with self.assertRaises(CHECK.ManagedTargetCheckError) as caught:
                    CHECK._abort_checker_group(process, streams, "CHECKER_READ_FAILED", "fixture read fault")
        finally:
            process.stdout.close()
            process.stderr.close()
        self.assertEqual(caught.exception.code, "CHECKER_CLEANUP_FAILED")
        self.assertIn("fixture read fault", caught.exception.detail)
        self.assert_child_not_running(pid_file)

    def test_installed_checker_rejects_superseded_ten_field_receipt(self):
        receipt = self.installed_receipt()
        receipt["resolved_build_dir"] = str(self.allowed / "checked")
        receipt["build_dir_selection_source"] = "TARGET_DEFAULT"
        self.fake_boi("import json\nprint(json.dumps(%r))\n" % receipt)
        self.assert_fails("CHECKER_MALFORMED_RECEIPT")

    def test_installed_checker_receives_target_as_an_argument(self):
        target = str(self.allowed / "explicit-target")
        receipt = self.installed_receipt(
            resolved_target=str(Path(target).resolve()),
            selection_source="ARGUMENT",
        )
        argv_log = self.root / "checker-argv.json"
        self.fake_boi(
            "import json, sys\nopen(%r, 'w').write(json.dumps(sys.argv))\nprint(json.dumps(%r))\n" % (str(argv_log), receipt)
        )
        self.bootstrap(target=target)
        self.assertEqual(
            json.loads(argv_log.read_text(encoding="utf-8"))[1:],
            ["target", "check", "--caller", "foundation-install", "--executable", str(self.tool),
             "--source-revision", "cac82662", "--target", target],
        )

    def test_missing_bootstrap_config_fails_without_creating_it(self):
        config = self.home / ".boi/v2/daemon.toml"
        config.unlink()
        self.assert_fails("MISSING_CONFIG")
        self.assertFalse(config.exists())

    def test_invalid_executable_rejects_before_checker(self):
        self.tool.chmod(0o644)
        self.assert_fails("INVALID_EXECUTABLE")


if __name__ == "__main__":
    unittest.main()
