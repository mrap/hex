//! On-disk store for hex-watch: one JSON file per watch, a streak/state
//! file, and an append-only transition log.
//!
//! ```text
//! $HEX_DIR/.hex/watch/
//!   items/<id>.json   one file per watch (8-hex ids); atomic tmp+rename writes
//!   state.json        {"poll_fail_streak": n, "last_tick": ts}
//!   log.jsonl         append-only: every status transition
//! ```
//!
//! One file per watch means an `add` during a tick is never lost to a
//! read-modify-write race (the single-file jsonl of the Python v0.x lost
//! them; CTO review 2026-09-16 risk 4).
//!
//! Every function takes `hex_dir: &Path` (same idiom as `hitl::store`) so
//! tests drive a tempdir. Failure stance (S6): a malformed item file is a
//! loud `Err` from `load_all`, never a skipped row; the caller decides.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Watch lifecycle. `pending -> firing -> done | failed`, `pending -> expired`.
/// `retry` returns `failed`, `firing`, `expired` to `pending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Firing,
    Done,
    Failed,
    Expired,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Firing => "firing",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Expired => "expired",
        }
    }
    /// Everything `list` shows by default: not yet resolved by a fire.
    pub fn is_live(&self) -> bool {
        !matches!(self, Status::Done)
    }
    pub fn is_retryable(&self) -> bool {
        matches!(self, Status::Failed | Status::Firing | Status::Expired)
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One watch record (spec `docs/hex-watch.md`, section 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Watch {
    pub id: String,
    /// Source adapter name: `gmail`, `event`.
    pub source: String,
    /// Source-specific match fields (`query`/`account` for gmail, `event` for event).
    #[serde(default)]
    pub r#match: BTreeMap<String, String>,
    /// Shell command run once on the first valid hit.
    pub action: String,
    #[serde(default)]
    pub note: String,
    pub status: Status,
    pub created: DateTime<Utc>,
    /// Only hits at or after this instant count. `None` on imported v1 records.
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    /// Pending past this instant becomes `expired`. `None` on imported v1 records.
    #[serde(default)]
    pub expires: Option<DateTime<Utc>>,
    #[serde(default)]
    pub fired: Option<DateTime<Utc>>,
    /// Dedupe key of the hit that fired (Gmail message id, `event@ts`).
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retried: Option<DateTime<Utc>>,
}

/// Cross-tick state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub poll_fail_streak: u32,
    #[serde(default)]
    pub last_tick: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

pub fn watch_dir(hex_dir: &Path) -> PathBuf {
    hex_dir.join(".hex").join("watch")
}
pub fn items_dir(hex_dir: &Path) -> PathBuf {
    watch_dir(hex_dir).join("items")
}
pub fn item_path(hex_dir: &Path, id: &str) -> PathBuf {
    items_dir(hex_dir).join(format!("{id}.json"))
}
pub fn state_path(hex_dir: &Path) -> PathBuf {
    watch_dir(hex_dir).join("state.json")
}
pub fn log_path(hex_dir: &Path) -> PathBuf {
    watch_dir(hex_dir).join("log.jsonl")
}
/// Where the Python v0.x watcher kept its queue; imported once by `import_v1`.
pub fn legacy_jsonl(hex_dir: &Path) -> PathBuf {
    hex_dir
        .join(".hex")
        .join("run")
        .join("hex-watch")
        .join("watches.jsonl")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn fmt_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `30m`, `12h`, `7d`, `45s` -> Duration. Anything else is an error that
/// names the accepted shape.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let n: i64 = num
        .parse()
        .map_err(|_| format!("bad duration {s:?}; use e.g. 30m, 12h, 7d"))?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => return Err(format!("bad duration {s:?}; use e.g. 30m, 12h, 7d")),
    };
    Ok(Duration::seconds(secs))
}

/// Parse an ISO-8601 instant. Naive input (no offset) is taken as UTC.
pub fn parse_iso(s: &str) -> Result<DateTime<Utc>, String> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(t, Utc));
    }
    // Python's isoformat with microseconds and an offset like "+00:00"
    if let Ok(t) = DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%:z") {
        return Ok(t.with_timezone(&Utc));
    }
    Err(format!(
        "bad timestamp {s:?}; want ISO-8601 like 2026-09-16T12:00:00-04:00"
    ))
}

fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("watch: no parent dir for {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("watch: mkdir {} failed: {e}", parent.display()))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)
        .map_err(|e| format!("watch: write {} failed: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        format!(
            "watch: rename {} -> {} failed: {e}",
            tmp.display(),
            path.display()
        )
    })
}

fn new_id() -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    u.chars().take(8).collect()
}

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

/// Persist a watch (create or update). Atomic.
pub fn save(hex_dir: &Path, w: &Watch) -> Result<(), String> {
    let body =
        serde_json::to_string_pretty(w).map_err(|e| format!("watch: serialize {}: {e}", w.id))?;
    write_atomic(&item_path(hex_dir, &w.id), &body)
}

pub fn load(hex_dir: &Path, id: &str) -> Result<Option<Watch>, String> {
    let p = item_path(hex_dir, id);
    let raw = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("watch: read {} failed: {e}", p.display())),
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| format!("watch: malformed item {}: {e}", p.display()))
}

/// Every watch, oldest first by `created`. A malformed file is a loud error
/// (S6); the caller decides whether to keep going.
pub fn load_all(hex_dir: &Path) -> Result<Vec<Watch>, String> {
    let dir = items_dir(hex_dir);
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("watch: read_dir {} failed: {e}", dir.display())),
    };
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| format!("watch: read_dir entry: {e}"))?;
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read_to_string(&p) {
            Ok(raw) => match serde_json::from_str::<Watch>(&raw) {
                Ok(w) => out.push(w),
                Err(e) => errors.push(format!("{}: {e}", p.display())),
            },
            Err(e) => errors.push(format!("{}: {e}", p.display())),
        }
    }
    if !errors.is_empty() {
        return Err(format!(
            "watch: malformed item file(s): {}",
            errors.join("; ")
        ));
    }
    out.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
    Ok(out)
}

pub fn delete(hex_dir: &Path, id: &str) -> Result<(), String> {
    let p = item_path(hex_dir, id);
    std::fs::remove_file(&p).map_err(|e| format!("watch: delete {} failed: {e}", p.display()))
}

/// Inputs for `create`.
pub struct NewWatch {
    pub source: String,
    pub r#match: BTreeMap<String, String>,
    pub action: String,
    pub note: String,
    pub since: Option<DateTime<Utc>>,
    /// Duration from `now` (never from `since`: a backdated since must not
    /// shorten the wait; that paged Mike once, 2026-09-16 13:28).
    pub expires_in: Duration,
}

pub fn create(hex_dir: &Path, new: NewWatch, now: DateTime<Utc>) -> Result<Watch, String> {
    if new.action.trim().is_empty() {
        return Err("watch: action cannot be empty".to_string());
    }
    let w = Watch {
        id: new_id(),
        source: new.source,
        r#match: new.r#match,
        action: new.action,
        note: new.note,
        status: Status::Pending,
        created: now,
        since: Some(new.since.unwrap_or(now)),
        expires: Some(now + new.expires_in),
        fired: None,
        key: None,
        error: None,
        expired_at: None,
        retried: None,
    };
    save(hex_dir, &w)?;
    log(hex_dir, &w.id, "add", now, None)?;
    Ok(w)
}

