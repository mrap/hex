//! Loud alert pathway: stderr + telemetry row + macOS notification, plus two
//! best-effort off-machine rails (ntfy push, gws/gmail email) driven by a
//! severity class.
//!
//! Delivery model (spec S6bg793ev, task T2cf094yz):
//!   * stderr + telemetry row + macOS banner: ALWAYS, every alert (current
//!     behavior, unchanged). Deduped per key via a stamp file (6h window) so a
//!     cron can call this every tick without spamming.
//!   * ntfy push: only when `$HEX_DIR/.hex/config/alerts.toml` configures a
//!     topic URL. Every alert pushes; the three named [`AlertClass`] variants
//!     push at urgent priority, the default class at normal priority. Push
//!     BODIES are generic (`[key] title` only) — never payload, paths, or
//!     personal data (the push service is third-party). One caller
//!     (`boi-spec-watch`) supplies an explicit, pre-sanitized name+outcome
//!     push body via the crate-internal `_push` seam so its phone alerts are
//!     human-readable; that override still never carries the machine key, a
//!     raw id, a path, or diagnostic detail.
//!   * gws/gmail email: only the three named classes (spend, harness-down,
//!     work-order-failed), and only when an operator address is configured.
//!
//! Config absent = exactly the pre-existing Mac-only behavior, plus one loud
//! line per process lifetime noting the rail is unconfigured.
//!
//! Both rails are best-effort: every failure is LOUD on stderr (S6) but never
//! fails the calling worker.

use serde::Deserialize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEDUPE_WINDOW: Duration = Duration::from_secs(6 * 3600);
const RATE_WINDOW_SECS: u64 = 3600;

/// Severity class for an alert. The `Default` variant reproduces the historical
/// behavior (push at normal priority, no email); the three named variants also
/// send the email rail and push at urgent priority. Adding this parameter did
/// not change any existing call site — `notify`/`notify_at` keep their exact
/// signatures and pass [`AlertClass::Default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlertClass {
    /// Push only, normal priority. The default for every historical call site.
    #[default]
    Default,
    /// Spend threshold crossed (burn guard). Push (urgent) + email.
    Spend,
    /// The harness itself is down (failures probe / watchdog). Push (urgent) + email.
    HarnessDown,
    /// A work order terminally failed (boi-spec-watch). Push (urgent) + email.
    WorkOrderFailed,
}

impl AlertClass {
    /// The three named classes additionally send the email rail.
    fn sends_email(self) -> bool {
        !matches!(self, AlertClass::Default)
    }

    /// ntfy priority header value. Named (email) classes push at urgent.
    fn push_priority(self) -> &'static str {
        if self.sends_email() {
            "urgent"
        } else {
            "default"
        }
    }
}

/// Off-machine rail configuration, read from `$HEX_DIR/.hex/config/alerts.toml`.
/// This file is INSTANCE data — the foundation ships only the loader, a
/// template, and docs; a real topic URL or address never lives in this repo.
#[derive(Debug, Clone, Deserialize)]
pub struct AlertsConfig {
    /// ntfy topic URL. Required for the push rail; absent = push loudly skipped.
    #[serde(default)]
    pub ntfy_topic_url: Option<String>,
    /// Operator email address. Required for the email rail; absent = email
    /// loudly skipped.
    #[serde(default)]
    pub email: Option<String>,
    /// Cap on human-facing pushes per rolling hour, across all keys.
    #[serde(default = "default_max_pushes")]
    pub max_pushes_per_hour: u32,
}

fn default_max_pushes() -> u32 {
    6
}

/// Read `hex_dir/.hex/config/alerts.toml`. Returns `None` when the file is
/// absent (→ Mac-only fallback) or malformed (→ loud fallback). NEVER consults
/// `HEX_DIR` or the home directory itself: resolution happens once, in
/// [`notify_with_class`], so an unset `HEX_DIR` can never read a real operator
/// config off-disk.
pub fn load_config(hex_dir: &Path) -> Option<AlertsConfig> {
    let path = hex_dir.join(".hex/config/alerts.toml");
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(_) => return None, // absent = pre-existing behavior, exactly
    };
    match toml::from_str::<AlertsConfig>(&body) {
        Ok(c) => Some(c),
        Err(e) => {
            warn(&format!(
                "alert: FAILED to parse .hex/config/alerts.toml — {e} (S6; falling back to Mac-only)"
            ));
            None
        }
    }
}

