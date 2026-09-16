//! The watch loop: one `tick` over every pending watch.
//!
//! Per watch, in order (spec `docs/hex-watch.md`, section 3):
//! 1. config check (known source, parseable match) — a config error fails
//!    that watch once, alerts once, and stays OUT of the poll streak
//! 2. expiry — `expired` + alert + hitl item + `hex.watch.expired`, once
//! 3. poll the source — transport errors feed the tick-level streak
//! 4. since guard — first hit with `at_ms >= since` (unknown time is trusted)
//! 5. claim: save `firing` BEFORE the action so a crash cannot refire
//! 6. run the action with an allowlisted env; `done` or `failed`
//! 7. emit `hex.watch.<outcome>` (loud on failure, never blocks)
//!
//! Streak: counts whole ticks with any poll failure, alerts once at 3, resets
//! (and clears the alert stamp) on a clean tick.
//!
//! Everything external goes through [`Env`]: [`Substrate`] (iii state +
//! emit), [`Shell`] (gmail command + action), [`Alerter`] (banner/push +
//! hitl). The worker and CLI pass real impls; tests pass fakes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::sources::{self, Hit};
use super::store::{self, Status, Watch};
use super::Config;

// ---------------------------------------------------------------------------
// Seams
// ---------------------------------------------------------------------------

pub trait Substrate {
    /// Newest envelope at iii state `events/<name>`, or `None` when nothing
    /// was ever emitted under that name.
    fn get_event(&self, name: &str) -> Result<Option<Value>, String>;
    fn emit(&self, event: &str, data: Value) -> Result<(), String>;
}

pub struct ShellOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait Shell {
    /// Run `sh -c cmd` with EXACTLY `env` (no inherited variables), killed
    /// after `timeout` (exit 124).
    fn run(
        &self,
        cmd: &str,
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<ShellOutput, String>;
}

pub trait Alerter {
    /// Banner + push rail, deduped per key by the alert module.
    fn alert(&self, key: &str, title: &str, msg: &str);
    fn clear(&self, key: &str);
    /// File a pending-human-action item (expiry path).
    fn hitl(&self, title: &str, body: &str) -> Result<(), String>;
}

pub struct Env<'a> {
    pub hex_dir: PathBuf,
    pub config: Config,
    pub substrate: &'a dyn Substrate,
    pub shell: &'a dyn Shell,
    pub alerter: &'a dyn Alerter,
    /// Parent-process env, filtered through the allowlist for actions.
    pub parent_env: Vec<(String, String)>,
    pub dry_run: bool,
    /// Where log lines go (stderr in production; captured in tests).
    pub log: &'a dyn Fn(&str),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub pending: usize,
    pub fired: usize,
    pub failed: usize,
    pub expired: usize,
    pub poll_failures: usize,
    pub streak: u32,
    /// `--dry-run`: what would have fired.
    pub would_fire: Vec<String>,
}

// ---------------------------------------------------------------------------
// Env allowlist
// ---------------------------------------------------------------------------

const ENV_ALLOW: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "TMPDIR",
    "GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND",
];

/// The env an action (or the gmail command) sees: the fixed allowlist plus
/// `env_passthrough`, plus `HEX_DIR`, plus whatever the caller adds. Nothing
/// else from the daemon leaks into a shell string (CTO review risk 8).
pub fn build_env(
    parent: &[(String, String)],
    passthrough: &[String],
    hex_dir: &Path,
    extra: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = parent
        .iter()
        .filter(|(k, _)| ENV_ALLOW.contains(&k.as_str()) || passthrough.iter().any(|p| p == k))
        .cloned()
        .collect();
    if !out
        .iter()
        .any(|(k, _)| k == "GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND")
    {
        out.push(("GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND".into(), "file".into()));
    }
    out.retain(|(k, _)| k != "HEX_DIR");
    out.push(("HEX_DIR".into(), hex_dir.display().to_string()));
    out.extend(extra);
    out
}

fn env_key(field: &str) -> String {
    let up: String = field
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("WATCH_{up}")
}

/// `WATCH_*` (+ `MAIL_*` for gmail) for one hit.
pub fn hit_env(w: &Watch, hit: &Hit) -> Vec<(String, String)> {
    let mut v = vec![
        ("WATCH_ID".to_string(), w.id.clone()),
        ("WATCH_SOURCE".to_string(), w.source.clone()),
        ("WATCH_KEY".to_string(), hit.key.clone()),
        (
            "WATCH_AT".to_string(),
            if hit.at_ms > 0 {
                hit.at_ms.to_string()
            } else {
                String::new()
            },
        ),
        ("WATCH_NOTE".to_string(), w.note.clone()),
    ];
    for (k, val) in &hit.fields {
        v.push((env_key(k), val.clone()));
    }
    if w.source == "gmail" {
        v.extend(sources::gmail_aliases(hit));
    }
    v
}

// ---------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------

