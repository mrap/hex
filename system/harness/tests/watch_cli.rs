//! L2 boundary tests for `hex watch` (spec docs/hex-watch.md): spawn the
//! built binary against a tempdir HEX_DIR with a stub gmail command in
//! watch.toml. The loop itself is unit-tested in `src/watch/tick.rs`; this
//! file covers argv, exit codes, stdout/stderr, and files written.

use std::path::{Path, PathBuf};
use std::process::Command;

fn hex_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hex"))
}

struct Rig {
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = dir.path().join(".hex/config");
        std::fs::create_dir_all(&cfg).unwrap();
        // stub gmail: one fresh hit, echoes argv to a log so tests can assert the query
        let log = dir.path().join("gmail.log");
        std::fs::write(
            cfg.join("watch.toml"),
            format!(
                "action_timeout_secs = 5\n[sources.gmail]\ncommand = \"echo {{query}} {{account}} >> {} && printf '%s\\\\n' '{{\\\"account\\\":\\\"a@b\\\",\\\"id\\\":\\\"m1\\\",\\\"internal_ms\\\":9999999999999,\\\"date\\\":\\\"D\\\",\\\"from\\\":\\\"F\\\",\\\"subject\\\":\\\"S\\\"}}'\"\n",
                log.display()
            ),
        )
        .unwrap();
        // stub tmux on PATH: records argv, "has-session" succeeds only for `retreat`
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("tmux"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TMUX_LOG\"\nif [ \"$1\" = has-session ]; then case \"$3\" in =retreat) exit 0;; *) exit 1;; esac; fi\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("tmux"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        // the default notify action runs `$HEX_DIR/.hex/bin/hex`: point it at the test binary
        let hb = dir.path().join(".hex/bin");
        std::fs::create_dir_all(&hb).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(hex_bin(), hb.join("hex")).unwrap();
        Rig { dir }
    }
    fn hex(&self) -> &Path {
        self.dir.path()
    }
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let path = format!(
            "{}:{}",
            self.hex().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new(hex_bin())
            .args(args)
            .env("HEX_DIR", self.hex())
            .env("PATH", path)
            .env("TMUX_LOG", self.hex().join("tmux.log"))
            // never reach the live engine from a test: emits fail loudly instead
            .env("III_URL", "ws://127.0.0.1:1")
            .env_remove("TMUX")
            .env_remove("HEX_SESSION_NAME")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }
    fn item(&self, id: &str) -> serde_json::Value {
        let p = self
            .hex()
            .join(".hex/watch/items")
            .join(format!("{id}.json"));
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }
}

#[test]
fn add_list_status_and_the_record_shape() {
    let r = Rig::new();
    let (rc, out, err) = r.run(&[
        "watch",
        "add",
        "--query",
        "from:x",
        "--account",
        "legacy",
        "--action",
        "true",
        "--note",
        "demo",
    ]);
    assert_eq!(rc, 0, "{err}");
    let id = out.trim().to_string();
    assert_eq!(id.len(), 8);
    let w = r.item(&id);
    assert_eq!(w["source"], "gmail");
    assert_eq!(w["match"]["query"], "from:x");
    assert_eq!(w["match"]["account"], "legacy");
    assert_eq!(w["status"], "pending");
    assert!(w["since"].is_string() && w["expires"].is_string());
    let (rc, out, _) = r.run(&["watch", "list"]);
    assert_eq!(rc, 0);
    assert!(
        out.contains(&id)
            && out.contains("pending")
            && out.contains("in 13d")
            && out.contains("demo"),
        "{out}"
    );
    let (_, out, _) = r.run(&["watch", "status"]);
    assert!(
        out.starts_with("hex-watch: 1 pending, 0 firing, 0 failed, 0 expired, 0 done"),
        "{out}"
    );
    assert!(out.contains("last tick never"));
}