/// Returns true if the alert fired (not suppressed by per-key dedupe).
/// Signature unchanged — every historical caller compiles as-is and now pushes
/// at normal priority (via [`AlertClass::Default`]).
pub fn notify(key: &str, title: &str, msg: &str) -> bool {
    notify_with_class(key, title, msg, AlertClass::Default)
}

/// Severity-aware entrypoint. Resolves `HEX_DIR` (the single resolution point),
/// then delegates to [`notify_at_with_class`].
pub fn notify_with_class(key: &str, title: &str, msg: &str, class: AlertClass) -> bool {
    notify_with_class_push(key, title, msg, class, None)
}

/// Like [`notify_with_class`], but with an optional PUSH-BODY OVERRIDE for the
/// third-party push rail only. When `Some`, the push carries exactly that
/// (caller-sanitized, generic) string instead of the default `[key] title`;
/// the stderr line, telemetry row, macOS banner, and email rail are unchanged
/// and still use `key`/`title`/`msg`. Lets `boi-spec-watch` send a
/// human-readable job name + outcome to the phone without leaking the internal
/// machine key. Crate-internal so the "arbitrary push body" seam never becomes
/// public API.
pub(crate) fn notify_with_class_push(
    key: &str,
    title: &str,
    msg: &str,
    class: AlertClass,
    push_override: Option<&str>,
) -> bool {
    let hex_dir = match std::env::var("HEX_DIR") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => {
            eprintln!("ALERT [{key}] {title}: {msg} (HEX_DIR unset — stderr only)");
            return true;
        }
    };
    notify_at_with_class_push(&hex_dir, key, title, msg, class, push_override)
}

/// Inner, testable form. Signature unchanged — historical callers keep working;
/// delegates at [`AlertClass::Default`].
pub fn notify_at(hex_dir: &Path, key: &str, title: &str, msg: &str) -> bool {
    notify_at_with_class(hex_dir, key, title, msg, AlertClass::Default)
}

/// Inner, testable, severity-aware form. Delegates at no push override.
pub fn notify_at_with_class(
    hex_dir: &Path,
    key: &str,
    title: &str,
    msg: &str,
    class: AlertClass,
) -> bool {
    notify_at_with_class_push(hex_dir, key, title, msg, class, None)
}

/// Inner, testable, severity-aware form with an optional push-body override.
/// See [`notify_with_class_push`] for the override semantics: `push_override`
/// affects ONLY the third-party push body; stderr, telemetry, the macOS banner,
/// and the email rail all still use `key`/`title`/`msg`.
pub(crate) fn notify_at_with_class_push(
    hex_dir: &Path,
    key: &str,
    title: &str,
    msg: &str,
    class: AlertClass,
    push_override: Option<&str>,
) -> bool {
    if suppressed(hex_dir, key) {
        return false;
    }
    // stderr + telemetry + Mac banner: ALWAYS, before any rail. This is what
    // keeps the collapse cap S6-compliant — every suppressed push still leaves
    // a loud stderr line and a telemetry row here; only the third-party push
    // collapses.
    eprintln!("ALERT [{key}] {title}: {msg}");
    let _ = crate::telemetry::record(&crate::telemetry::TelemetryEvent {
        source: "alert".into(),
        event: key.into(),
        status: "alert".into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(format!("{title}: {msg}")),
    });
    #[cfg(all(target_os = "macos", not(test)))]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            msg.replace('"', "'"),
            title.replace('"', "'")
        );
        if let Err(e) = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .status()
        {
            eprintln!("alert [{key}]: osascript failed: {e}");
        }
    }

    // Off-machine rails.
    match load_config(hex_dir) {
        Some(cfg) => deliver_rails(hex_dir, &cfg, key, title, msg, class, push_override),
        None => {
            if take_unconfigured_warning() {
                warn(
                    "alert: off-machine rail unconfigured (.hex/config/alerts.toml absent) \
                     — Mac-only for this process",
                );
            }
        }
    }

    stamp(hex_dir, key);
    true
}

