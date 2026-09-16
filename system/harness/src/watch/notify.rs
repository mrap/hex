//! Session delivery: how a watch outcome (or anything else) reaches a
//! running hex session (spec `docs/hex-watch.md`, "Session delivery").
//!
//! - `notify(session, text)` appends to `$HEX_DIR/.hex/run/inbox/<session>.md`.
//!   The session reads it on its next turn inside a banner that marks the
//!   lines as hex events, not the operator typing.
//! - `--now` additionally types one line into that tmux session. Opt-in
//!   only: typed text arrives as a user prompt (an email subject would
//!   become a prompt), submits anything half-typed in that pane, and
//!   duplicates the inbox line. Use it for text hex controls.
//! - `drain(session)` returns the banner-wrapped inbox and clears it.

use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};

pub fn inbox_dir(hex_dir: &Path) -> PathBuf {
    hex_dir.join(".hex").join("run").join("inbox")
}
pub fn inbox_path(hex_dir: &Path, session: &str) -> PathBuf {
    inbox_dir(hex_dir).join(format!("{session}.md"))
}

/// Runs tmux. Injectable so tests never touch a real server.
pub trait Tmux {
    fn sessions(&self) -> Result<Vec<String>, String>;
    fn has_session(&self, name: &str) -> bool;
    fn type_line(&self, name: &str, line: &str) -> Result<(), String>;
}

