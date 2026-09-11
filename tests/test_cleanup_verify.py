"""Exercise the cleanup gate with isolated tools and real npm scripts."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


GATE = Path(__file__).resolve().parents[1] / "system/skills/repo-cleanup/scripts/verify.sh"


class CleanupGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.repo = self.root / "project"
        self.repo.mkdir()
        for command in ("mktemp", "tail", "rm", "sh"):
            (self.bin / command).symlink_to(shutil.which(command))

    def runner(self, name, code):
        path = self.bin / name
        path.write_text(f"#!/bin/sh\necho 'ran {name}'\nexit {code}\n")
        path.chmod(0o755)

    def run_gate(self):
        env = os.environ.copy()
        env.update(PATH=str(self.bin), HOME=str(self.root))
        return subprocess.run(
            ["/bin/bash", str(GATE), str(self.repo)],
            env=env, text=True, capture_output=True, timeout=15,
        )

    def assert_failed(self, result):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Verify gate: FAIL", result.stdout)

    def test_python_without_test_runner_fails(self):
        (self.repo / "pyproject.toml").write_text("")
        result = self.run_gate()
        self.assert_failed(result)
        self.assertIn("pytest", result.stdout + result.stderr)

    def test_javascript_without_npm_fails(self):
        (self.repo / "package.json").write_text("{}")
        result = self.run_gate()
        self.assert_failed(result)
        self.assertIn("npm", result.stdout + result.stderr)

    def test_python_test_failure_is_not_hidden(self):
        (self.repo / "setup.py").write_text("")
        self.runner("pytest", 5)
        result = self.run_gate()
        self.assert_failed(result)
        self.assertIn("exit: 5", result.stdout)

    def test_lint_failure_is_advisory_when_tests_pass(self):
        (self.repo / "pyproject.toml").write_text("")
        self.runner("pytest", 0)
        self.runner("ruff", 3)
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("ran pytest", result.stdout)
        self.assertIn("exit: 3", result.stdout)

    def test_one_ecosystem_cannot_hide_another_missing_runner(self):
        (self.repo / "package.json").write_text("{}")
        (self.repo / "setup.py").write_text("")
        self.runner("pytest", 0)
        result = self.run_gate()
        self.assert_failed(result)
        self.assertIn("ran pytest", result.stdout)

    def test_real_npm_requires_a_test_script_and_preserves_its_exit(self):
        # No skip: the contract suite must declare this prerequisite in CI.
        npm, node = shutil.which("npm"), shutil.which("node")
        self.assertIsNotNone(npm, "npm is required for the real-script regression")
        self.assertIsNotNone(node, "node is required for the real-script regression")
        (self.bin / "npm").symlink_to(Path(npm).resolve())
        (self.bin / "node").symlink_to(Path(node).resolve())
        package = self.repo / "package.json"
        package.write_text(json.dumps({"name": "cleanup-fixture", "scripts": {}}))
        self.assert_failed(self.run_gate())
        for exit_code in (0, 7):
            with self.subTest(exit_code=exit_code):
                package.write_text(json.dumps({
                    "name": "cleanup-fixture",
                    "scripts": {"test": f"node -e 'process.exit({exit_code})'"},
                }))
                result = self.run_gate()
                if exit_code:
                    self.assert_failed(result)
                    self.assertIn("exit: 7", result.stdout)
                else:
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        package.write_text(json.dumps({
            "name": "cleanup-fixture",
            "scripts": {"test": "node -e 'process.exit(0)'",
                        "build": "node -e 'process.exit(9)'"},
        }))
        result = self.run_gate()
        self.assert_failed(result)
        self.assertIn("exit: 9", result.stdout)

        package.write_text(json.dumps({
            "name": "cleanup-fixture",
            "scripts": {"test": "node -e 'process.exit(0)'",
                        "lint": "node -e 'process.exit(4)'"},
        }))
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("exit: 4", result.stdout)


if __name__ == "__main__":
    unittest.main()