/// Clear a dedupe stamp so the next `notify(key, …)` is guaranteed to fire.
///
/// Used when a watched condition ENDS (e.g. a BOI task leaves `blocked`) so the
/// next genuine episode re-alerts instead of being swallowed by the shared 6h
/// dedupe window. Without this, an unblock→re-block inside 6h stays silent (the
/// bug this closes). Never fails the caller (S6): a missing stamp is a no-op; an
/// unexpected IO error is logged loudly to stderr, not propagated.
pub fn clear(key: &str) {
    if let Ok(d) = std::env::var("HEX_DIR") {
        clear_at(std::path::Path::new(&d), key);
    }
}

/// Inner, testable form of [`clear`].
pub fn clear_at(hex_dir: &Path, key: &str) {
    let p = stamp_path(hex_dir, key);
    match std::fs::remove_file(&p) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("alert [{key}]: stamp clear failed: {e}"),
    }
}

/// Generic push payload: the alert key and short title ONLY. Never the message
/// body, a path, or personal data — the push service is third-party. This is a
/// pure, cfg-independent function so the generic-body invariant can be proven
/// against the exact value production sends.
fn push_body(key: &str, title: &str) -> String {
    format!("[{key}] {title}")
}

/// Body of the single collapse push. `suppressed` is a floor (the count at
/// emit time; more may be dropped afterward within the window) — it never
/// claims a total it will outlive. Generic: no key, title, path, or payload.
fn collapse_body(suppressed: u64, max: u32) -> String {
    format!("hex alerts: {suppressed}+ push(es) suppressed this hour (rate cap {max}/h reached)")
}

fn deliver_rails(
    hex_dir: &Path,
    cfg: &AlertsConfig,
    key: &str,
    title: &str,
    msg: &str,
    class: AlertClass,
    push_override: Option<&str>,
) {
    // Push rail (rate-capped across all keys). Only consult the rate gate when
    // a topic URL is actually configured, so a missing URL never burns budget.
    match cfg.ntfy_topic_url.as_deref() {
        Some(url) if !url.is_empty() => match push_gate(hex_dir, cfg.max_pushes_per_hour) {
            PushDecision::Send => {
                // A caller-supplied override (already sanitized + generic)
                // replaces the default `[key] title`; the collapse notice and
                // every other rail are untouched.
                let body = match push_override {
                    Some(b) => b.to_string(),
                    None => push_body(key, title),
                };
                deliver_push(url, &body, class.push_priority(), false)
            }
            PushDecision::Collapse { suppressed } => deliver_push(
                url,
                &collapse_body(suppressed, cfg.max_pushes_per_hour),
                "default",
                true,
            ),
            PushDecision::Suppress => { /* stderr + telemetry already fired; push dropped */ }
        },
        _ => warn("alert: push requested but ntfy_topic_url unset in alerts.toml (S6)"),
    }

    // Email rail (second rail; only the three named classes; not rate-capped).
    if class.sends_email() {
        match cfg.email.as_deref() {
            Some(addr) if !addr.is_empty() => {
                let subject = format!("[hex alert] {title}");
                // Email is first-party (operator's own mailbox), so it may carry
                // detail; only the third-party PUSH body must stay generic.
                let body = format!("key: {key}\ntitle: {title}\n\n{msg}");
                deliver_email(addr, &subject, &body);
            }
            _ => warn("alert: email rail requested but email unset in alerts.toml (S6)"),
        }
    }
}

