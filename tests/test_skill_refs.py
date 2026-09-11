"""Hermetic regressions for the source-only skill-reference audit."""

from __future__ import annotations

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
AUDIT = REPO_ROOT / "tests" / "test_skill_refs.sh"


class SkillReferenceAuditTest(unittest.TestCase):
    def make_fixture(self, skill_text: str | None) -> tuple[tempfile.TemporaryDirectory[str], Path]:
        tempdir = tempfile.TemporaryDirectory()
        root = Path(tempdir.name)
        (root / "tests").mkdir()
        shutil.copy2(AUDIT, root / "tests" / "test_skill_refs.sh")
        skills = root / "system" / "skills"
        skills.mkdir(parents=True)
        if skill_text is not None:
            skill = skills / "demo" / "SKILL.md"
            skill.parent.mkdir()
            skill.write_text(skill_text, encoding="utf-8")
        installer = root / "install.sh"
        installer.write_text(
            "#!/bin/sh\nprintf '%s\\n' installer >> \"$MARKER\"\nexit 99\n",
            encoding="utf-8",
        )
        installer.chmod(0o755)
        return tempdir, root

    def run_audit(self, root: Path) -> subprocess.CompletedProcess[str]:
        runtime = tempfile.TemporaryDirectory()
        self.addCleanup(runtime.cleanup)
        sandbox = Path(runtime.name)
        home = sandbox / "operator-home"
        home.mkdir(exist_ok=True)
        sentinel = home / "sentinel"
        sentinel.write_bytes(b"do not modify")
        fake_bin = sandbox / "hostile-bin"
        fake_bin.mkdir(exist_ok=True)
        marker = sandbox / "external-command-invoked"
        for name in ("install.sh", "cargo", "curl", "git", "launchctl", "security"):
            command = fake_bin / name
            command.write_text(
                "#!/bin/sh\nprintf '%s\\n' \"$0\" >> \"$MARKER\"\nexit 99\n",
                encoding="utf-8",
            )
            command.chmod(0o755)
        env = {
            "HOME": str(home),
            "MARKER": str(marker),
            "PATH": str(fake_bin),
            "LANG": "C",
        }
        result = subprocess.run(
            ["/bin/bash", str(root / "tests" / "test_skill_refs.sh")],
            cwd=root,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertFalse(marker.exists(), result.stdout + result.stderr)
        self.assertEqual(["sentinel"], sorted(path.name for path in home.iterdir()))
        self.assertEqual(b"do not modify", sentinel.read_bytes())
        return result

    def test_existing_source_script_passes_without_external_commands(self) -> None:
        tempdir, root = self.make_fixture("# `bash .hex/skills/demo/scripts/run.sh`\n")
        self.addCleanup(tempdir.cleanup)
        script = root / "system" / "skills" / "demo" / "scripts" / "run.sh"
        script.parent.mkdir()
        script.write_text("#!/bin/sh\n", encoding="utf-8")

        result = self.run_audit(root)

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("PASS: skill reference audit succeeded", result.stdout)

    def test_missing_required_script_fails(self) -> None:
        tempdir, root = self.make_fixture("# `bash .hex/skills/demo/scripts/missing.sh`\n")
        self.addCleanup(tempdir.cleanup)

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("missing source: system/skills/demo/scripts/missing.sh", result.stdout)

    def test_runtime_prefix_does_not_hide_missing_script(self) -> None:
        tempdir, root = self.make_fixture("# `projects/demo/missing.sh`\n")
        self.addCleanup(tempdir.cleanup)

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("missing required script", result.stdout)

    def test_directory_cannot_satisfy_required_script(self) -> None:
        tempdir, root = self.make_fixture("# `bash .hex/skills/demo/scripts/run.sh`\n")
        self.addCleanup(tempdir.cleanup)
        (root / "system" / "skills" / "demo" / "scripts" / "run.sh").mkdir(parents=True)

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("required source is not a file", result.stdout)

    def test_outside_symlink_cannot_satisfy_reference(self) -> None:
        tempdir, root = self.make_fixture("# `.hex/skills/demo/scripts/run.sh`\n")
        self.addCleanup(tempdir.cleanup)
        outside_dir = tempfile.TemporaryDirectory()
        self.addCleanup(outside_dir.cleanup)
        outside = Path(outside_dir.name) / "outside-run.sh"
        outside.write_text("#!/bin/sh\n", encoding="utf-8")
        scripts = root / "system" / "skills" / "demo" / "scripts"
        scripts.mkdir()
        (scripts / "run.sh").symlink_to(outside)

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("source escapes repository", result.stdout)

    def test_external_skill_directory_is_not_read(self) -> None:
        tempdir, root = self.make_fixture(None)
        self.addCleanup(tempdir.cleanup)
        outside_dir = tempfile.TemporaryDirectory()
        self.addCleanup(outside_dir.cleanup)
        outside_skill = Path(outside_dir.name) / "evil"
        outside_skill.mkdir()
        (outside_skill / "SKILL.md").write_text(
            "# `.hex/skills/evil/scripts/outside.sh`\n", encoding="utf-8"
        )
        (root / "system" / "skills" / "evil").symlink_to(outside_skill, target_is_directory=True)

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("skill directory escapes repository", result.stdout)

    def test_external_skill_root_is_not_read(self) -> None:
        tempdir, root = self.make_fixture(None)
        self.addCleanup(tempdir.cleanup)
        outside_dir = tempfile.TemporaryDirectory()
        self.addCleanup(outside_dir.cleanup)
        outside_skills = Path(outside_dir.name) / "skills"
        outside_skills.mkdir()
        (outside_skills / "SKILL.md").write_text("# outside\n", encoding="utf-8")
        skills = root / "system" / "skills"
        shutil.rmtree(skills)
        skills.symlink_to(outside_skills, target_is_directory=True)

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("skill root escapes repository", result.stdout)

    def test_claude_command_maps_to_source(self) -> None:
        tempdir, root = self.make_fixture("```\n.claude/commands/demo.md\n```\n")
        self.addCleanup(tempdir.cleanup)
        command = root / "system" / "commands" / "demo.md"
        command.parent.mkdir()
        command.write_text("# demo\n", encoding="utf-8")

        result = self.run_audit(root)

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("system/commands", result.stdout)

    def test_directory_cannot_satisfy_claude_command_document(self) -> None:
        tempdir, root = self.make_fixture("# `.claude/commands/demo.md`\n")
        self.addCleanup(tempdir.cleanup)
        (root / "system" / "commands" / "demo.md").mkdir(parents=True)

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("required source is not a file", result.stdout)

    def test_unsupported_claude_path_fails(self) -> None:
        tempdir, root = self.make_fixture("# `.claude/scripts/demo.sh`\n")
        self.addCleanup(tempdir.cleanup)

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("no source-layout mapping", result.stdout)

    def test_generated_binary_requires_build_and_install_declarations(self) -> None:
        tempdir, root = self.make_fixture("# `.hex/bin/hex`\n")
        self.addCleanup(tempdir.cleanup)
        manifest = root / "system" / "harness" / "Cargo.toml"
        manifest.parent.mkdir(parents=True)
        manifest.write_text('[[bin]]\nname = "hex"\n', encoding="utf-8")
        (root / "install.sh").write_text(
            'cp "$built" "$TARGET_DIR/.hex/bin/hex"\n', encoding="utf-8"
        )

        result = self.run_audit(root)

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("generated from declared source", result.stdout)

    def test_generated_binary_missing_declaration_fails(self) -> None:
        tempdir, root = self.make_fixture("# `.hex/bin/hex`\n")
        self.addCleanup(tempdir.cleanup)
        manifest = root / "system" / "harness" / "Cargo.toml"
        manifest.parent.mkdir(parents=True)
        manifest.write_text('[[bin]]\nname = "hex"\n', encoding="utf-8")

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("generated artifact declaration missing", result.stdout)

    def test_generated_binary_comment_only_declarations_fail(self) -> None:
        tempdir, root = self.make_fixture("# `.hex/bin/hex`\n")
        self.addCleanup(tempdir.cleanup)
        manifest = root / "system" / "harness" / "Cargo.toml"
        manifest.parent.mkdir(parents=True)
        manifest.write_text('# name = "hex"\n', encoding="utf-8")
        (root / "install.sh").write_text(
            '# cp "$built" "$TARGET_DIR/.hex/bin/hex"\n', encoding="utf-8"
        )

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("generated artifact declaration missing", result.stdout)

    def test_generated_binary_rejects_external_declaration_symlink(self) -> None:
        tempdir, root = self.make_fixture("# `.hex/bin/hex`\n")
        self.addCleanup(tempdir.cleanup)
        outside_dir = tempfile.TemporaryDirectory()
        self.addCleanup(outside_dir.cleanup)
        manifest = Path(outside_dir.name) / "Cargo.toml"
        manifest.write_text('[[bin]]\nname = "hex"\n', encoding="utf-8")
        source_manifest = root / "system" / "harness" / "Cargo.toml"
        source_manifest.parent.mkdir(parents=True)
        source_manifest.symlink_to(manifest)
        (root / "install.sh").write_text(
            'cp "$built" "$TARGET_DIR/.hex/bin/hex"\n', encoding="utf-8"
        )

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("generated artifact declaration missing", result.stdout)

    def test_empty_skill_tree_fails(self) -> None:
        tempdir, root = self.make_fixture(None)
        self.addCleanup(tempdir.cleanup)

        result = self.run_audit(root)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("no readable SKILL.md files found", result.stdout)

    def test_actual_source_check_passes(self) -> None:
        result = self.run_audit(REPO_ROOT)

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("PASS: skill reference audit succeeded", result.stdout)


if __name__ == "__main__":
    unittest.main()
