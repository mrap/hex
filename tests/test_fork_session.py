"""Behavior tests for system/scripts/hex-handoff-inject and hex-fork-session.

Runs headless: a fake `tmux` on PATH records calls, answers `has-session` with exact-match-on-`=name`
vs prefix-match semantics, answers `capture-pane` with baseline glyphs plus a banner (once a fake
launcher has run) plus a post-submit glyph, and treats any non-Enter/non-`-l` `send-keys` payload as
the launch line to `eval` (so both a real launch command like `hex-new <name>` and a test double like
`FAKE-LAUNCH <name>` run for real and can invoke the SessionStart hook). This exercises the round trip
stage -> launch -> inject -> archive -> kickoff -> register end to end.
"""
import os, shutil, subprocess, sys, tempfile, unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
INJECT = REPO / "system/scripts/hex-handoff-inject"
FORK = REPO / "system/scripts/hex-fork-session"

TMUX_FAKE = '''#!/bin/bash
echo "$*" >> "__CALLS__"
case "$1" in
  has-session)
    t="$3"
    if [[ "$t" == "="* ]]; then
      tgt="${t#=}"
      [[ -n "$FAKE_EXISTING" && "$tgt" == "$FAKE_EXISTING" ]] && exit 0 || exit 1
    else
      [[ -n "$FAKE_EXISTING" && "$FAKE_EXISTING" == "$t"* ]] && exit 0 || exit 1
    fi ;;
  capture-pane)
    # Build the whole pane in one string and print it with a single write. grep -q exits the
    # instant it sees a match, and if this producer is still mid multi-echo when that happens the
    # pipe closes under it (SIGPIPE); with `pipefail` that non-zero producer exit -- not grep's
    # own success -- becomes the pipeline's exit status. One write sidesteps the race entirely.
    out="⏺ Remote Control disconnected"$'\n'"✻ idle"
    [[ -f "__TMP__/banner-$3" && -z "$FAKE_NO_BANNER" ]] && out="$out"$'\n'"Claude Code v9 banner"
    if [[ -f "__TMP__/submitted-$3" ]]; then
      if [[ -n "$FAKE_RC_AFTER_ENTER" ]]; then
        out="$out"$'\n'"⏺ Remote Control connected"
      elif [[ -z "$FAKE_NO_SUBMIT" ]]; then
        out="$out"$'\n'"⏺ working"
      fi
    fi
    printf '%s\n' "$out" ;;
  send-keys)
    case "$4" in
      Enter) [[ -f "__TMP__/typed-$3" ]] && touch "__TMP__/submitted-$3" ;;
      -l) touch "__TMP__/typed-$3" ;;
      *) export HEX_SESSION_NAME="$3" HEX_DIR="__HEX__"; eval "$4" ;;
    esac ;;
esac
exit 0
'''

HEX_NEW_FAKE = '''#!/bin/bash
for a in "$@"; do case "$a" in --*) exit 1 ;; esac; done
touch "__TMP__/banner-$HEX_SESSION_NAME"
[[ -z "$FAKE_NO_INJECT" ]] && "__INJECT__" > "__TMP__/injected-$HEX_SESSION_NAME.txt"
exit 0
'''