/// Send one ntfy push via curl (best-effort, loud on failure). `url` is
/// guaranteed non-empty by the caller. Under `cfg(test)` the real curl is
/// replaced by a recording sink so delivery can be asserted without network.
fn deliver_push(url: &str, body: &str, priority: &str, collapse: bool) {
    #[cfg(test)]
    {
        let _ = url;
        test_sink::record_push(body, priority, collapse);
    }
    #[cfg(not(test))]
    {
        let _ = collapse;
        let status = std::process::Command::new("curl")
            .arg("-fsS")
            .arg("-H")
            .arg(format!("Priority: {priority}"))
            .arg("-H")
            .arg("Title: hex alert")
            .arg("-d")
            .arg(body)
            .arg(url)
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => warn(&format!("alert: ntfy push exited non-zero: {s} (S6)")),
            Err(e) => warn(&format!("alert: ntfy push failed to spawn curl: {e} (S6)")),
        }
    }
}

/// Send one email via the gws CLI (best-effort, loud on failure). Under
/// `cfg(test)` the real gws is replaced by a recording sink.
fn deliver_email(to: &str, subject: &str, body: &str) {
    #[cfg(test)]
    test_sink::record_email(to, subject, body);
    #[cfg(not(test))]
    {
        let status = std::process::Command::new("gws")
            .args([
                "gmail",
                "send",
                "--to",
                to,
                "--subject",
                subject,
                "--body",
                body,
            ])
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => warn(&format!("alert: gws email exited non-zero: {s} (S6)")),
            Err(e) => warn(&format!("alert: gws email failed to spawn: {e} (S6)")),
        }
    }
}

/// One loud warning line to stderr. Under `cfg(test)` it also records the
/// message so "every failure path is loud" can be asserted deterministically.
fn warn(msg: &str) {
    eprintln!("{msg}");
    #[cfg(test)]
    test_sink::record_warning(msg);
}

// ---- Rate cap (hourly collapse) --------------------------------------------

/// Decision for a single push under the hourly cap.
enum PushDecision {
    /// Under the cap: send the real, human-facing push.
    Send,
    /// First overflow this window: send exactly one collapse notice.
    Collapse { suppressed: u64 },
    /// Over the cap and the collapse already went out: send nothing.
    Suppress,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct PushWindow {
    /// Unix seconds at which this rolling hour window opened.
    window_start: u64,
    /// Human-facing pushes sent this window.
    sent: u64,
    /// Pushes suppressed this window (the collapse names a floor of this).
    suppressed: u64,
    /// Whether the single collapse notice has already gone out this window.
    collapse_sent: bool,
}

fn window_path(hex_dir: &Path) -> std::path::PathBuf {
    hex_dir.join(".hex/run/alerts/push-window.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Advance the rolling-hour push counter and decide the fate of this push.
/// State persists under `.hex/run/alerts/push-window.json`. A corrupt state
/// file is loudly reset rather than silently trusted (S6).
fn push_gate(hex_dir: &Path, max: u32) -> PushDecision {
    let path = window_path(hex_dir);
    let now = now_secs();
    let mut w = read_window(&path);
    if w.window_start == 0 || now.saturating_sub(w.window_start) >= RATE_WINDOW_SECS {
        w = PushWindow {
            window_start: now,
            ..Default::default()
        };
    }
    let decision = if w.sent < max as u64 {
        w.sent += 1;
        PushDecision::Send
    } else {
        w.suppressed += 1;
        if w.collapse_sent {
            PushDecision::Suppress
        } else {
            w.collapse_sent = true;
            PushDecision::Collapse {
                suppressed: w.suppressed,
            }
        }
    };
    write_window(&path, &w);
    decision
}

fn read_window(path: &Path) -> PushWindow {
    match std::fs::read_to_string(path) {
        Ok(body) => match serde_json::from_str(&body) {
            Ok(w) => w,
            Err(e) => {
                warn(&format!(
                    "alert: corrupt push-window state — {e} (S6; resetting hourly cap)"
                ));
                PushWindow::default()
            }
        },
        Err(_) => PushWindow::default(),
    }
}

fn write_window(path: &Path, w: &PushWindow) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string(w) {
        Ok(body) => {
            if let Err(e) = std::fs::write(path, body) {
                warn(&format!("alert: push-window write failed: {e} (S6)"));
            }
        }
        Err(e) => warn(&format!("alert: push-window serialize failed: {e} (S6)")),
    }
}

// ---- Per-key dedupe (unchanged) --------------------------------------------

fn stamp_path(hex_dir: &Path, key: &str) -> std::path::PathBuf {
    hex_dir.join(".hex/run/alerts").join(format!("{key}.last"))
}

fn suppressed(hex_dir: &Path, key: &str) -> bool {
    stamp_path(hex_dir, key)
        .metadata()
        .and_then(|m| m.modified())
        .map(|t| SystemTime::now().duration_since(t).unwrap_or(Duration::MAX) < DEDUPE_WINDOW)
        .unwrap_or(false)
}

fn stamp(hex_dir: &Path, key: &str) {
    let p = stamp_path(hex_dir, key);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&p, b"") {
        eprintln!("alert [{key}]: stamp write failed: {e}");
    }
}

// ---- Unconfigured-rail warning (once per process) --------------------------

static UNCONFIGURED_WARNED: AtomicBool = AtomicBool::new(false);

/// Returns true exactly once per process (the first call), false thereafter, so
/// the "rail unconfigured" note is emitted at most one loud line per lifetime.
fn take_unconfigured_warning() -> bool {
    !UNCONFIGURED_WARNED.swap(true, Ordering::SeqCst)
}

#[cfg(test)]
fn reset_unconfigured_warning() {
    UNCONFIGURED_WARNED.store(false, Ordering::SeqCst);
}

// ---- Test recording sink ----------------------------------------------------

/// Under `cfg(test)`, the curl/gws/stderr side effects are recorded here so the
/// test set can assert deliveries and loud warnings without touching the
/// network. `cfg(test)` is active only for this lib target's own unit tests —
/// production (bin + integration) links the real curl/gws arms.
#[cfg(test)]
pub(crate) mod test_sink {
    use std::sync::Mutex;