fn outcome_data(w: &Watch, outcome: &str, hit: Option<&Hit>, error: Option<&str>) -> Value {
    let mut d = json!({
        "id": w.id, "note": w.note, "source": w.source, "match": w.r#match, "outcome": outcome,
        "since": w.since.map(store::fmt_ts), "expires": w.expires.map(store::fmt_ts),
    });
    if let Some(h) = hit {
        d["hit"] = json!({"key": h.key, "at_ms": h.at_ms, "fields": h.fields});
    }
    if let Some(e) = error {
        d["error"] = json!(e);
    }
    d
}

fn emit_outcome(env: &Env, w: &Watch, outcome: &str, hit: Option<&Hit>, error: Option<&str>) {
    let name = format!("hex.watch.{outcome}");
    match env
        .substrate
        .emit(&name, outcome_data(w, outcome, hit, error))
    {
        Ok(()) => (env.log)(&format!("{}: emitted {name}", w.id)),
        Err(e) => (env.log)(&format!("ERROR {}: emit {name} failed: {e}", w.id)),
    }
}

fn label(w: &Watch) -> String {
    if w.note.is_empty() {
        w.id.clone()
    } else {
        w.note.clone()
    }
}

/// Poll one watch. `Err` = transport error (feeds the streak).
fn poll(env: &Env, w: &Watch) -> Result<Vec<Hit>, String> {
    match w.source.as_str() {
        "gmail" => {
            let account = sources::gmail_account(&w.r#match);
            let query = sources::gmail_query(&w.r#match, w.since)?;
            let cmd = sources::gmail_command(&env.config.sources.gmail.command, &query, &account);
            let shell_env = build_env(
                &env.parent_env,
                &env.config.action.env_passthrough,
                &env.hex_dir,
                vec![],
            );
            let out = env.shell.run(&cmd, &shell_env, Duration::from_secs(60))?;
            if out.code != 0 {
                return Err(format!(
                    "gmail command rc={}: {}",
                    out.code,
                    tail(&out.stderr, 300)
                ));
            }
            let hits = sources::parse_gmail_output(&out.stdout, &account)?;
            if let Some(h) = hits.iter().find(|h| h.at_ms <= 0) {
                // Gmail always has internalDate; a missing one is a broken
                // adapter, and trusting it would defeat the since guard.
                return Err(format!(
                    "gmail hit {} has no internal_ms; refusing to trust it",
                    h.key
                ));
            }
            Ok(hits)
        }
        "event" => {
            let name = sources::event_name(&w.r#match)?;
            Ok(env
                .substrate
                .get_event(&name)?
                .map(|v| vec![sources::hit_from_envelope(&name, &v)])
                .unwrap_or_default())
        }
        other => Err(format!("unknown source {other:?}")),
    }
}

/// Config errors are permanent for that watch; they must never feed the
/// streak. Checked before any transport happens.
fn config_error(env: &Env, w: &Watch) -> Option<String> {
    match w.source.as_str() {
        "gmail" => sources::gmail_query(&w.r#match, None).err(),
        "event" => sources::event_name(&w.r#match).err(),
        other => Some(format!("unknown source {other:?}")),
    }
    .or_else(|| {
        if env.config.sources.gmail.command.trim().is_empty() && w.source == "gmail" {
            Some("sources.gmail.command is empty in watch.toml".to_string())
        } else {
            None
        }
    })
}

/// First hit at or after `since`. Unknown time (0) is trusted and logged.
pub fn first_new_hit(w: &Watch, hits: &[Hit], log: &dyn Fn(&str)) -> Option<Hit> {
    let since_ms = match w.since {
        Some(t) => t.timestamp_millis(),
        None => return hits.first().cloned(),
    };
    for h in hits {
        if h.at_ms <= 0 {
            log(&format!(
                "WARN {}: hit {} has no timestamp; trusting the adapter's since filter",
                w.id, h.key
            ));
            return Some(h.clone());
        }
        if h.at_ms >= since_ms {
            return Some(h.clone());
        }
        log(&format!(
            "{}: hit {} is older than since ({} < {since_ms}); ignored",
            w.id, h.key, h.at_ms
        ));
    }
    None
}

fn tail(s: &str, n: usize) -> String {
    let t = s.trim();
    let chars: Vec<char> = t.chars().collect();
    if chars.len() <= n {
        t.to_string()
    } else {
        chars[chars.len() - n..].iter().collect()
    }
}

/// One pass. Returns the report; `Err` only when the store itself is broken
/// (unreadable items or state), which the worker turns into an error row.
pub fn run(env: &Env, now: DateTime<Utc>) -> Result<TickReport, String> {
    use fs2::FileExt;
    let hex_dir = env.hex_dir.as_path();
    // One tick at a time per HEX_DIR: a hand-run `hex watch tick` racing the
    // cron worker would otherwise both claim and fire the same watch
    // (adversarial review 2026-09-16, finding 1). The lock lives for the
    // whole pass; a second caller fails loudly instead of waiting.
    std::fs::create_dir_all(store::watch_dir(hex_dir))
        .map_err(|e| format!("watch: mkdir {}: {e}", store::watch_dir(hex_dir).display()))?;
    let lock_path = store::watch_dir(hex_dir).join("tick.lock");
    let lock = std::fs::File::create(&lock_path)
        .map_err(|e| format!("watch: open {}: {e}", lock_path.display()))?;
    // Short grace: a forked child of another thread can hold a dup of the
    // lock fd for a few ms before it execs (CLOEXEC then drops it).
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(e) => {
                return Err(format!(
                    "another tick is already running for this HEX_DIR (tick.lock held: {e}); not racing it"
                ))
            }
        }
    }
    let imported = store::import_v1(hex_dir, now)?;
    if imported > 0 {
        (env.log)(&format!(
            "imported {imported} v1 watch record(s) from the Python queue"
        ));
    }
    let mut state = store::load_state(hex_dir)?;
    let all = store::load_all(hex_dir)?;
    let pending: Vec<Watch> = all
        .into_iter()
        .filter(|w| w.status == Status::Pending)
        .collect();
    let mut report = TickReport {
        pending: pending.len(),
        ..Default::default()
    };
    let mut tick_failed = false;

    for mut w in pending {
        if let Some(e) = config_error(env, &w) {
            w.status = Status::Failed;
            w.error = Some(format!("config: {e}"));
            store::save(hex_dir, &w)?;
            store::log(hex_dir, &w.id, "failed", now, Some(&format!("config: {e}")))?;
            (env.log)(&format!("{}: CONFIG ERROR, watch failed: {e}", w.id));
            env.alerter.alert(
                &format!("watch-{}-config", w.id),
                "hex watch: config error",
                &format!("watch {} has a config error and was failed: {e}", label(&w)),
            );
            emit_outcome(env, &w, "failed", None, Some(&format!("config: {e}")));
            report.failed += 1;
            continue;
        }
        if let Some(exp) = w.expires {
            if now >= exp {
                w.status = Status::Expired;
                w.expired_at = Some(now);
                store::save(hex_dir, &w)?;
                store::log(hex_dir, &w.id, "expired", now, None)?;
                (env.log)(&format!(
                    "{}: EXPIRED without a match (since {}, expires {})",
                    w.id,
                    w.since.map(store::fmt_ts).unwrap_or_default(),
                    store::fmt_ts(exp)
                ));
                env.alerter.alert(
                    &format!("watch-{}-expired", w.id),
                    "hex watch: expired",
                    &format!(
                        "watch expired with no match: {}. If still wanted, re-add it with a longer --expires",
                        label(&w)
                    ),
                );
                emit_outcome(env, &w, "expired", None, None);
                let body = format!(
                    "Watch `{}` ({} {}) waited from {} to {} and nothing matched.\n\nDecide: re-add with a longer window (`hex watch add ... --expires 30d`), chase the sender, or drop it.\nAction it would have run: `{}`",
                    w.id,
                    w.source,
                    serde_json::to_string(&w.r#match).unwrap_or_default(),
                    w.since.map(store::fmt_ts).unwrap_or_default(),
                    store::fmt_ts(exp),
                    w.action
                );
                if let Err(e) = env
                    .alerter
                    .hitl(&format!("hex-watch expired: {}", label(&w)), &body)
                {
                    (env.log)(&format!("ERROR {}: hitl item not filed: {e}", w.id));
                }
                report.expired += 1;
                continue;
            }
        }
        let hits = match poll(env, &w) {
            Ok(h) => h,
            Err(e) => {
                tick_failed = true;
                report.poll_failures += 1;
                (env.log)(&format!(
                    "ERROR poll failed for {} ({}): {e}",
                    w.id, w.source
                ));
                continue;
            }
        };
        let hit = match first_new_hit(&w, &hits, env.log) {
            Some(h) => h,
            None => {
                (env.log)(&format!(
                    "{}: no match yet ({} {})",
                    w.id,
                    w.source,
                    serde_json::to_string(&w.r#match).unwrap_or_default()
                ));
                continue;
            }
        };
        (env.log)(&format!("{}: MATCH {} | {}", w.id, hit.key, describe(&hit)));
        if env.dry_run {
            report.would_fire.push(format!(
                "{}: {}  ({}: {})",
                w.id,
                w.action,
                hit.key,
                describe(&hit)
            ));
            continue;
        }
        // Claim BEFORE running the action: a crash mid-action leaves `firing`,
        // which is never re-run (list flags it, retry is the human path back).
        w.status = Status::Firing;
        w.fired = Some(now);
        w.key = Some(hit.key.clone());
        store::save(hex_dir, &w)?;
        store::log(hex_dir, &w.id, "firing", now, Some(&hit.key))?;

        let action_env = build_env(
            &env.parent_env,
            &env.config.action.env_passthrough,
            hex_dir,
            hit_env(&w, &hit),
        );
        let timeout = Duration::from_secs(env.config.action_timeout_secs);
        let out = match env.shell.run(&w.action, &action_env, timeout) {
            Ok(o) => o,
            Err(e) => ShellOutput {
                code: 127,
                stdout: String::new(),
                stderr: e,
            },
        };
        if out.code == 0 {
            w.status = Status::Done;
            store::save(hex_dir, &w)?;
            store::log(hex_dir, &w.id, "done", now, None)?;
            (env.log)(&format!("{}: action OK\n{}", w.id, tail(&out.stdout, 500)));
            emit_outcome(env, &w, "fired", Some(&hit), None);
            report.fired += 1;
        } else {
            let err = tail(&out.stderr, 800);
            w.status = Status::Failed;
            w.error = Some(err.clone());
            store::save(hex_dir, &w)?;
            store::log(
                hex_dir,
                &w.id,
                "failed",
                now,
                Some(&format!("rc={}", out.code)),
            )?;
            (env.log)(&format!("{}: ACTION FAILED rc={}\n{err}", w.id, out.code));
            env.alerter.alert(
                &format!("watch-{}-failed", w.id),
                "hex watch: action failed",
                &format!(
                    "action failed for {} (rc={}); see harness log",
                    label(&w),
                    out.code
                ),
            );
            emit_outcome(env, &w, "failed", Some(&hit), Some(&tail(&err, 300)));
            report.failed += 1;
        }
    }

    if tick_failed {
        state.poll_fail_streak += 1;
        if state.poll_fail_streak == 3 {
            env.alerter.alert(
                "watch-poll-streak",
                "hex watch: polling failing",
                "source polling failing 3 ticks in a row; watches are blind",
            );
        }
    } else {
        if state.poll_fail_streak >= 3 {
            (env.log)("source polling recovered");
            env.alerter.clear("watch-poll-streak");
        }
        state.poll_fail_streak = 0;
    }
    state.last_tick = Some(now);
    store::save_state(hex_dir, &state)?;
    report.streak = state.poll_fail_streak;
    Ok(report)
}

pub fn describe(hit: &Hit) -> String {
    let parts: Vec<&str> = hit.fields.values().take(4).map(|s| s.as_str()).collect();
    if parts.is_empty() {
        hit.key.clone()
    } else {
        parts.join(" | ")
    }
}

// ---------------------------------------------------------------------------
// Real impls
// ---------------------------------------------------------------------------

/// iii through the `ops` seam (CLI path).
pub struct OpsSubstrate;
impl Substrate for OpsSubstrate {
    fn get_event(&self, name: &str) -> Result<Option<Value>, String> {
        crate::ops::state_get("events", name)
    }
    fn emit(&self, event: &str, data: Value) -> Result<(), String> {
        crate::ops::emit(event, data, Some("hex-watch"))
    }
}

/// `sh -c` with an explicit env and a hard timeout. Output goes through temp
/// files so a chatty action cannot deadlock a pipe while we poll for exit.
pub struct ShShell;
impl Shell for ShShell {
    fn run(
        &self,
        cmd: &str,
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<ShellOutput, String> {
        use std::process::{Command, Stdio};
        let dir = tempfile::TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
        let out_p = dir.path().join("out");
        let err_p = dir.path().join("err");
        let out_f = std::fs::File::create(&out_p)
            .map_err(|e| format!("create {}: {e}", out_p.display()))?;
        let err_f = std::fs::File::create(&err_p)
            .map_err(|e| format!("create {}: {e}", err_p.display()))?;
        #[allow(unused_imports)]
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(cmd)
            // own process group, so a timeout kills grandchildren too
            // (an action that shells out to `gws` must not leave it running)
            .process_group(0)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::from(out_f))
            .stderr(Stdio::from(err_f));
        let mut child = command.spawn().map_err(|e| format!("spawn sh: {e}"))?;
        let start = std::time::Instant::now();
        let code = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st.code().unwrap_or(128 + 9),
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        // SAFETY: plain libc call on a pgid we created above.
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        let mut o = ShellOutput {
                            code: 124,
                            stdout: String::new(),
                            stderr: String::new(),
                        };
                        o.stdout = std::fs::read_to_string(&out_p).unwrap_or_default();
                        o.stderr = format!(
                            "{}\naction timed out after {}s",
                            std::fs::read_to_string(&err_p).unwrap_or_default(),
                            timeout.as_secs()
                        );
                        return Ok(o);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(format!("wait: {e}")),
            }
        };
        Ok(ShellOutput {
            code,
            stdout: std::fs::read_to_string(&out_p).unwrap_or_default(),
            stderr: std::fs::read_to_string(&err_p).unwrap_or_default(),
        })
    }
}

/// Banner/push via `alert::notify_at`, hitl via `hitl::store::create`.
pub struct RealAlerter {
    pub hex_dir: PathBuf,
}
impl Alerter for RealAlerter {
    fn alert(&self, key: &str, title: &str, msg: &str) {
        crate::alert::notify_at(&self.hex_dir, key, title, msg);
    }
    fn clear(&self, key: &str) {
        crate::alert::clear_at(&self.hex_dir, key);
    }
    fn hitl(&self, title: &str, body: &str) -> Result<(), String> {
        crate::hitl::store::create(
            &self.hex_dir,
            crate::hitl::store::NewItem {
                title: title.to_string(),
                project: "hex-ops".to_string(),
                body: body.to_string(),
                priority: Some(crate::hitl::store::Priority::P2),
                deadline: None,
                est_minutes: None,
                depends_on: Vec::new(),
            },
            Utc::now(),
        )
        .map(|_| ())
    }
}

pub fn parent_env() -> Vec<(String, String)> {
    std::env::vars().collect()
}

pub fn log_stderr(msg: &str) {
    eprintln!("{} hex-watch: {msg}", store::fmt_ts(Utc::now()));
}

// ---------------------------------------------------------------------------
// Tests: the loop against fakes
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    pub struct FakeSubstrate {
        pub events: RefCell<BTreeMap<String, Value>>,
        pub emitted: RefCell<Vec<(String, Value)>>,
        pub fail_emit: bool,
    }
    impl Substrate for FakeSubstrate {
        fn get_event(&self, name: &str) -> Result<Option<Value>, String> {
            Ok(self.events.borrow().get(name).cloned())
        }
        fn emit(&self, event: &str, data: Value) -> Result<(), String> {
            if self.fail_emit {
                return Err("engine down".into());
            }
            self.emitted.borrow_mut().push((event.to_string(), data));
            Ok(())
        }
    }

    /// Gmail command behavior by `mode`; actions run through the real `sh`.
    pub struct FakeShell {
        pub mode: RefCell<String>,
        pub gmail_calls: RefCell<Vec<String>>,
        pub real: ShShell,
    }
    impl FakeShell {
        pub fn new(mode: &str) -> Self {
            FakeShell {
                mode: RefCell::new(mode.into()),
                gmail_calls: RefCell::new(vec![]),
                real: ShShell,
            }
        }
    }
    impl Shell for FakeShell {
        fn run(
            &self,
            cmd: &str,
            env: &[(String, String)],
            timeout: Duration,
        ) -> Result<ShellOutput, String> {
            if cmd.starts_with("GMAILSTUB") {
                self.gmail_calls.borrow_mut().push(cmd.to_string());
                let now_ms = Utc::now().timestamp_millis();
                let (code, stdout, stderr) = match self.mode.borrow().as_str() {
                    "fail" => (1, String::new(), "boom".to_string()),
                    "hit" => (0, format!("{{\"account\":\"me@example.com\",\"id\":\"m1\",\"internal_ms\":{now_ms},\"date\":\"D\",\"from\":\"a@b\",\"subject\":\"Subj\"}}\n"), String::new()),
                    "old" => (0, "{\"account\":\"me@example.com\",\"id\":\"m0\",\"internal_ms\":1000000000000,\"date\":\"D\",\"from\":\"a@b\",\"subject\":\"Old\"}\n".to_string(), String::new()),
                    "nots" => (0, "{\"account\":\"me@example.com\",\"id\":\"m9\",\"date\":\"D\",\"from\":\"a@b\",\"subject\":\"NoTs\"}\n".to_string(), String::new()),
                    _ => (0, String::new(), String::new()),
                };
                return Ok(ShellOutput {
                    code,
                    stdout,
                    stderr,
                });
            }
            self.real.run(cmd, env, timeout)
        }
    }

    #[derive(Default)]
    pub struct FakeAlerter {
        pub alerts: RefCell<Vec<(String, String)>>,
        pub cleared: RefCell<Vec<String>>,
        pub hitl: RefCell<Vec<String>>,
    }
    impl Alerter for FakeAlerter {
        fn alert(&self, key: &str, _title: &str, msg: &str) {
            self.alerts
                .borrow_mut()
                .push((key.to_string(), msg.to_string()));
        }
        fn clear(&self, key: &str) {
            self.cleared.borrow_mut().push(key.to_string());
        }
        fn hitl(&self, title: &str, _body: &str) -> Result<(), String> {
            self.hitl.borrow_mut().push(title.to_string());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::*;
    use super::*;
    use chrono::Duration as CDur;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    struct Rig {
        dir: tempfile::TempDir,
        sub: FakeSubstrate,
        shell: FakeShell,
        al: FakeAlerter,
        logs: RefCell<Vec<String>>,
        parent: Vec<(String, String)>,
    }
    impl Rig {
        fn new(mode: &str) -> Self {
            Rig {
                dir: tempfile::TempDir::new().unwrap(),
                sub: FakeSubstrate::default(),
                shell: FakeShell::new(mode),
                al: FakeAlerter::default(),
                logs: RefCell::new(vec![]),
                parent: vec![
                    ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
                    ("HOME".into(), std::env::var("HOME").unwrap_or_default()),
                    ("WATCH_TEST_SECRET".into(), "leak".into()),
                ],
            }
        }
        fn hex(&self) -> &Path {
            self.dir.path()
        }
        fn tick_at(&self, now: DateTime<Utc>, dry: bool) -> TickReport {
            let log = |m: &str| self.logs.borrow_mut().push(m.to_string());
            let mut config = Config::default();
            config.sources.gmail.command = "GMAILSTUB --account {account} --json {query} 5".into();
            config.action_timeout_secs = 5;
            let env = Env {
                hex_dir: self.hex().to_path_buf(),
                config,
                substrate: &self.sub,
                shell: &self.shell,
                alerter: &self.al,
                parent_env: self.parent.clone(),
                dry_run: dry,
                log: &log,
            };
            run(&env, now).unwrap()
        }
        fn tick(&self) -> TickReport {
            self.tick_at(Utc::now(), false)
        }
        fn add(&self, action: &str) -> Watch {
            self.add_with(
                action,
                "gmail",
                &[("query", "q"), ("account", "primary")],
                "n",
                None,
                CDur::days(14),
            )
        }
        fn add_with(
            &self,
            action: &str,
            source: &str,
            m: &[(&str, &str)],
            note: &str,
            since: Option<DateTime<Utc>>,
            expires_in: CDur,
        ) -> Watch {
            let mut map = BTreeMap::new();
            for (k, v) in m {
                map.insert(k.to_string(), v.to_string());
            }
            store::create(
                self.hex(),
                store::NewWatch {
                    source: source.into(),
                    r#match: map,
                    action: action.into(),
                    note: note.into(),
                    since,
                    expires_in,
                },
                Utc::now(),
            )
            .unwrap()
        }
        fn get(&self, id: &str) -> Watch {
            store::load(self.hex(), id).unwrap().unwrap()
        }
        fn alerts_with(&self, s: &str) -> usize {
            self.al
                .alerts
                .borrow()
                .iter()
                .filter(|(_, m)| m.contains(s))
                .count()
        }
        fn emitted(&self, name: &str) -> Vec<Value> {
            self.sub
                .emitted
                .borrow()
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, d)| d.clone())
                .collect()
        }
        fn logs_contain(&self, s: &str) -> bool {
            self.logs.borrow().iter().any(|l| l.contains(s))
        }
    }

    #[test]
    fn alert_fires_once_after_three_failed_ticks_even_with_two_watches_and_resets_on_clean() {
        let r = Rig::new("fail");
        r.add("true");
        r.add("true");
        for _ in 0..4 {
            let rep = r.tick();
            assert_eq!(rep.poll_failures, 2);
        }
        assert_eq!(r.alerts_with("failing 3 ticks"), 1);
        assert_eq!(store::load_state(r.hex()).unwrap().poll_fail_streak, 4);
        *r.shell.mode.borrow_mut() = "none".into();
        r.tick();
        assert_eq!(store::load_state(r.hex()).unwrap().poll_fail_streak, 0);
        assert_eq!(r.al.cleared.borrow().as_slice(), ["watch-poll-streak"]);
        *r.shell.mode.borrow_mut() = "fail".into();
        r.tick();
        r.tick();
        assert_eq!(
            r.alerts_with("failing 3 ticks"),
            1,
            "two failures after a reset must not alert"
        );
    }

    #[test]
    fn watch_is_claimed_before_the_action_so_a_crash_cannot_refire() {
        let r = Rig::new("hit");
        let marker = r.dir.path().join("ran");
        // kill our own shell mid-action: the tick's save(firing) already happened
        let w = r.add(&format!("echo x >> {}; kill -9 $$", marker.display()));
        r.tick();
        assert_eq!(
            r.get(&w.id).status,
            Status::Failed,
            "killed action is a failed action"
        );
        // simulate the harness dying mid-action instead: force the record back to firing
        let mut f = r.get(&w.id);
        f.status = Status::Firing;
        store::save(r.hex(), &f).unwrap();
        r.tick();
        assert_eq!(
            std::fs::read_to_string(&marker)
                .unwrap()
                .matches('x')
                .count(),
            1
        );
        assert_eq!(r.get(&w.id).status, Status::Firing);
    }

    #[test]
    fn action_failure_marks_failed_alerts_and_siblings_still_fire() {
        let r = Rig::new("hit");
        let bad = r.add_with(
            "exit 7",
            "gmail",
            &[("query", "q")],
            "n1",
            None,
            CDur::days(1),
        );
        let good = r.add("true");
        let rep = r.tick();
        assert_eq!((rep.failed, rep.fired), (1, 1));
        let b = r.get(&bad.id);
        assert_eq!(b.status, Status::Failed);
        assert_eq!(r.get(&good.id).status, Status::Done);
        assert_eq!(r.alerts_with("action failed for n1"), 1);
        assert_eq!(r.emitted("hex.watch.failed").len(), 1);
        assert_eq!(r.emitted("hex.watch.failed")[0]["note"], "n1");
    }

    #[test]
    fn success_sets_key_and_env_is_allowlisted() {
        let r = Rig::new("hit");
        let out = r.dir.path().join("env");
        let w = r.add(&format!("env > {}", out.display()));
        r.tick();
        let w2 = r.get(&w.id);
        assert_eq!((w2.status, w2.key.as_deref()), (Status::Done, Some("m1")));
        let env = std::fs::read_to_string(&out).unwrap();
        assert!(env.contains("WATCH_SOURCE=gmail\n"), "{env}");
        assert!(env.contains("WATCH_KEY=m1\n"));
        assert!(env.contains("WATCH_SUBJECT=Subj\n"));
        assert!(env.contains("MAIL_MSG_ID=m1\n"));
        assert!(env.contains("MAIL_SUBJECT=Subj\n"));
        assert!(env.contains("GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND=file\n"));
        assert!(env.contains(&format!("HEX_DIR={}\n", r.hex().display())));
        assert!(
            !env.contains("WATCH_TEST_SECRET"),
            "parent secrets must not reach the action: {env}"
        );
        let fired = r.emitted("hex.watch.fired");
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0]["hit"]["key"], "m1");
    }

    #[test]
    fn env_passthrough_extends_the_allowlist() {
        let parent = vec![
            ("SECRET".to_string(), "x".to_string()),
            ("OK_VAR".to_string(), "y".to_string()),
        ];
        let env = build_env(&parent, &["OK_VAR".to_string()], Path::new("/h"), vec![]);
        assert!(env.iter().any(|(k, v)| k == "OK_VAR" && v == "y"));
        assert!(!env.iter().any(|(k, _)| k == "SECRET"));
        assert!(env.iter().any(|(k, v)| k == "HEX_DIR" && v == "/h"));
    }

    #[test]
    fn after_epoch_and_account_reach_the_gmail_command_and_old_mail_does_not_fire() {
        let r = Rig::new("old");
        let marker = r.dir.path().join("ran");
        let w = r.add_with(
            &format!("touch {}", marker.display()),
            "gmail",
            &[("query", "q"), ("account", "legacy")],
            "",
            None,
            CDur::days(1),
        );
        r.tick();
        let calls = r.shell.gmail_calls.borrow();
        let since = r.get(&w.id).since.unwrap().timestamp();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].contains(&format!("'q after:{since}'")),
            "{}",
            calls[0]
        );
        assert!(calls[0].contains("--account 'legacy'"));
        assert_eq!(r.get(&w.id).status, Status::Pending);
        assert!(!marker.exists());
        assert!(r.logs_contain("older than since"));
    }

    #[test]
    fn expired_watch_is_closed_once_with_alert_hitl_and_event_and_is_not_polled() {
        let r = Rig::new("hit");
        let w = r.add_with(
            "true",
            "gmail",
            &[("query", "q")],
            "rafting",
            None,
            CDur::seconds(1),
        );
        let later = Utc::now() + CDur::seconds(5);
        let rep = r.tick_at(later, false);
        assert_eq!(rep.expired, 1);
        r.tick_at(later, false);
        let w2 = r.get(&w.id);
        assert_eq!(w2.status, Status::Expired);
        assert!(w2.expired_at.is_some());
        assert_eq!(r.alerts_with("expired with no match: rafting"), 1);
        assert_eq!(
            r.al.hitl.borrow().as_slice(),
            ["hex-watch expired: rafting"]
        );
        assert_eq!(r.emitted("hex.watch.expired").len(), 1);
        assert!(
            r.shell.gmail_calls.borrow().is_empty(),
            "an expired watch must not be polled"
        );
    }

    #[test]
    fn unknown_source_fails_that_watch_once_and_does_not_touch_the_streak() {
        let r = Rig::new("hit");
        let ok = r.add("true");
        let bad = r.add_with("true", "carrier-pigeon", &[], "", None, CDur::days(1));
        r.tick();
        assert_eq!(r.get(&ok.id).status, Status::Done);
        let b = r.get(&bad.id);
        assert_eq!(b.status, Status::Failed);
        assert!(b.error.unwrap().contains("unknown source"));
        assert_eq!(store::load_state(r.hex()).unwrap().poll_fail_streak, 0);
        assert_eq!(r.alerts_with("config error"), 1);
        for _ in 0..4 {
            r.tick();
        }
        assert_eq!(r.alerts_with("config error"), 1);
        assert_eq!(r.alerts_with("failing 3 ticks"), 0);
    }

    #[test]
    fn v1_record_without_since_fires_on_any_hit_and_never_expires() {
        let r = Rig::new("old");
        let p = store::legacy_jsonl(r.hex());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{\"id\":\"old1\",\"query\":\"legacy q\",\"action\":\"true\",\"status\":\"pending\",\"created\":\"2026-09-16T12:00:00-04:00\"}\n").unwrap();
        r.tick();
        assert_eq!(r.get("old1").status, Status::Done);
        assert!(!r.shell.gmail_calls.borrow()[0].contains("after:"));
    }

    #[test]
    fn event_source_fires_on_a_fresh_envelope_and_not_on_a_stale_one() {
        let r = Rig::new("none");
        let out = r.dir.path().join("env");
        let w = r.add_with(
            &format!("echo $WATCH_KEY $WATCH_DATA_ID > {}", out.display()),
            "event",
            &[("event", "deploy.done")],
            "",
            None,
            CDur::days(1),
        );
        r.tick();
        assert_eq!(r.get(&w.id).status, Status::Pending, "no envelope yet");
        r.sub.events.borrow_mut().insert(
            "deploy.done".into(),
            json!({"event":"deploy.done","producer":"t","ts":"2020-01-01T00:00:00+00:00","data":{"id":"old"}}),
        );
        r.tick();
        assert_eq!(r.get(&w.id).status, Status::Pending, "stale envelope");
        assert!(r.logs_contain("older than since"));
        let fresh =
            (Utc::now() + CDur::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        r.sub.events.borrow_mut().insert(
            "deploy.done".into(),
            json!({"event":"deploy.done","producer":"t","ts":fresh,"data":{"id":"d42"}}),
        );
        r.tick();
        assert_eq!(r.get(&w.id).status, Status::Done);
        assert_eq!(
            std::fs::read_to_string(&out).unwrap().trim(),
            format!("deploy.done@{fresh} d42")
        );
    }

    #[test]
    fn emit_failure_is_loud_and_does_not_block_the_watch() {
        let mut r = Rig::new("hit");
        r.sub.fail_emit = true;
        let w = r.add("true");
        r.tick();
        assert_eq!(r.get(&w.id).status, Status::Done);
        assert!(r.logs_contain("emit hex.watch.fired failed"));
    }

    #[test]
    fn dry_run_reports_and_runs_nothing() {
        let r = Rig::new("hit");
        let marker = r.dir.path().join("ran");
        let w = r.add(&format!("touch {}", marker.display()));
        let rep = r.tick_at(Utc::now(), true);
        assert_eq!(rep.would_fire.len(), 1);
        assert!(rep.would_fire[0].starts_with(&w.id));
        assert!(!marker.exists());
        assert_eq!(r.get(&w.id).status, Status::Pending);
    }

    #[test]
    fn a_second_tick_cannot_run_while_the_lock_is_held() {
        use fs2::FileExt;
        let r = Rig::new("hit");
        r.add("true");
        std::fs::create_dir_all(store::watch_dir(r.hex())).unwrap();
        let held = std::fs::File::create(store::watch_dir(r.hex()).join("tick.lock")).unwrap();
        held.lock_exclusive().unwrap();
        let log = |_m: &str| {};
        let env = Env {
            hex_dir: r.hex().to_path_buf(),
            config: Config::default(),
            substrate: &r.sub,
            shell: &r.shell,
            alerter: &r.al,
            parent_env: r.parent.clone(),
            dry_run: false,
            log: &log,
        };
        let err = run(&env, Utc::now()).unwrap_err();
        assert!(err.contains("another tick is already running"), "{err}");
        assert!(
            r.shell.gmail_calls.borrow().is_empty(),
            "locked-out tick must not poll"
        );
        held.unlock().unwrap();
        assert!(run(&env, Utc::now()).is_ok());
    }

    #[test]
    fn gmail_hit_without_internal_ms_is_a_poll_error_not_a_fire() {
        let r = Rig::new("nots");
        let w = r.add("true");
        let rep = r.tick();
        assert_eq!(rep.poll_failures, 1);
        assert_eq!(r.get(&w.id).status, Status::Pending);
        assert!(r.logs_contain("has no internal_ms"));
    }

    #[test]
    fn action_timeout_kills_the_whole_process_group() {
        let r = Rig::new("hit");
        let marker = r.dir.path().join("grandchild");
        // background grandchild in the same group; must die with the group
        let w = r.add(&format!("(sleep 2; touch {}) & sleep 30", marker.display()));
        let log = |_m: &str| {};
        let mut config = Config::default();
        config.sources.gmail.command = "GMAILSTUB {query} {account}".into();
        config.action_timeout_secs = 1;
        let env = Env {
            hex_dir: r.hex().to_path_buf(),
            config,
            substrate: &r.sub,
            shell: &r.shell,
            alerter: &r.al,
            parent_env: r.parent.clone(),
            dry_run: false,
            log: &log,
        };
        run(&env, Utc::now()).unwrap();
        assert_eq!(r.get(&w.id).status, Status::Failed);
        std::thread::sleep(Duration::from_millis(2500));
        assert!(!marker.exists(), "grandchild survived the timeout kill");
    }

    #[test]
    fn action_timeout_marks_failed_with_rc_124() {
        let r = Rig::new("hit");
        let w = r.add("sleep 30");
        let log = |_m: &str| {};
        let mut config = Config::default();
        config.sources.gmail.command = "GMAILSTUB {query} {account}".into();
        config.action_timeout_secs = 1;
        let env = Env {
            hex_dir: r.hex().to_path_buf(),
            config,
            substrate: &r.sub,
            shell: &r.shell,
            alerter: &r.al,
            parent_env: r.parent.clone(),
            dry_run: false,
            log: &log,
        };
        run(&env, Utc::now()).unwrap();
        let w2 = r.get(&w.id);
        assert_eq!(w2.status, Status::Failed);
        assert!(w2.error.unwrap().contains("timed out after 1s"));
        assert_eq!(r.alerts_with("rc=124"), 1);
    }
}
