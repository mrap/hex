"""hex-hooks-merge: manifest entries reach settings.json exactly once (install and upgrade paths)."""
import json, os, subprocess, sys, tempfile, unittest
from pathlib import Path
REPO = Path(__file__).resolve().parents[1]
SCRIPT = REPO / "system/scripts/hex-hooks-merge"
MANIFEST = REPO / "system/hooks/required-hooks.json"

class T(unittest.TestCase):
    def run_merge(self, settings):
        r = subprocess.run([sys.executable, str(SCRIPT), str(MANIFEST), str(settings)], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr); return r.stdout
    def run_merge_raw(self, manifest, settings):
        return subprocess.run([sys.executable, str(SCRIPT), str(manifest), str(settings)], capture_output=True, text=True)
    def run_check(self, manifest, settings):
        return subprocess.run([sys.executable, str(SCRIPT), "--check", str(manifest), str(settings)], capture_output=True, text=True)
    def test_fresh_settings_gets_every_manifest_hook_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as d:
            s = Path(d) / ".claude/settings.json"
            out1 = self.run_merge(s); cfg = json.loads(s.read_text())
            manifest = json.loads(MANIFEST.read_text())
            for ev in manifest: self.assertIn(ev, cfg["hooks"])
            self.assertIn("hex-handoff-inject", json.dumps(cfg["hooks"]["SessionStart"]))
            self.assertEqual(out1.count("hook added:"), sum(len(v) for v in manifest.values()))
            out2 = self.run_merge(s); self.assertEqual(out2, "", "second merge must add nothing")
    def test_existing_session_start_command_is_kept_and_inject_appended(self):
        with tempfile.TemporaryDirectory() as d:
            s = Path(d) / "settings.json"
            s.write_text(json.dumps({"hooks": {"SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "hex memory recent"}]}]}}))
            self.run_merge(s); cfg = json.loads(s.read_text())
            cmds = [h["command"] for e in cfg["hooks"]["SessionStart"] for h in e["hooks"]]
            self.assertIn("hex memory recent", cmds); self.assertTrue(any("hex-handoff-inject" in c for c in cmds))

    def test_malformed_settings_refuses_to_overwrite(self):
        """review 20260916 finding #5: an existing settings.json that fails to parse must never
        be silently treated as {} and overwritten; refuse loudly and leave the file untouched."""
        with tempfile.TemporaryDirectory() as d:
            s = Path(d) / "settings.json"
            s.write_text("{oops")
            before = s.read_bytes()
            r = self.run_merge_raw(MANIFEST, s)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn(str(s), r.stderr)
            self.assertIn("refusing", r.stderr)
            self.assertEqual(s.read_bytes(), before)

    def test_chained_command_segment_counts_as_present(self):
        """review 20260916 finding #13: a manifest command chained onto an existing command with
        ';' must count as present (segment equality), so it is not duplicated."""
        with tempfile.TemporaryDirectory() as d:
            s = Path(d) / "settings.json"
            chained = 'hex memory recent; "$HEX_DIR"/.hex/scripts/hex-handoff-inject'
            s.write_text(json.dumps({"hooks": {"SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": chained}]}]}}))
            out = self.run_merge(s)
            self.assertNotIn("hex-handoff-inject", out, "already-present chained command must not be re-added")
            cfg = json.loads(s.read_text())
            chained_entry = next(e for e in cfg["hooks"]["SessionStart"] for h in e["hooks"] if h["command"] == chained)
            self.assertEqual(len(chained_entry["hooks"]), 1, "entry count for the chained command must be unchanged")

    def test_manifest_command_containing_separator_is_idempotent(self):
        """Review 20260916 round 2 #2: a manifest command that itself contains ';' must match the
        whole existing command, not only its split segments; otherwise every merge re-adds it."""
        with tempfile.TemporaryDirectory() as d:
            manifest = Path(d) / "manifest.json"
            manifest.write_text(json.dumps({"SessionStart": [{"matcher": "", "command": "bash -c 'a; b'"}]}))
            s = Path(d) / "settings.json"
            self.assertEqual(0, self.run_merge_raw(manifest, s).returncode)
            self.assertEqual(0, self.run_merge_raw(manifest, s).returncode)
            hooks = json.loads(s.read_text())["hooks"]["SessionStart"]
            self.assertEqual(1, len(hooks), hooks)
            self.assertEqual(0, self.run_check(manifest, s).returncode)

    def test_mention_inside_echo_string_is_not_presence(self):
        """R3/KTD7: a mention of the hook command inside a longer echo string is not presence;
        the real hook is still added."""
        with tempfile.TemporaryDirectory() as d:
            s = Path(d) / "settings.json"
            s.write_text(json.dumps({"hooks": {"PreToolUse": [{"matcher": "Write|Edit|MultiEdit|NotebookEdit", "hooks": [{"type": "command", "command": 'echo "hex hook worktree-guard is off"'}]}]}}))
            out = self.run_merge(s)
            self.assertIn("hex hook worktree-guard", out)
            cfg = json.loads(s.read_text())
            cmds = [h["command"] for e in cfg["hooks"]["PreToolUse"] for h in e["hooks"]]
            self.assertIn("hex hook worktree-guard", cmds)

    def test_second_run_over_already_merged_file_is_idempotent(self):
        with tempfile.TemporaryDirectory() as d:
            s = Path(d) / "settings.json"
            self.run_merge(s)
            out2 = self.run_merge(s)
            self.assertEqual(out2, "")

    def test_missing_settings_file_is_created_with_only_manifest_hooks(self):
        with tempfile.TemporaryDirectory() as d:
            s = Path(d) / "nested" / "settings.json"
            self.assertFalse(s.exists())
            self.run_merge(s)
            cfg = json.loads(s.read_text())
            manifest = json.loads(MANIFEST.read_text())
            for ev in manifest: self.assertIn(ev, cfg["hooks"])

    def test_manifest_entry_without_command_raises(self):
        """review 20260916 finding #22: the dead 'script' manifest branch is deleted; a manifest
        entry lacking 'command' must fail loudly (KeyError), never silently build a script wrapper."""
        with tempfile.TemporaryDirectory() as d:
            m = Path(d) / "manifest.json"
            m.write_text(json.dumps({"SessionStart": [{"matcher": "", "script": "system/scripts/hex-handoff-inject"}]}))
            s = Path(d) / "settings.json"
            r = self.run_merge_raw(m, s)
            self.assertNotEqual(r.returncode, 0)
            self.assertFalse(s.exists())

    def test_check_mode_writes_nothing_and_reports_missing(self):
        """R1a --check: writes nothing; exits 3 with missing 'event: command' lines on stdout when
        settings is missing (everything missing), 0 when all present, 2 with the JSON error on
        stderr when settings is malformed."""
        with tempfile.TemporaryDirectory() as d:
            s = Path(d) / "settings.json"
            r = self.run_check(MANIFEST, s)
            self.assertEqual(r.returncode, 3)
            self.assertFalse(s.exists(), "--check must never write settings")
            self.assertIn("SessionStart:", r.stdout)

            self.run_merge(s)
            r2 = self.run_check(MANIFEST, s)
            self.assertEqual(r2.returncode, 0)
            self.assertEqual(r2.stdout, "")

            bad = Path(d) / "bad.json"
            bad.write_text("{oops")
            r3 = self.run_check(MANIFEST, bad)
            self.assertEqual(r3.returncode, 2)
            self.assertIn(str(bad), r3.stderr)
            self.assertEqual(bad.read_text(), "{oops", "--check must never write settings")

if __name__ == "__main__": unittest.main()
