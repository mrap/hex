//! Durable, privacy-preserving Codex usage ledger.
//!
//! This module stores normalized accounting metadata and source coordinates only.
//! It never stores JSONL payloads, prompts, tool output, or credentials.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug)]
pub enum LedgerError {
    Sql(rusqlite::Error),
    Io(std::io::Error),
}
impl From<rusqlite::Error> for LedgerError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}
impl From<std::io::Error> for LedgerError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
pub type Result<T> = std::result::Result<T, LedgerError>;

/// A bounded importer never consumes a partial trailing JSONL line.
#[derive(Debug, Clone, Copy)]
pub struct ImportOptions {
    pub max_records: usize,
    pub abort_before_commit: bool,
}
impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            max_records: 1_000,
            abort_before_commit: false,
        }
    }
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportResult {
    pub accepted: u64,
    pub duplicates: u64,
    pub conflicts: u64,
    pub quarantined: u64,
    pub pending_partial: bool,
    pub backlog: bool,
    pub bytes_read: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRow {
    pub provider: String,
    pub account_scope: String,
    pub response_id: String,
    pub parent_response_id: Option<String>,
    pub root_task_family: Option<String>,
    pub event_at: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub input_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    pub accepted: u64,
    pub duplicates: u64,
    pub conflicts: u64,
    pub quarantined: u64,
    pub pending_sources: u64,
    pub stale_sources: u64,
}
pub struct UsageLedger {
    conn: Connection,
}

impl UsageLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        // journal_mode returns a row, so use query_row rather than pragma_update.
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        conn.execute_batch(SCHEMA)?;
        let _ = conn.execute(
            "ALTER TABLE source_files ADD COLUMN source_mtime_ns INTEGER NOT NULL DEFAULT 0",
            [],
        );
        Ok(Self { conn })
    }
    /// Inserted observations, canonical rows, quarantine state, and cursor movement share one transaction.
    pub fn import_jsonl(
        &mut self,
        path: impl AsRef<Path>,
        options: ImportOptions,
    ) -> Result<ImportResult> {
        let path = path.as_ref();
        let meta = fs::metadata(path)?;
        let identity = file_identity(&meta);
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_nanos() as i64)
            .unwrap_or(0);
        let path_text = path.to_string_lossy().to_string();
        let prior:Option<(i64,i64,String,i64,i64)>=self.conn.query_row("SELECT generation,cursor,prefix_hash,source_len,source_mtime_ns FROM source_files WHERE identity=?1 ORDER BY generation DESC LIMIT 1",params![identity],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        // No-change path reads metadata only. A body fingerprint is read only if
        // size or mtime says the source may have changed.
        let unchanged = matches!(&prior, Some((_, _, _, old_len, old_mtime)) if *old_len == meta.len() as i64 && *old_mtime == mtime_ns);
        let prefix = if unchanged {
            prior.as_ref().unwrap().2.clone()
        } else {
            prefix_hash(path)?
        };
        let (generation, cursor) = match prior {
            Some((g, c, old, _, _)) if meta.len() as i64 >= c && old == prefix => (g, c),
            Some((g, _, _, _, _)) => (g + 1, 0),
            None => (0, 0),
        };
        let source_key = format!("{identity}:{generation}");
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(cursor as u64))?;
        let mut reader = BufReader::new(file);
        let mut pending = Vec::new();
        let mut offset = cursor as u64;
        let mut last_complete = offset;
        let mut bytes_read = 0;
        let mut partial = false;
        loop {
            if pending.len() >= options.max_records {
                break;
            }
            let mut line = Vec::new();
            let n = reader.read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }
            bytes_read += n as u64;
            let start = offset;
            offset += n as u64;
            if !line.ends_with(b"\n") {
                partial = true;
                break;
            }
            last_complete = offset;
            pending.push((
                start as i64,
                String::from_utf8_lossy(&line)
                    .trim_end_matches(['\r', '\n'])
                    .to_string(),
            ));
        }
        let backlog = pending.len() >= options.max_records || (!partial && offset < meta.len());
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO source_files(source_key,identity,generation,path,prefix_hash,cursor,source_len,updated_at,source_mtime_ns) VALUES(?1,?2,?3,?4,?5,0,?6,?7,?8) ON CONFLICT(source_key) DO UPDATE SET path=excluded.path,source_len=excluded.source_len,updated_at=excluded.updated_at,source_mtime_ns=excluded.source_mtime_ns",params![source_key,identity,generation,path_text,prefix,meta.len() as i64,Utc::now().to_rfc3339(),mtime_ns])?;
        let mut out = ImportResult {
            pending_partial: partial,
            backlog,
            bytes_read,
            ..Default::default()
        };
        for (byte_offset, line) in pending {
            import_line(&tx, &source_key, byte_offset, &line, &mut out)?;
        }
        tx.execute(
            "UPDATE source_files SET cursor=?1,source_len=?2,updated_at=?3 WHERE source_key=?4",
            params![
                last_complete as i64,
                meta.len() as i64,
                Utc::now().to_rfc3339(),
                source_key
            ],
        )?;
        if options.abort_before_commit {
            return Err(LedgerError::Io(std::io::Error::other(
                "test interruption before commit",
            )));
        }
        tx.commit()?;
        Ok(out)
    }
    pub fn rows(&self, limit: usize, offset: usize) -> Result<Vec<UsageRow>> {
        let mut s=self.conn.prepare("SELECT provider,account_scope,response_id,parent_response_id,root_task_family,event_at,model,effort,input_tokens,cached_input_tokens,output_tokens FROM canonical_responses ORDER BY event_at,response_id LIMIT ?1 OFFSET ?2")?;
        let rows = s
            .query_map(params![limit as i64, offset as i64], |r| {
                Ok(UsageRow {
                    provider: r.get(0)?,
                    account_scope: r.get(1)?,
                    response_id: r.get(2)?,
                    parent_response_id: r.get(3)?,
                    root_task_family: r.get(4)?,
                    event_at: r.get(5)?,
                    model: r.get(6)?,
                    effort: r.get(7)?,
                    input_tokens: r.get(8)?,
                    cached_input_tokens: r.get(9)?,
                    output_tokens: r.get(10)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }
    pub fn coverage(&self) -> Result<Coverage> {
        let mut c = Coverage::default();
        for (v, t) in [
            ("accepted", &mut c.accepted),
            ("duplicate", &mut c.duplicates),
            ("conflict", &mut c.conflicts),
            ("quarantine", &mut c.quarantined),
        ] {
            *t = self.conn.query_row(
                "SELECT count(*) FROM observations WHERE verdict=?1",
                [v],
                |r| r.get(0),
            )?;
        }
        c.pending_sources = self.conn.query_row(
            "SELECT count(*) FROM source_files WHERE cursor < source_len",
            [],
            |r| r.get(0),
        )?;
        c.stale_sources = self.conn.query_row(
            "SELECT count(*) FROM source_files WHERE updated_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(c)
    }
}
fn import_line(
    tx: &Transaction<'_>,
    source: &str,
    offset: i64,
    line: &str,
    out: &mut ImportResult,
) -> Result<()> {
    let hash = hash(line.as_bytes());
    if tx
        .query_row(
            "SELECT 1 FROM observations WHERE source_key=?1 AND byte_offset=?2",
            params![source, offset],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some()
    {
        return Ok(());
    }
    let mut r = match parse(line) {
        Ok(r) => r,
        Err(reason) => {
            observe(tx, source, offset, &hash, "quarantine", Some(&reason), None)?;
            out.quarantined += 1;
            return Ok(());
        }
    };
    if r.account_scope == "unknown-local-source" {
        r.account_scope = format!("unknown-local-source:{source}");
    }
    let old:Option<String>=tx.query_row("SELECT record_hash FROM canonical_responses WHERE provider=?1 AND account_scope=?2 AND response_id=?3",params![r.provider,r.account_scope,r.response_id],|x|x.get(0)).optional()?;
    let seen: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM observations WHERE provider=?1 AND account_scope=?2 AND response_id=?3)", params![r.provider,r.account_scope,r.response_id], |x| x.get(0))?;
    match old {
        None if !seen => {
            insert(tx, &r, &hash)?;
            observe(tx, source, offset, &hash, "accepted", None, Some(&r))?;
            out.accepted += 1
        }
        Some(x) if x == hash => {
            observe(tx, source, offset, &hash, "duplicate", None, Some(&r))?;
            out.duplicates += 1
        }
        _ => {
            tx.execute("DELETE FROM canonical_responses WHERE provider=?1 AND account_scope=?2 AND response_id=?3",params![r.provider,r.account_scope,r.response_id])?;
            observe(
                tx,
                source,
                offset,
                &hash,
                "conflict",
                Some("conflicting_response_values"),
                Some(&r),
            )?;
            out.conflicts += 1
        }
    }
    Ok(())
}
struct Parsed {
    provider: String,
    account_scope: String,
    response_id: String,
    parent_response_id: Option<String>,
    root_task_family: Option<String>,
    event_at: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    input: Option<i64>,
    cached: Option<i64>,
    output: Option<i64>,
}
fn parse(line: &str) -> std::result::Result<Parsed, String> {
    let v: Value = serde_json::from_str(line).map_err(|_| "malformed_json".to_string())?;
    if v.get("type").and_then(Value::as_str) == Some("event_msg")
        && v.pointer("/payload/type").and_then(Value::as_str) == Some("token_count")
    {
        let payload = v.get("payload").ok_or("missing_payload")?;
        let info = payload.get("info").ok_or("missing_usage")?;
        let usage = info.get("last_token_usage").ok_or("missing_last_usage")?;
        let n = |key: &str| usage.get(key).and_then(Value::as_i64);
        let input = n("input_tokens");
        let cached = n("cached_input_tokens");
        let output = n("output_tokens");
        if input.is_none() && cached.is_none() && output.is_none() {
            return Err("unknown_usage".into());
        }
        let total = n("total_tokens").or_else(|| n("total_token_usage"));
        let event_at = v
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or("missing_timestamp")?;
        let response_id = info
            .get("response_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("event:{}:{}", event_at, total.unwrap_or(-1)));
        return Ok(Parsed {
            provider: "codex".into(),
            account_scope: "unknown-local-source".into(),
            response_id,
            parent_response_id: payload
                .pointer("/context/parent_thread_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            root_task_family: payload
                .pointer("/context/root_task_family")
                .and_then(Value::as_str)
                .map(str::to_owned),
            event_at: Some(event_at),
            model: payload
                .pointer("/info/model")
                .and_then(Value::as_str)
                .map(str::to_owned),
            effort: None,
            input,
            cached,
            output,
        });
    }
    if v.get("type").and_then(Value::as_str) != Some("token_usage_record") {
        return Err("unsupported_record_type".into());
    }
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let n = |k: &str| v.get(k).and_then(Value::as_i64);
    Ok(Parsed {
        provider: s("provider").ok_or("missing_provider")?,
        account_scope: s("account_scope").unwrap_or_else(|| "unknown-local-source".into()),
        response_id: s("response_id").ok_or("missing_response_id")?,
        parent_response_id: s("parent_response_id"),
        root_task_family: s("root_task_family"),
        event_at: s("event_at"),
        model: s("model"),
        effort: s("effort"),
        input: n("input_tokens"),
        cached: n("cached_input_tokens"),
        output: n("output_tokens"),
    })
}
fn observe(
    tx: &Transaction<'_>,
    source: &str,
    offset: i64,
    hash: &str,
    verdict: &str,
    reason: Option<&str>,
    r: Option<&Parsed>,
) -> Result<()> {
    tx.execute("INSERT INTO observations(source_key,byte_offset,record_hash,verdict,reason,provider,account_scope,response_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![source,offset,hash,verdict,reason,r.map(|x|&x.provider),r.map(|x|&x.account_scope),r.map(|x|&x.response_id)])?;
    Ok(())
}
fn insert(tx: &Transaction<'_>, r: &Parsed, hash: &str) -> Result<()> {
    tx.execute("INSERT INTO canonical_responses(provider,account_scope,response_id,record_hash,parent_response_id,root_task_family,event_at,model,effort,input_tokens,cached_input_tokens,output_tokens) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![r.provider,r.account_scope,r.response_id,hash,r.parent_response_id,r.root_task_family,r.event_at,r.model,r.effort,r.input,r.cached,r.output])?;
    Ok(())
}
fn hash(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}
fn prefix_hash(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut b = vec![0; 4096];
    let n = f.read(&mut b)?;
    Ok(hash(&b[..n]))
}
#[cfg(unix)]
fn file_identity(meta: &fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("{}:{}", meta.dev(), meta.ino())
}
#[cfg(not(unix))]
fn file_identity(meta: &fs::Metadata) -> String {
    format!("{}", meta.len())
}
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS source_files (source_key TEXT PRIMARY KEY,identity TEXT NOT NULL,generation INTEGER NOT NULL,path TEXT NOT NULL,prefix_hash TEXT NOT NULL,cursor INTEGER NOT NULL,source_len INTEGER NOT NULL,updated_at TEXT,source_mtime_ns INTEGER NOT NULL DEFAULT 0,UNIQUE(identity,generation));
CREATE TABLE IF NOT EXISTS observations (source_key TEXT NOT NULL,byte_offset INTEGER NOT NULL,record_hash TEXT NOT NULL,verdict TEXT NOT NULL CHECK(verdict IN ('accepted','duplicate','conflict','quarantine')),reason TEXT,provider TEXT,account_scope TEXT,response_id TEXT,PRIMARY KEY(source_key,byte_offset));
CREATE TABLE IF NOT EXISTS canonical_responses (provider TEXT NOT NULL,account_scope TEXT NOT NULL,response_id TEXT NOT NULL,record_hash TEXT NOT NULL,parent_response_id TEXT,root_task_family TEXT,event_at TEXT,model TEXT,effort TEXT,input_tokens INTEGER,cached_input_tokens INTEGER,output_tokens INTEGER,PRIMARY KEY(provider,account_scope,response_id));
CREATE INDEX IF NOT EXISTS observations_identity ON observations(provider,account_scope,response_id);CREATE INDEX IF NOT EXISTS canonical_time ON canonical_responses(event_at);CREATE INDEX IF NOT EXISTS canonical_family ON canonical_responses(root_task_family);"#;