    #[derive(Clone, Debug)]
    pub struct PushRec {
        pub body: String,
        pub priority: String,
        pub collapse: bool,
    }

    #[derive(Clone, Debug)]
    pub struct EmailRec {
        pub to: String,
        pub subject: String,
        pub body: String,
    }

    pub static PUSHES: Mutex<Vec<PushRec>> = Mutex::new(Vec::new());
    pub static EMAILS: Mutex<Vec<EmailRec>> = Mutex::new(Vec::new());
    pub static WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    pub fn reset() {
        PUSHES.lock().unwrap().clear();
        EMAILS.lock().unwrap().clear();
        WARNINGS.lock().unwrap().clear();
    }

    pub fn record_push(body: &str, priority: &str, collapse: bool) {
        PUSHES.lock().unwrap().push(PushRec {
            body: body.to_string(),
            priority: priority.to_string(),
            collapse,
        });
    }

    pub fn record_email(to: &str, subject: &str, body: &str) {
        EMAILS.lock().unwrap().push(EmailRec {
            to: to.to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
        });
    }

    pub fn record_warning(msg: &str) {
        WARNINGS.lock().unwrap().push(msg.to_string());
    }

    pub fn pushes() -> Vec<PushRec> {
        PUSHES.lock().unwrap().clone()
    }

    pub fn emails() -> Vec<EmailRec> {
        EMAILS.lock().unwrap().clone()
    }