/// Append one transition to `log.jsonl`. Never fails the caller's main
/// path: an unwritable log is reported, the transition already happened.
pub fn log(
    hex_dir: &Path,
    id: &str,
    transition: &str,
    now: DateTime<Utc>,
    detail: Option<&str>,
) -> Result<(), String> {
    use std::io::Write;
    let p = log_path(hex_dir);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("watch: mkdir {}: {e}", parent.display()))?;
    }
    let row = serde_json::json!({"ts": fmt_ts(now), "id": id, "transition": transition, "detail": detail});
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .map_err(|e| format!("watch: open {}: {e}", p.display()))?;
    writeln!(f, "{row}").map_err(|e| format!("watch: append {}: {e}", p.display()))
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub fn load_state(hex_dir: &Path) -> Result<State, String> {
    let p = state_path(hex_dir);
    match std::fs::read_to_string(&p) {
        Ok(raw) => {
            serde_json::from_str(&raw).map_err(|e| format!("watch: malformed {}: {e}", p.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(format!("watch: read {}: {e}", p.display())),
    }
}

pub fn save_state(hex_dir: &Path, s: &State) -> Result<(), String> {
    let body = serde_json::to_string(s).map_err(|e| format!("watch: serialize state: {e}"))?;
    write_atomic(&state_path(hex_dir), &body)
}

// ---------------------------------------------------------------------------
// v1 import
// ---------------------------------------------------------------------------

/// Import the Python v0.x jsonl queue once: one file per record, then rename
/// the jsonl to `.imported`. Returns how many records landed. v1 records had a
/// top-level `query` and no `source`/`since`/`expires`; they become gmail
/// watches with no since guard that never expire (spec section 3).
pub fn import_v1(hex_dir: &Path, now: DateTime<Utc>) -> Result<usize, String> {
    let p = legacy_jsonl(hex_dir);
    let raw = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(format!("watch: read {}: {e}", p.display())),
    };
    let mut n = 0;
    for (i, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| format!("watch: {} line {}: {e}", p.display(), i + 1))?;
        let w = watch_from_v1(&v, now)
            .map_err(|e| format!("watch: {} line {}: {e}", p.display(), i + 1))?;
        if load(hex_dir, &w.id)?.is_some() {
            continue;
        }
        save(hex_dir, &w)?;
        log(hex_dir, &w.id, "import-v1", now, None)?;
        n += 1;
    }
    let done = p.with_extension("jsonl.imported");
    std::fs::rename(&p, &done)
        .map_err(|e| format!("watch: rename {} -> {}: {e}", p.display(), done.display()))?;
    Ok(n)
}

fn watch_from_v1(v: &serde_json::Value, now: DateTime<Utc>) -> Result<Watch, String> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    let ts = |k: &str| -> Result<Option<DateTime<Utc>>, String> {
        match s(k) {
            Some(x) => parse_iso(&x).map(Some),
            None => Ok(None),
        }
    };
    let mut m = BTreeMap::new();
    if let Some(obj) = v.get("match").and_then(|x| x.as_object()) {
        for (k, val) in obj {
            if let Some(sv) = val.as_str() {
                m.insert(k.clone(), sv.to_string());
            }
        }
    }
    if m.is_empty() {
        if let Some(q) = s("query") {
            m.insert("query".to_string(), q);
        }
    }
    let status = match s("status").as_deref() {
        Some("pending") | None => Status::Pending,
        Some("firing") => Status::Firing,
        Some("done") => Status::Done,
        Some("failed") => Status::Failed,
        Some("expired") => Status::Expired,
        Some(other) => return Err(format!("unknown status {other:?}")),
    };
    Ok(Watch {
        id: s("id").ok_or("record has no id")?,
        source: s("source").unwrap_or_else(|| "gmail".to_string()),
        r#match: m,
        action: s("action").ok_or("record has no action")?,
        note: s("note").unwrap_or_default(),
        status,
        created: ts("created")?.unwrap_or(now),
        since: ts("since")?,
        expires: ts("expires")?,
        fired: ts("fired")?,
        key: s("key").or_else(|| s("msg_id")),
        error: s("error"),
        expired_at: ts("expired_at")?,
        retried: ts("retried")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::TempDir::new().unwrap()
    }
    fn now() -> DateTime<Utc> {
        "2026-09-16T12:00:00Z".parse().unwrap()
    }
    fn new(action: &str) -> NewWatch {
        let mut m = BTreeMap::new();
        m.insert("query".into(), "q".into());
        m.insert("account".into(), "primary".into());
        NewWatch {
            source: "gmail".into(),
            r#match: m,
            action: action.into(),
            note: "n".into(),
            since: None,
            expires_in: parse_duration("14d").unwrap(),
        }
    }

    #[test]
    fn create_stamps_since_now_and_expires_14d_from_now() {
        let t = tmp();
        let w = create(t.path(), new("true"), now()).unwrap();
        assert_eq!(w.since, Some(now()));
        assert_eq!(w.expires, Some(now() + Duration::days(14)));
        assert_eq!(w.status, Status::Pending);
        assert_eq!(w.id.len(), 8);
        let back = load(t.path(), &w.id).unwrap().unwrap();
        assert_eq!(back, w);
        assert!(log_path(t.path()).exists());
    }

    #[test]
    fn backdated_since_does_not_shorten_expiry() {
        let t = tmp();
        let mut n = new("true");
        n.since = Some(now() - Duration::days(30));
        n.expires_in = Duration::hours(1);
        let w = create(t.path(), n, now()).unwrap();
        assert_eq!(w.expires, Some(now() + Duration::hours(1)));
    }

    #[test]
    fn duration_parsing_accepts_s_m_h_d_and_rejects_the_rest() {
        assert_eq!(parse_duration("45s").unwrap(), Duration::seconds(45));
        assert_eq!(parse_duration("30m").unwrap(), Duration::minutes(30));
        assert_eq!(parse_duration("12h").unwrap(), Duration::hours(12));
        assert_eq!(parse_duration("7d").unwrap(), Duration::days(7));
        assert!(parse_duration("soon").unwrap_err().contains("bad duration"));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("7w").is_err());
    }

    #[test]
    fn iso_parsing_covers_offsets_naive_and_python_microseconds() {
        assert_eq!(parse_iso("2026-09-16T12:00:00Z").unwrap(), now());
        assert_eq!(parse_iso("2026-09-16T08:00:00-04:00").unwrap(), now());
        assert_eq!(parse_iso("2026-09-16T12:00:00").unwrap(), now());
        assert_eq!(
            parse_iso("2026-09-16T12:00:00.123456+00:00").unwrap(),
            now() + Duration::microseconds(123456)
        );
        assert!(parse_iso("yesterday").is_err());
    }

    #[test]
    fn load_all_sorts_by_created_and_is_loud_on_malformed_files() {
        let t = tmp();
        let a = create(t.path(), new("true"), now() + Duration::seconds(5)).unwrap();
        let b = create(t.path(), new("true"), now()).unwrap();
        let all = load_all(t.path()).unwrap();
        assert_eq!(
            all.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            vec![b.id.as_str(), a.id.as_str()]
        );
        std::fs::write(items_dir(t.path()).join("bad.json"), "{not json").unwrap();
        let err = load_all(t.path()).unwrap_err();
        assert!(err.contains("malformed item file"), "{err}");
    }

    #[test]
    fn state_roundtrip_and_missing_file_is_default() {
        let t = tmp();
        assert_eq!(load_state(t.path()).unwrap(), State::default());
        let s = State {
            poll_fail_streak: 3,
            last_tick: Some(now()),
        };
        save_state(t.path(), &s).unwrap();
        assert_eq!(load_state(t.path()).unwrap(), s);
    }

    #[test]
    fn import_v1_moves_python_records_into_item_files_once() {
        let t = tmp();
        let p = legacy_jsonl(t.path());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            concat!(
                r#"{"id": "c09961bd", "query": "newer_than:7d from:x", "action": "/usr/bin/true", "note": "old shape", "status": "done", "created": "2026-09-16T12:07:41-04:00", "fired": "2026-09-16T12:07:58-04:00", "msg_id": "1a0aaefb0de09904"}"#,
                "\n",
                r#"{"id": "8918dcb2", "source": "event", "match": {"event": "hex.watch.probe"}, "action": "x", "note": "probe", "status": "pending", "created": "2026-09-16T13:41:36-04:00", "since": "2026-09-16T13:41:36-04:00", "expires": "2026-09-16T13:51:36-04:00", "fired": null, "key": null}"#,
                "\n"
            ),
        )
        .unwrap();
        assert_eq!(import_v1(t.path(), now()).unwrap(), 2);
        assert!(!p.exists());
        assert!(p.with_extension("jsonl.imported").exists());
        let old = load(t.path(), "c09961bd").unwrap().unwrap();
        assert_eq!(old.source, "gmail");
        assert_eq!(old.r#match.get("query").unwrap(), "newer_than:7d from:x");
        assert_eq!(old.since, None);
        assert_eq!(old.expires, None);
        assert_eq!(old.status, Status::Done);
        assert_eq!(old.key.as_deref(), Some("1a0aaefb0de09904"));
        let ev = load(t.path(), "8918dcb2").unwrap().unwrap();
        assert_eq!(ev.source, "event");
        assert_eq!(ev.r#match.get("event").unwrap(), "hex.watch.probe");
        assert!(ev.expires.is_some());
        // second call: nothing to import, no error
        assert_eq!(import_v1(t.path(), now()).unwrap(), 0);
    }
}