#[test]
fn add_validates_source_match_action_and_expires() {
    let r = Rig::new();
    let (rc, _, err) = r.run(&["watch", "add", "--action", "true"]);
    assert_eq!(rc, 2);
    assert!(err.contains("needs --query"), "{err}");
    let (rc, _, err) = r.run(&["watch", "add", "--source", "event", "--action", "true"]);
    assert_eq!(rc, 2);
    assert!(err.contains("needs --match event="), "{err}");
    let (rc, _, err) = r.run(&["watch", "add", "--source", "pigeon", "--action", "true"]);
    assert_eq!(rc, 2);
    assert!(err.contains("unknown source"), "{err}");
    let (rc, _, err) = r.run(&["watch", "add", "--query", "q"]);
    assert_eq!(rc, 2);
    assert!(err.contains("--action CMD or --notify SESSION"), "{err}");
    let (rc, _, err) = r.run(&[
        "watch",
        "add",
        "--query",
        "q",
        "--action",
        "true",
        "--expires",
        "soon",
    ]);
    assert_eq!(rc, 2);
    assert!(err.contains("bad duration"), "{err}");
    let (rc, _, err) = r.run(&[
        "watch", "add", "--query", "q", "--action", "true", "--match", "novalue",
    ]);
    assert_eq!(rc, 2);
    assert!(err.contains("K=V"), "{err}");
    assert!(
        !r.hex().join(".hex/watch/items").exists(),
        "nothing persisted on a rejected add"
    );
}