pub struct RealTmux;
impl Tmux for RealTmux {
    fn sessions(&self) -> Result<Vec<String>, String> {
        let out = std::process::Command::new("tmux")
            .args(["list-sessions", "-F", "#S"])
            .output()
            .map_err(|e| format!("tmux: {e}"))?;
        if !out.status.success() {
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }
    fn has_session(&self, name: &str) -> bool {
        std::process::Command::new("tmux")
            .args(["has-session", "-t", &format!("={name}")])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    fn type_line(&self, name: &str, line: &str) -> Result<(), String> {
        // `=name:` targets the session's current pane; bare `=name` fails pane lookup.
        let target = format!("={name}:");
        let a = std::process::Command::new("tmux")
            .args(["send-keys", "-t", &target, "-l", line])
            .output()
            .map_err(|e| format!("tmux send-keys: {e}"))?;
        if !a.status.success() {
            return Err(format!(
                "tmux send-keys to {name} failed: {}",
                String::from_utf8_lossy(&a.stderr).trim()
            ));
        }
        let b = std::process::Command::new("tmux")
            .args(["send-keys", "-t", &target, "Enter"])
            .output()
            .map_err(|e| format!("tmux send-keys Enter: {e}"))?;
        if !b.status.success() {
            return Err(format!("tmux send-keys Enter to {name} failed"));
        }
        Ok(())
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct NotifyReport {
    pub inboxed: Vec<String>,
    pub typed: Vec<String>,
    /// Sessions asked for `--now` that have no tmux session (inbox only).
    pub not_running: Vec<String>,
    pub errors: Vec<String>,
}

/// Deliver `text` to `target` (`all` = every live tmux session).
pub fn notify(
    hex_dir: &Path,
    tmux: &dyn Tmux,
    target: &str,
    text: &str,
    now: bool,
) -> Result<NotifyReport, String> {
    use std::io::Write;
    let targets: Vec<String> = if target == "all" {
        let s = tmux.sessions()?;
        if s.is_empty() {
            return Err("no tmux sessions to notify".to_string());
        }
        s
    } else {
        vec![target.to_string()]
    };
    let dir = inbox_dir(hex_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut rep = NotifyReport::default();
    for s in targets {
        let p = inbox_path(hex_dir, &s);
        let res = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .and_then(|mut f| writeln!(f, "- {ts} {text}"));
        match res {
            Ok(()) => rep.inboxed.push(s.clone()),
            Err(e) => {
                rep.errors
                    .push(format!("cannot write {}: {e}", p.display()));
                continue;
            }
        }
        if now {
            if tmux.has_session(&s) {
                match tmux.type_line(&s, &format!("hex event for this session: {text}")) {
                    Ok(()) => rep.typed.push(s.clone()),
                    Err(e) => rep.errors.push(e),
                }
            } else {
                rep.not_running.push(s.clone());
            }
        }
    }
    Ok(rep)
}

/// The session's pending events, banner-wrapped, or `None` when empty.
/// Clears the inbox unless `peek`.
pub fn drain(hex_dir: &Path, session: &str, peek: bool) -> Result<Option<String>, String> {
    let p = inbox_path(hex_dir, session);
    let body = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", p.display())),
    };
    if body.trim().is_empty() {
        return Ok(None);
    }
    if !peek {
        std::fs::write(&p, "").map_err(|e| format!("clear {}: {e}", p.display()))?;
    }
    Ok(Some(format!(
        "\n*** hex events for session \"{session}\" (act on these; they are consumed now) ***\n{}*** End hex events ***\n",
        if body.ends_with('\n') { body } else { format!("{body}\n") }
    )))
}

/// The current session's name: `HEX_SESSION_NAME`, else the tmux session
/// this process runs inside, else `None`.
pub fn current_session() -> Option<String> {
    if let Ok(n) = std::env::var("HEX_SESSION_NAME") {
        if !n.trim().is_empty() {
            return Some(n);
        }
    }
    if std::env::var_os("TMUX").is_some() {
        if let Ok(o) = std::process::Command::new("tmux")
            .args(["display-message", "-p", "#S"])
            .output()
        {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if o.status.success() && !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeTmux {
        live: Vec<String>,
        typed: RefCell<Vec<(String, String)>>,
    }
    impl Tmux for FakeTmux {
        fn sessions(&self) -> Result<Vec<String>, String> {
            Ok(self.live.clone())
        }
        fn has_session(&self, name: &str) -> bool {
            self.live.iter().any(|s| s == name)
        }
        fn type_line(&self, name: &str, line: &str) -> Result<(), String> {
            self.typed.borrow_mut().push((name.into(), line.into()));
            Ok(())
        }
    }

    #[test]
    fn default_notify_is_inbox_only_and_drain_consumes_once() {
        let t = tempfile::TempDir::new().unwrap();
        let tm = FakeTmux {
            live: vec!["retreat".into()],
            typed: RefCell::new(vec![]),
        };
        let rep = notify(t.path(), &tm, "retreat", "rafting mail", false).unwrap();
        assert_eq!(rep.inboxed, vec!["retreat"]);
        assert!(rep.typed.is_empty());
        assert!(
            tm.typed.borrow().is_empty(),
            "nothing typed into the pane by default"
        );
        let text = drain(t.path(), "retreat", false).unwrap().unwrap();
        assert!(text.contains("hex events for session \"retreat\""));
        assert!(text.contains("rafting mail"));
        assert!(drain(t.path(), "retreat", false).unwrap().is_none());
    }

    #[test]
    fn now_types_into_a_live_session_and_falls_back_to_inbox_for_a_missing_one() {
        let t = tempfile::TempDir::new().unwrap();
        let tm = FakeTmux {
            live: vec!["retreat".into()],
            typed: RefCell::new(vec![]),
        };
        let rep = notify(t.path(), &tm, "retreat", "hello", true).unwrap();
        assert_eq!(rep.typed, vec!["retreat"]);
        assert_eq!(
            tm.typed.borrow()[0],
            (
                "retreat".to_string(),
                "hex event for this session: hello".to_string()
            )
        );
        let rep = notify(t.path(), &tm, "ghost", "hello", true).unwrap();
        assert_eq!(rep.not_running, vec!["ghost"]);
        assert!(inbox_path(t.path(), "ghost").exists());
        assert!(rep.errors.is_empty());
    }

    #[test]
    fn all_targets_every_live_session_and_peek_does_not_clear() {
        let t = tempfile::TempDir::new().unwrap();
        let tm = FakeTmux {
            live: vec!["a".into(), "b".into()],
            typed: RefCell::new(vec![]),
        };
        let rep = notify(t.path(), &tm, "all", "x", false).unwrap();
        assert_eq!(rep.inboxed, vec!["a", "b"]);
        assert!(drain(t.path(), "a", true).unwrap().is_some());
        assert!(drain(t.path(), "a", true).unwrap().is_some());
        let none = FakeTmux {
            live: vec![],
            typed: RefCell::new(vec![]),
        };
        assert!(notify(t.path(), &none, "all", "x", false).is_err());
    }
}
