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

if __name__ == "__main__": unittest.main()