FAKE_LAUNCH_FAKE = '''#!/bin/bash
touch "__TMP__/banner-$HEX_SESSION_NAME"
[[ -z "$FAKE_NO_INJECT" ]] && "__INJECT__" > "__TMP__/injected-$HEX_SESSION_NAME.txt"
exit 0
'''


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.hex = self.tmp / "hex"; (self.hex / ".hex/run").mkdir(parents=True)
        (self.hex / "projects/hex-ops").mkdir(parents=True)
        self.bin = self.tmp / "bin"; self.bin.mkdir()
        self.calls = self.tmp / "tmux-calls"

        def fill(tpl):
            return (tpl.replace("__CALLS__", str(self.calls))
                       .replace("__TMP__", str(self.tmp))
                       .replace("__HEX__", str(self.hex))
                       .replace("__INJECT__", str(INJECT)))

        (self.bin / "tmux").write_text(fill(TMUX_FAKE)); os.chmod(self.bin / "tmux", 0o755)
        (self.bin / "hex-new").write_text(fill(HEX_NEW_FAKE)); os.chmod(self.bin / "hex-new", 0o755)
        (self.bin / "FAKE-LAUNCH").write_text(fill(FAKE_LAUNCH_FAKE)); os.chmod(self.bin / "FAKE-LAUNCH", 0o755)

        self.env = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin", HEX_DIR=str(self.hex),
                        HEX_FORK_LAUNCH="FAKE-LAUNCH @name@", HEX_FORK_TIMEOUT="3", HEX_FORK_INJECT_TIMEOUT="1",
                        HEX_FORK_KICKOFF_DELAY="0", HEX_FORK_ENTER_DELAY="0", HEX_FORK_SUBMIT_TIMEOUT="1",
                        FAKE_EXISTING="", FAKE_NO_SUBMIT="", FAKE_NO_BANNER="", FAKE_NO_INJECT="",
                        FAKE_RC_AFTER_ENTER="")
        self.env.pop("HEX_SESSION_NAME", None); self.env.pop("TMUX", None)

    def tearDown(self): shutil.rmtree(self.tmp)

    def run_fork(self, name, stdin, *args, env=None):
        return subprocess.run([str(FORK), name, *args], input=stdin, text=True, capture_output=True,
                               env=env or self.env, timeout=60)

    def stage_path(self, name):
        return self.hex / ".hex/run/handoffs" / f"{name}.md"

    def archive_dir(self):
        return self.hex / "projects/hex-ops/handoffs"


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

    def test_session_name_path_traversal_is_ignored(self):
        # R14: a non-kebab-case name (e.g. containing "..") exits 0 without acting, even if it
        # resolves to a real file outside the handoffs dir.
        stage = self.hex / ".hex/run/handoffs"; stage.mkdir(parents=True)
        target = self.hex / ".hex/x.md"  # .hex/run/handoffs/../../x.md resolves here
        target.write_text("secret\n")
        env = dict(self.env, HEX_SESSION_NAME="../../x")
        r = subprocess.run([str(INJECT)], env=env, capture_output=True, text=True)
        self.assertEqual((r.returncode, r.stdout), (0, ""))
        self.assertEqual(target.read_text(), "secret\n")

    def test_second_run_after_consumption_is_silent(self):
        # R13: the hook consumes exactly once even across repeated invocations for the same name.
        stage = self.hex / ".hex/run/handoffs"; stage.mkdir(parents=True)
        (stage / "kappa.md").write_text("kappa body\n")
        env = dict(self.env, HEX_SESSION_NAME="kappa")
        r1 = subprocess.run([str(INJECT)], env=env, capture_output=True, text=True)
        self.assertEqual(r1.returncode, 0); self.assertIn("kappa body", r1.stdout)
        r2 = subprocess.run([str(INJECT)], env=env, capture_output=True, text=True)
        self.assertEqual((r2.returncode, r2.stdout), (0, ""))

    def test_archive_dir_occupied_by_file_blocks_claim(self):
        # R13: claim-before-print. If the archive dir path is a regular file, the claim (mkdir/mv)
        # fails BEFORE anything is printed, so stdout stays empty and the pending file survives.
        stage = self.hex / ".hex/run/handoffs"; stage.mkdir(parents=True)
        (stage / "iota.md").write_text("iota body\n")
        (self.hex / "projects/hex-ops/handoffs").write_text("not a directory")
        env = dict(self.env, HEX_SESSION_NAME="iota")
        r = subprocess.run([str(INJECT)], env=env, capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(r.stdout, "")
        self.assertIn("ERROR", r.stderr)
        self.assertTrue((stage / "iota.md").exists())


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

    def test_kickoff_flag_is_sent_verbatim(self):
        r = self.run_fork("eta", "body\n", "--kickoff", "Do change #2: add since/expires/source.")
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls.read_text().splitlines()
        # Text and Enter are separate sends (2026-09-16: "text Enter" in one send was bundled as a paste
        # by Claude Code and the prompt sat unsent in the input box).
        i = calls.index("send-keys -t eta -l Do change #2: add since/expires/source.")
        self.assertEqual(calls[i + 1], "send-keys -t eta Enter")
        self.assertEqual(calls.count("send-keys -t eta Enter"), 1)                    # submitted first try, no retry

    def test_unsubmitted_kickoff_retries_enter_once_then_fails_loudly(self):
        self.env["FAKE_NO_SUBMIT"] = "1"
        r = self.run_fork("theta", "body\n")
        self.assertEqual(r.returncode, 5, r.stderr)
        self.assertEqual(self.calls.read_text().splitlines().count("send-keys -t theta Enter"), 2)
        self.assertIn("NOT submitted", r.stderr); self.assertIn("tmux attach -t theta", r.stderr)

    def test_no_registry_is_fine(self):
        r = self.run_fork("zeta", "body\n")
        self.assertEqual(r.returncode, 0, r.stderr); self.assertNotIn("Traceback", r.stderr)

    # --- U3 additions ---

    def test_default_launch_has_no_fresh_flag(self):
        # R5/KTD2: with HEX_FORK_LAUNCH unset, the default is `hex-new @name@` with no --fresh.
        # The fake `hex-new` exits 1 on any `--` flag, so a lingering --fresh would surface as a
        # missing banner (exit 4) instead of exit 0.
        env = dict(self.env); env.pop("HEX_FORK_LAUNCH", None)
        r = self.run_fork("mu", "body\n", env=env)
        self.assertEqual(r.returncode, 0, r.stderr)
        launch_line = next(l for l in self.calls.read_text().splitlines() if "hex-new mu" in l)
        self.assertNotIn("--fresh", launch_line)

    def test_exact_name_match_not_prefix(self):
        # R6: collision check uses tmux's exact `=name` match, not a prefix match.
        self.env["FAKE_EXISTING"] = "foo-bar"
        r_ok = self.run_fork("foo", "body\n")
        self.assertEqual(r_ok.returncode, 0, r_ok.stderr)
        r_collide = self.run_fork("foo-bar", "body\n")
        self.assertEqual(r_collide.returncode, 3, r_collide.stderr)

    def test_refuses_to_stage_over_existing_pending_handoff(self):
        # R9
        stage = self.hex / ".hex/run/handoffs"; stage.mkdir(parents=True)
        (stage / "nu.md").write_text("original\n")
        r = self.run_fork("nu", "new body\n")
        self.assertEqual(r.returncode, 7, r.stderr)
        self.assertIn("already staged", r.stderr)
        self.assertEqual((stage / "nu.md").read_text(), "original\n")

    def test_banner_timeout_kills_session_and_archives_handoff(self):
        # R8/KTD4: no banner within the timeout -> kill the session this run created, archive the
        # staged handoff (never delete it), name the path. FAKE_NO_INJECT keeps the handoff staged
        # so there is something to archive when the banner check runs.
        self.env["FAKE_NO_BANNER"] = "1"; self.env["FAKE_NO_INJECT"] = "1"; self.env["HEX_FORK_TIMEOUT"] = "1"
        r = self.run_fork("xi", "body\n")
        self.assertEqual(r.returncode, 4, r.stderr)
        self.assertIn("kill-session -t =xi", self.calls.read_text())
        self.assertFalse(self.stage_path("xi").exists())
        archived = list(self.archive_dir().glob("xi-unconsumed-*.md"))
        self.assertEqual(len(archived), 1, archived)

    def test_unconsumed_handoff_kills_session_archives_and_exits_six(self):
        # R7/KTD4/KTD6: banner appears (session up) but the hook never consumes the staged file
        # (SessionStart not wired) -> kill, archive, exit 6, no kickoff, no registry row, no OK.
        reg = self.hex / "projects/hex-ops/sessions.md"
        reg.write_text("# Sessions\n\n## Fleet\n\n| Session | Purpose |\n|---|---|\n| `main` | main |\n\n## Changelog\n")
        self.env["FAKE_NO_INJECT"] = "1"
        r = self.run_fork("omicron", "body\n")
        self.assertEqual(r.returncode, 6, r.stderr)
        calls = self.calls.read_text()
        self.assertNotIn("send-keys -t omicron -l", calls)
        self.assertIn("kill-session -t =omicron", calls)
        self.assertFalse(self.stage_path("omicron").exists())
        archived = list(self.archive_dir().glob("omicron-unconsumed-*.md"))
        self.assertEqual(len(archived), 1, archived)
        self.assertIn("hex-fork-session omicron <", r.stderr)
        self.assertNotIn("| `omicron` |", reg.read_text())
        self.assertNotIn("OK:", r.stdout)

    def test_banner_timeout_with_unwritable_archive_dir_still_exits_four(self):
        # S6/set -e guard: if the archive dir path is occupied by a regular file, kill_and_archive's
        # mkdir fails; the launcher must still report the documented exit 4, not die at 1 under
        # `set -e` inside the command substitution.
        self.env["FAKE_NO_BANNER"] = "1"; self.env["FAKE_NO_INJECT"] = "1"; self.env["HEX_FORK_TIMEOUT"] = "1"
        self.archive_dir().parent.mkdir(parents=True, exist_ok=True)
        self.archive_dir().write_text("not a directory")
        r = self.run_fork("upsilon", "body\n")
        self.assertEqual(r.returncode, 4, r.stderr)
        self.assertIn("cannot create", r.stderr)

    def test_unsubmitted_kickoff_writes_no_registry_row(self):
        # R10: the registry row is written only after submission is proven.
        reg = self.hex / "projects/hex-ops/sessions.md"
        reg.write_text("# Sessions\n\n## Fleet\n\n| Session | Purpose |\n|---|---|\n| `main` | main |\n\n## Changelog\n")
        before = reg.read_text()
        self.env["FAKE_NO_SUBMIT"] = "1"
        r = self.run_fork("pi", "body\n")
        self.assertEqual(r.returncode, 5, r.stderr)
        self.assertEqual(reg.read_text(), before)

    def test_registry_without_table_rows_warns(self):
        # R11: no fleet-table row to append after -> WARN to stderr naming the registry, dead
        # `re.search` removed (no crash either way).
        reg = self.hex / "projects/hex-ops/sessions.md"
        reg.write_text("# Sessions\n\n## Fleet\n\n## Changelog\n")
        r = self.run_fork("rho", "body\n")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("WARN: no fleet-table row", r.stderr)

    # --- U4 additions ---

    def test_baseline_glyphs_are_not_counted_as_submission(self):
        # R12/KTD3: the pane already shows "Remote Control disconnected" and a "*" busy glyph
        # before the kickoff is ever typed. Those must not be mistaken for a submitted prompt.
        self.env["FAKE_NO_SUBMIT"] = "1"
        r = self.run_fork("sigma", "handoff with ⏺ and ✻ glyphs already in it\n")
        self.assertEqual(r.returncode, 5, r.stderr)

    def test_remote_control_reconnect_after_enter_not_counted_as_submission(self):
        # R12: an async Remote Control notice appearing right after Enter is new pane content but
        # must be excluded from the activity check by name, not mistaken for a real submission.
        self.env["FAKE_RC_AFTER_ENTER"] = "1"; self.env["FAKE_NO_SUBMIT"] = "1"
        r = self.run_fork("tau", "body\n")
        self.assertEqual(r.returncode, 5, r.stderr)


if __name__ == "__main__":
    unittest.main()
