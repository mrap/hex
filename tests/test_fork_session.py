"""Behavior tests for system/scripts/hex-handoff-inject and hex-fork-session.

Runs headless: a fake `tmux` on PATH records calls and answers capture-pane with the
Claude Code banner, and the fake launcher command is what the SessionStart hook would
run in the real session (it invokes hex-handoff-inject itself), so the round trip
stage -> launch -> inject -> archive -> kickoff -> register is exercised end to end.
"""
import os, shutil, subprocess, sys, tempfile, unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
INJECT = REPO / "system/scripts/hex-handoff-inject"
FORK = REPO / "system/scripts/hex-fork-session"


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.hex = self.tmp / "hex"; (self.hex / ".hex/run").mkdir(parents=True)
        (self.hex / "projects/hex-ops").mkdir(parents=True)
        self.bin = self.tmp / "bin"; self.bin.mkdir()
        self.calls = self.tmp / "tmux-calls"
        # fake tmux: log every call; has-session -> "no such session" unless EXISTING matches;
        # capture-pane -> banner; send-keys with the launch command -> run the hook like Claude would.
        (self.bin / "tmux").write_text(f'''#!/bin/bash
echo "$*" >> "{self.calls}"
case "$1" in
  has-session) [[ "$3" == "$FAKE_EXISTING" ]] && exit 0 || exit 1 ;;
  capture-pane) echo "Claude Code v9 banner" ;;
  send-keys) if [[ "$4" == *"FAKE-LAUNCH"* ]]; then name="${{4#FAKE-LAUNCH }}"; name="${{name%% *}}";
               HEX_SESSION_NAME="$name" HEX_DIR="{self.hex}" "{INJECT}" > "{self.tmp}/injected-$name.txt"; fi ;;
esac
exit 0
''')
        os.chmod(self.bin / "tmux", 0o755)
        self.env = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin", HEX_DIR=str(self.hex),
                        HEX_FORK_LAUNCH="FAKE-LAUNCH @name@", HEX_FORK_TIMEOUT="3", HEX_FORK_KICKOFF_DELAY="0",
                        FAKE_EXISTING="")
        self.env.pop("HEX_SESSION_NAME", None); self.env.pop("TMUX", None)

    def tearDown(self): shutil.rmtree(self.tmp)

    def run_fork(self, name, stdin, *args):
        return subprocess.run([str(FORK), name, *args], input=stdin, text=True, capture_output=True, env=self.env, timeout=60)


class InjectTests(Base):
    def test_no_pending_handoff_prints_nothing_and_exits_zero(self):
        r = subprocess.run([str(INJECT)], env=dict(self.env, HEX_SESSION_NAME="alpha"), capture_output=True, text=True)
        self.assertEqual((r.returncode, r.stdout), (0, ""))

    def test_pending_handoff_is_printed_then_archived_once(self):
        stage = self.hex / ".hex/run/handoffs"; stage.mkdir(parents=True)
        (stage / "alpha.md").write_text("# Handoff: alpha\n## Work queue\n1. MARKER-42\n")
        env = dict(self.env, HEX_SESSION_NAME="alpha")
        r = subprocess.run([str(INJECT)], env=env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0); self.assertIn("MARKER-42", r.stdout); self.assertIn('Fork handoff for session "alpha"', r.stdout)
        self.assertFalse((stage / "alpha.md").exists())
        archived = list((self.hex / "projects/hex-ops/handoffs").glob("alpha-*.md"))
        self.assertEqual(len(archived), 1); self.assertIn("MARKER-42", archived[0].read_text())
        r2 = subprocess.run([str(INJECT)], env=env, capture_output=True, text=True)
        self.assertEqual(r2.stdout, "", "second start must not re-inject")

    def test_session_name_falls_back_to_tmux_when_env_unset(self):
        stage = self.hex / ".hex/run/handoffs"; stage.mkdir(parents=True)
        (stage / "beta.md").write_text("beta body\n")
        (self.bin / "tmux").write_text("#!/bin/bash\necho beta\n"); os.chmod(self.bin / "tmux", 0o755)
        r = subprocess.run([str(INJECT)], env=dict(self.env, TMUX="fake"), capture_output=True, text=True)
        self.assertIn("beta body", r.stdout)


class ForkTests(Base):
    def test_full_round_trip_stages_launches_injects_kicks_off_and_registers(self):
        reg = self.hex / "projects/hex-ops/sessions.md"
        reg.write_text("# Sessions\n\n## Fleet\n\n| Session | Purpose |\n|---|---|\n| `main` | main |\n\n## Changelog\n")
        r = self.run_fork("gamma", "# Handoff: gamma\n## Work queue\n1. MARKER-77\n", "--purpose", "test lane")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("OK: session 'gamma' up with handoff", r.stdout)
        self.assertIn("MARKER-77", (self.tmp / "injected-gamma.txt").read_text())      # hook saw it
        self.assertFalse((self.hex / ".hex/run/handoffs/gamma.md").exists())              # consumed
        calls = self.calls.read_text()
        self.assertIn("new-session -d -s gamma", calls)
        self.assertIn("Start on the first item in the handoff work queue.", calls)       # kickoff sent
        text = reg.read_text()
        self.assertIn("| `gamma` | test lane |", text); self.assertIn("Created `gamma` via hex-fork-session", text)
        self.assertNotIn("WARN", r.stderr)

    def test_refuses_existing_session_name(self):
        self.env["FAKE_EXISTING"] = "delta"
        r = self.run_fork("delta", "body\n")
        self.assertEqual(r.returncode, 3); self.assertIn("already exists", r.stderr)
        self.assertFalse((self.hex / ".hex/run/handoffs/delta.md").exists())

    def test_rejects_bad_name_and_empty_handoff(self):
        self.assertEqual(self.run_fork("Bad.Name", "body\n").returncode, 2)
        r = self.run_fork("eps", "")
        self.assertEqual(r.returncode, 2); self.assertIn("empty handoff", r.stderr)
        self.assertFalse((self.hex / ".hex/run/handoffs/eps.md").exists())

    def test_no_registry_is_fine(self):
        r = self.run_fork("zeta", "body\n")
        self.assertEqual(r.returncode, 0, r.stderr); self.assertNotIn("Traceback", r.stderr)


if __name__ == "__main__":
    unittest.main()