    pub fn warnings() -> Vec<String> {
        WARNINGS.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Prepare an isolated process-global test environment. Every alert test
    /// mutates process-global state (HEX_DIR, the recording sink, the
    /// once-only warning flag), so all of them serialize on the crate's single
    /// HEX_DIR lock (telemetry::test_support) and reset shared state up front.
    fn setup(tmp: &tempfile::TempDir) {
        std::env::set_var("HEX_DIR", tmp.path());
        std::fs::create_dir_all(tmp.path().join(".hex/config")).unwrap();
        test_sink::reset();
        reset_unconfigured_warning();
    }

    fn write_alerts_toml(tmp: &tempfile::TempDir, body: &str) {
        let p = tmp.path().join(".hex/config/alerts.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn dedupe_suppresses_within_window() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        setup(&tmp);
        assert!(notify_at(tmp.path(), "test-key", "t", "m"));
        assert!(!notify_at(tmp.path(), "test-key", "t", "m")); // suppressed
    }

    #[test]
    fn clear_stamp_reenables_notify() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HEX_DIR", tmp.path());
        assert!(notify_at(tmp.path(), "clear-key", "t", "m"));
        assert!(!notify_at(tmp.path(), "clear-key", "t", "m")); // suppressed by stamp
        clear_at(tmp.path(), "clear-key"); // unblock clears the stamp
        assert!(
            notify_at(tmp.path(), "clear-key", "t", "m"),
            "after clear, the next notify must fire again (re-block re-alert)"
        );
        // Clearing an absent stamp is a quiet no-op, never a panic.
        clear_at(tmp.path(), "never-stamped-key");
    }

    // --- config precedence + absent-config fallback -------------------------

    #[test]
    fn config_precedence_file_value_beats_builtin_default() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        setup(&tmp);
        // Field present in the file wins over the built-in 6.
        write_alerts_toml(
            &tmp,
            "ntfy_topic_url = \"https://ntfy.example.invalid/t\"\nmax_pushes_per_hour = 2\n",
        );
        let cfg = load_config(tmp.path()).expect("config present");
        assert_eq!(cfg.max_pushes_per_hour, 2);

        // Field omitted falls back to the built-in default of 6.
        write_alerts_toml(
            &tmp,
            "ntfy_topic_url = \"https://ntfy.example.invalid/t\"\n",
        );
        let cfg = load_config(tmp.path()).expect("config present");
        assert_eq!(cfg.max_pushes_per_hour, 6);
    }

    #[test]
    fn absent_config_falls_back_and_delivers_no_rail() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        setup(&tmp);
        assert!(load_config(tmp.path()).is_none());
        // notify still fires (returns true) but no push/email leaves the box.
        assert!(notify_at_with_class(
            tmp.path(),
            "no-config",
            "t",
            "m",
            AlertClass::Spend
        ));
        assert!(test_sink::pushes().is_empty());
        assert!(test_sink::emails().is_empty());
    }

    #[test]
    fn unconfigured_warning_is_once_per_process() {
        let _g = crate::telemetry::test_support::lock_env();
        reset_unconfigured_warning();
        assert!(take_unconfigured_warning());
        assert!(!take_unconfigured_warning());
        assert!(!take_unconfigured_warning());
    }

    // --- generic-body invariant ---------------------------------------------

    #[test]
    fn push_body_contains_no_path_email_or_personal_tokens() {
        // Prove the invariant against the exact value production sends: the
        // pure push_body(), not the sink. Seed a message stuffed with a path,
        // an email address, and personal tokens; none may reach the push body.
        let seeded_tokens = [
            "PERSONA-ALPHA",
            "PERSONA-BRAVO",
            "hunter2",
            "SSN-000-00-0000",
        ];
        let msg = "/Users/test/secret/report.pdf contact person@example.invalid \
                   PERSONA-ALPHA PERSONA-BRAVO hunter2 SSN-000-00-0000";
        let body = push_body("burn-guard", "Spend threshold crossed");

        assert!(!body.contains('/'), "push body leaked a path: {body}");
        assert!(!body.contains('@'), "push body leaked an email: {body}");
        for tok in seeded_tokens {
            assert!(!body.contains(tok), "push body leaked token {tok}: {body}");
        }
        // The msg itself must never appear — payload is push-forbidden.
        assert!(!body.contains(msg));
        assert!(!body.contains("report.pdf"));
        // Sanity: the allowed generic pieces (key + title) are present.
        assert!(body.contains("burn-guard"));
        assert!(body.contains("Spend threshold crossed"));
    }

    // --- email classes send both rails; default sends push only -------------