#[test]
fn tick_fires_once_with_after_epoch_in_the_gmail_query_and_dry_run_runs_nothing() {
    let r = Rig::new();
    let out_file = r.hex().join("out");
    let (_, id, _) = r.run(&[
        "watch",
        "add",
        "--query",
        "from:x",
        "--action",
        &format!(
            "echo $WATCH_KEY $MAIL_SUBJECT $WATCH_NOTE > {}",
            out_file.display()
        ),
        "--note",
        "n",
    ]);
    let id = id.trim().to_string();
    let (rc, out, _) = r.run(&["watch", "tick", "--dry-run"]);
    assert_eq!(rc, 0);
    assert!(out.contains(&format!("DRY-RUN would fire {id}")), "{out}");
    assert!(!out_file.exists());
    assert_eq!(r.item(&id)["status"], "pending");
    let (rc, out, _) = r.run(&["watch", "tick"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("fired 1"), "{out}");
    assert_eq!(std::fs::read_to_string(&out_file).unwrap().trim(), "m1 S n");
    let w = r.item(&id);
    assert_eq!(w["status"], "done");
    assert_eq!(w["key"], "m1");
    let since = chrono::DateTime::parse_from_rfc3339(w["since"].as_str().unwrap())
        .unwrap()
        .timestamp();
    let glog = std::fs::read_to_string(r.hex().join("gmail.log")).unwrap();
    assert!(
        glog.contains(&format!("from:x after:{since} primary")),
        "{glog}"
    );
    let (_, out, _) = r.run(&["watch", "tick"]);
    assert!(
        out.contains("ticked 0 pending"),
        "a done watch is never polled again: {out}"
    );
    let (_, out, _) = r.run(&["watch", "status"]);
    assert!(
        out.contains("1 done") && out.contains("last tick") && !out.contains("never"),
        "{out}"
    );
}

#[test]
fn retry_done_drop_transitions_and_their_refusals() {
    let r = Rig::new();
    let (_, id, _) = r.run(&[
        "watch", "add", "--query", "q", "--action", "exit 7", "--note", "bad",
    ]);
    let id = id.trim().to_string();
    let (rc, out, _) = r.run(&["watch", "tick"]);
    assert_eq!(
        rc, 0,
        "an action failure is a watch outcome, not a tick failure: {out}"
    );
    assert_eq!(r.item(&id)["status"], "failed");
    let (_, out, _) = r.run(&["watch", "list"]);
    assert!(out.contains("FAILED"), "{out}");
    let (rc, _, _) = r.run(&["watch", "retry", &id]);
    assert_eq!(rc, 0);
    let w = r.item(&id);
    assert_eq!(w["status"], "pending");
    assert!(w.get("error").is_none() && w["retried"].is_string());
    let (rc, _, err) = r.run(&["watch", "retry", &id]);
    assert_eq!(rc, 2);
    assert!(
        err.contains("retry needs failed, firing or expired"),
        "{err}"
    );
    let (rc, _, _) = r.run(&["watch", "done", &id]);
    assert_eq!(rc, 0);
    assert_eq!(r.item(&id)["status"], "done");
    let (_, out, _) = r.run(&["watch", "list"]);
    assert_eq!(out.trim(), "no live watches");
    let (rc, _, _) = r.run(&["watch", "drop", &id]);
    assert_eq!(rc, 0);
    assert!(!r
        .hex()
        .join(".hex/watch/items")
        .join(format!("{id}.json"))
        .exists());
    let (rc, _, err) = r.run(&["watch", "drop", &id]);
    assert_eq!(rc, 1);
    assert!(err.contains("no watch"), "{err}");
    let log = std::fs::read_to_string(r.hex().join(".hex/watch/log.jsonl")).unwrap();
    for t in [
        "\"add\"",
        "\"firing\"",
        "\"failed\"",
        "\"retry\"",
        "\"done\"",
        "\"drop\"",
    ] {
        assert!(log.contains(t), "log missing {t}: {log}");
    }
}

#[test]
fn notify_is_the_default_action_inbox_only_and_the_prompt_hook_drains_it_once() {
    let r = Rig::new();
    let (rc, id, err) = r.run(&[
        "watch",
        "add",
        "--query",
        "q",
        "--notify",
        "retreat",
        "--note",
        "rafting mail",
    ]);
    assert_eq!(rc, 0, "{err}");
    let id = id.trim().to_string();
    assert!(r.item(&id)["action"]
        .as_str()
        .unwrap()
        .starts_with("\"$HEX_DIR/.hex/bin/hex\" watch notify 'retreat'"));
    let (rc, out, err) = r.run(&["watch", "tick"]);
    assert_eq!(rc, 0, "{out}\n{err}");
    assert_eq!(r.item(&id)["status"], "done", "{err}");
    let inbox = std::fs::read_to_string(r.hex().join(".hex/run/inbox/retreat.md")).unwrap();
    assert!(
        inbox.contains(&format!("watch {id} fired (gmail m1): rafting mail")),
        "{inbox}"
    );
    assert!(
        !r.hex().join("tmux.log").exists(),
        "default notify never touches tmux"
    );

    // the UserPromptSubmit hook injects and clears it, once
    let hook = |r: &Rig| {
        let path = format!(
            "{}:{}",
            r.hex().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut child = Command::new(hex_bin())
            .args(["hook", "user-prompt-submit"])
            .env("HEX_DIR", r.hex())
            .env("HEX_SESSION_NAME", "retreat")
            .env("PATH", path)
            .env("III_URL", "ws://127.0.0.1:1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"prompt\":\"hi\"}")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let first = hook(&r);
    assert!(
        first.contains("hex events for session \\\"retreat\\\""),
        "{first}"
    );
    assert!(first.contains("rafting mail"), "{first}");
    let second = hook(&r);
    assert!(
        !second.contains("rafting mail"),
        "inbox must drain once: {second}"
    );
}

#[test]
fn notify_now_types_into_a_live_tmux_session_and_inbox_command_drains() {
    let r = Rig::new();
    let (rc, _, err) = r.run(&["watch", "notify", "retreat", "hello", "--now"]);
    assert_eq!(rc, 0, "{err}");
    let tl = std::fs::read_to_string(r.hex().join("tmux.log")).unwrap();
    assert!(tl.contains("has-session -t =retreat"), "{tl}");
    assert!(
        tl.contains("send-keys -t =retreat: -l hex event for this session: hello"),
        "{tl}"
    );
    let (rc, _, err) = r.run(&["watch", "notify", "ghost", "hello", "--now"]);
    assert_eq!(rc, 0);
    assert!(err.contains("no tmux session 'ghost'"), "{err}");
    assert!(r.hex().join(".hex/run/inbox/ghost.md").exists());
    let (rc, out, _) = r.run(&["watch", "inbox", "--session", "retreat"]);
    assert_eq!(rc, 0);
    assert!(out.contains("hello"), "{out}");
    let (_, out, _) = r.run(&["watch", "inbox", "--session", "retreat"]);
    assert_eq!(out, "");
    let (rc, out, _) = r.run(&["watch", "inbox"]);
    assert_eq!((rc, out.as_str()), (0, ""), "no session name = quiet no-op");
}

#[test]
fn v1_python_queue_is_imported_on_first_tick() {
    let r = Rig::new();
    let legacy = r.hex().join(".hex/run/hex-watch/watches.jsonl");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "{\"id\": \"c09961bd\", \"query\": \"from:x\", \"action\": \"true\", \"note\": \"old\", \"status\": \"done\", \"created\": \"2026-09-16T12:07:41-04:00\", \"fired\": \"2026-09-16T12:07:58-04:00\", \"msg_id\": \"1a0aaefb0de09904\"}\n").unwrap();
    let (rc, _, err) = r.run(&["watch", "tick"]);
    assert_eq!(rc, 0, "{err}");
    assert!(err.contains("imported 1 v1 watch record"), "{err}");
    assert_eq!(r.item("c09961bd")["status"], "done");
    assert!(!legacy.exists());
    let (_, out, _) = r.run(&["watch", "list", "--all"]);
    assert!(out.contains("c09961bd"), "{out}");
}