    #[test]
    fn email_classes_send_both_rails() {
        let _g = crate::telemetry::test_support::lock_env();
        for (i, class) in [
            AlertClass::Spend,
            AlertClass::HarnessDown,
            AlertClass::WorkOrderFailed,
        ]
        .into_iter()
        .enumerate()
        {
            let tmp = tempfile::TempDir::new().unwrap();
            setup(&tmp);
            write_alerts_toml(
                &tmp,
                "ntfy_topic_url = \"https://ntfy.example.invalid/t\"\n\
                 email = \"ops@example.invalid\"\n",
            );
            let key = format!("class-{i}");
            assert!(notify_at_with_class(
                tmp.path(),
                &key,
                "title",
                "detail",
                class
            ));
            let pushes = test_sink::pushes();
            let emails = test_sink::emails();
            assert_eq!(pushes.len(), 1, "{class:?} should send exactly one push");
            assert_eq!(pushes[0].priority, "urgent", "{class:?} pushes urgent");
            assert_eq!(emails.len(), 1, "{class:?} should send exactly one email");
            assert_eq!(emails[0].to, "ops@example.invalid");
            // Email is first-party, so it carries the detail the push omits.
            assert!(
                emails[0].subject.contains("title"),
                "email subject: {}",
                emails[0].subject
            );
            assert!(
                emails[0].body.contains("detail"),
                "email body: {}",
                emails[0].body
            );
        }
    }

    #[test]
    fn default_class_sends_push_only_at_normal_priority() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        setup(&tmp);
        write_alerts_toml(
            &tmp,
            "ntfy_topic_url = \"https://ntfy.example.invalid/t\"\n\
             email = \"ops@example.invalid\"\n",
        );
        assert!(notify_at_with_class(
            tmp.path(),
            "routine",
            "title",
            "detail",
            AlertClass::Default
        ));
        let pushes = test_sink::pushes();
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].priority, "default");
        assert!(
            test_sink::emails().is_empty(),
            "default class must not email"
        );
    }

    // --- hourly cap collapses and reports -----------------------------------

    #[test]
    fn hourly_cap_collapses_and_reports() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        setup(&tmp);
        write_alerts_toml(
            &tmp,
            "ntfy_topic_url = \"https://ntfy.example.invalid/t\"\nmax_pushes_per_hour = 2\n",
        );
        // Distinct keys so per-key dedupe never interferes.
        for i in 0..4 {
            assert!(notify_at_with_class(
                tmp.path(),
                &format!("cap-{i}"),
                "t",
                "m",
                AlertClass::Default
            ));
        }
        let pushes = test_sink::pushes();
        // 2 human-facing pushes + exactly 1 collapse notice = 3 total.
        assert_eq!(pushes.len(), 3, "cap 2 → 2 real + 1 collapse");
        let human: Vec<_> = pushes.iter().filter(|p| !p.collapse).collect();
        let collapses: Vec<_> = pushes.iter().filter(|p| p.collapse).collect();
        assert_eq!(human.len(), 2, "at most max human-facing pushes");
        assert_eq!(collapses.len(), 1, "exactly one collapse push");
        // The collapse is visible and names a suppressed count (never silent).
        assert!(
            collapses[0].body.contains("suppressed"),
            "collapse must report the suppressed count: {}",
            collapses[0].body
        );
    }

    // --- every failure path is loud -----------------------------------------

    #[test]
    fn missing_rails_fail_loudly() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        setup(&tmp);
        // Config present but BOTH rails unconfigured, on an email class: both
        // the push and the email path must warn loudly, and nothing delivers.
        write_alerts_toml(&tmp, "max_pushes_per_hour = 6\n");
        assert!(notify_at_with_class(
            tmp.path(),
            "loud",
            "t",
            "m",
            AlertClass::HarnessDown
        ));
        let warnings = test_sink::warnings();
        assert!(
            warnings.iter().any(|w| w.contains("ntfy_topic_url unset")),
            "missing push URL must warn loudly: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("email unset")),
            "missing email must warn loudly: {warnings:?}"
        );
        assert!(test_sink::pushes().is_empty());
        assert!(test_sink::emails().is_empty());
    }

    #[test]
    fn malformed_config_falls_back_loudly() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        setup(&tmp);
        write_alerts_toml(&tmp, "this is = = not valid toml ]][[");
        assert!(load_config(tmp.path()).is_none(), "malformed → None");
        assert!(
            test_sink::warnings()
                .iter()
                .any(|w| w.contains("FAILED to parse")),
            "malformed config must warn loudly: {:?}",
            test_sink::warnings()
        );
    }
}
