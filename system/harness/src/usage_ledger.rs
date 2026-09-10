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
    /// Token-count snapshots retained as stream state, not canonical responses.
    pub noncanonical: u64,
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
    pub cache_write_input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    pub accepted: u64,
    pub noncanonical: u64,
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
        for column in [
            "cache_write_input_tokens INTEGER",
            "reasoning_output_tokens INTEGER",
            "total_tokens INTEGER",
        ] {
            let _ = conn.execute(
                &format!("ALTER TABLE canonical_responses ADD COLUMN {column}"),
                [],
            );
        }
        let _ = conn.execute(
            "ALTER TABLE canonical_responses ADD COLUMN session_id TEXT",
            [],
        );
        for column in [
            "cumulative_cache_write INTEGER",
            "cumulative_reasoning INTEGER",
            "cumulative_total INTEGER",
        ] {
            let _ = conn.execute(
                &format!("ALTER TABLE codex_session_state ADD COLUMN {column}"),
                [],
            );
        }
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
        // Decode complete lines before opening the transaction, but retain only
        // typed accounting fields and a fingerprint. Raw JSON never reaches the
        // ledger or the transaction state machine.
        let mut pending: Vec<(i64, std::result::Result<Event, String>, String)> = Vec::new();
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
            let text = String::from_utf8_lossy(&line)
                .trim_end_matches(['\r', '\n'])
                .to_string();
            let record_hash = hash(text.as_bytes());
            pending.push((start as i64, parse_event(&text), record_hash));
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
        for (byte_offset, event, record_hash) in pending {
            import_event(&tx, &source_key, byte_offset, &record_hash, event, &mut out)?;
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
        let mut s=self.conn.prepare("SELECT provider,account_scope,response_id,parent_response_id,root_task_family,event_at,model,effort,input_tokens,cached_input_tokens,cache_write_input_tokens,output_tokens,reasoning_output_tokens,total_tokens FROM canonical_responses ORDER BY event_at,response_id LIMIT ?1 OFFSET ?2")?;
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
                    cache_write_input_tokens: r.get(10)?,
                    output_tokens: r.get(11)?,
                    reasoning_output_tokens: r.get(12)?,
                    total_tokens: r.get(13)?,
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
        c.noncanonical =
            self.conn
                .query_row("SELECT count(*) FROM codex_stream_events", [], |r| r.get(0))?;
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
fn import_event(
    tx: &Transaction<'_>,
    source: &str,
    offset: i64,
    record_hash: &str,
    event: std::result::Result<Event, String>,
    out: &mut ImportResult,
) -> Result<()> {
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
    let event = match event {
        Ok(event) => event,
        Err(reason) => {
            observe(
                tx,
                source,
                offset,
                record_hash,
                "quarantine",
                Some(&reason),
                None,
            )?;
            out.quarantined += 1;
            return Ok(());
        }
    };
    match event {
        Event::Ignore => Ok(()),
        Event::Canonical(mut record) => {
            hydrate_from_session(tx, source, &mut record)?;
            import_parsed(tx, source, offset, record_hash, record, out)
        }
        Event::SessionMeta(meta) => {
            upsert_session(tx, source, &meta)?;
            backfill_session_attribution(tx, source, &meta.session_id)?;
            Ok(())
        }
        Event::TurnContext(context) => {
            let Some(session_id) = active_session(tx, source)? else {
                // A context line before session metadata is non-candidate
                // metadata. It advances the source cursor but is not bad usage.
                return Ok(());
            };
            tx.execute(
                "UPDATE codex_session_state SET model=COALESCE(?1,model),effort=COALESCE(?2,effort),root_task_family=COALESCE(?3,root_task_family),parent_thread_id=COALESCE(?4,parent_thread_id) WHERE source_key=?5 AND session_id=?6",
                params![context.model, context.effort, context.root_task_family, context.parent_thread_id, source, session_id],
            )?;
            backfill_session_attribution(tx, source, &session_id)?;
            Ok(())
        }
        Event::TokenCount(count) => import_token_count(tx, source, offset, record_hash, count, out),
    }
}

fn import_parsed(
    tx: &Transaction<'_>,
    source: &str,
    offset: i64,
    record_hash: &str,
    mut r: Parsed,
    out: &mut ImportResult,
) -> Result<()> {
    if r.account_scope == "unknown-local-source" {
        r.account_scope = format!("unknown-local-source:{source}");
    }
    let old:Option<String>=tx.query_row("SELECT record_hash FROM canonical_responses WHERE provider=?1 AND account_scope=?2 AND response_id=?3",params![r.provider,r.account_scope,r.response_id],|x|x.get(0)).optional()?;
    let seen: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM observations WHERE provider=?1 AND account_scope=?2 AND response_id=?3)", params![r.provider,r.account_scope,r.response_id], |x| x.get(0))?;
    match old {
        None if !seen => {
            insert(tx, &r, record_hash)?;
            observe(tx, source, offset, record_hash, "accepted", None, Some(&r))?;
            out.accepted += 1
        }
        Some(x) if x == record_hash => {
            observe(tx, source, offset, record_hash, "duplicate", None, Some(&r))?;
            out.duplicates += 1
        }
        _ => {
            tx.execute("DELETE FROM canonical_responses WHERE provider=?1 AND account_scope=?2 AND response_id=?3",params![r.provider,r.account_scope,r.response_id])?;
            observe(
                tx,
                source,
                offset,
                record_hash,
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
    cache_write: Option<i64>,
    output: Option<i64>,
    reasoning: Option<i64>,
    total: Option<i64>,
    session_id: Option<String>,
}
#[derive(Default)]
struct SessionMeta {
    session_id: String,
    model: Option<String>,
    effort: Option<String>,
    root_task_family: Option<String>,
    parent_thread_id: Option<String>,
}
#[derive(Default)]
struct TurnContext {
    model: Option<String>,
    effort: Option<String>,
    root_task_family: Option<String>,
    parent_thread_id: Option<String>,
}
struct TokenCount {
    event_at: String,
    session_id: Option<String>,
    last: TokenUsage,
    total: TokenUsage,
}
#[derive(Default, Clone, Copy)]
struct TokenUsage {
    input: Option<i64>,
    cached: Option<i64>,
    cache_write: Option<i64>,
    output: Option<i64>,
    reasoning: Option<i64>,
    total: Option<i64>,
}
enum Event {
    Ignore,
    Canonical(Parsed),
    SessionMeta(SessionMeta),
    TurnContext(TurnContext),
    TokenCount(TokenCount),
}

fn parse_event(line: &str) -> std::result::Result<Event, String> {
    let v: Value = serde_json::from_str(line).map_err(|_| "malformed_json".to_string())?;
    let payload = v.get("payload").unwrap_or(&Value::Null);
    let field =
        |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    if v.get("type").and_then(Value::as_str) == Some("session_meta") {
        let Some(session_id) = field(payload, "id") else {
            return Ok(Event::Ignore);
        };
        return Ok(Event::SessionMeta(SessionMeta {
            session_id,
            model: field(payload, "model"),
            effort: field(payload, "effort"),
            root_task_family: field(payload, "root_task_family"),
            parent_thread_id: payload
                .pointer("/source/subagent/thread_spawn/parent_thread_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }));
    }
    if v.get("type").and_then(Value::as_str) == Some("turn_context") {
        return Ok(Event::TurnContext(TurnContext {
            model: field(payload, "model"),
            effort: field(payload, "effort"),
            root_task_family: field(payload, "root_task_family"),
            parent_thread_id: field(payload, "parent_thread_id"),
        }));
    }
    if v.pointer("/payload/type").and_then(Value::as_str) == Some("token_count") {
        let info = payload.get("info").ok_or("missing_usage")?;
        let usage = |name: &str| {
            let u = info.get(name).unwrap_or(&Value::Null);
            TokenUsage {
                input: u.get("input_tokens").and_then(Value::as_i64),
                cached: u.get("cached_input_tokens").and_then(Value::as_i64),
                cache_write: u.get("cache_write_input_tokens").and_then(Value::as_i64),
                output: u.get("output_tokens").and_then(Value::as_i64),
                reasoning: u.get("reasoning_output_tokens").and_then(Value::as_i64),
                total: u.get("total_tokens").and_then(Value::as_i64),
            }
        };
        let last = usage("last_token_usage");
        let total = usage("total_token_usage");
        if !last.any() && !total.any() {
            return Err("unknown_usage".into());
        }
        let event_at = v
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or("missing_timestamp")?;
        return Ok(Event::TokenCount(TokenCount {
            event_at,
            session_id: field(info, "thread_id").or_else(|| field(payload, "thread_id")),
            last,
            total,
        }));
    }
    if v.get("type").and_then(Value::as_str) != Some("token_usage_record") {
        return Ok(Event::Ignore);
    }
    // Current Codex archives place the response identity and all usage beneath
    // `payload`; the outer envelope intentionally has no provider field.
    if payload.is_object() {
        let response_id = field(payload, "response_id").ok_or("missing_response_id")?;
        let usage = payload.get("usage").unwrap_or(&Value::Null);
        let n = |key: &str| usage.get(key).and_then(Value::as_i64);
        let session_id = field(payload, "session_id");
        let thread_id = field(payload, "thread_id");
        let parent_response_id = match (thread_id, session_id.clone()) {
            (Some(thread), Some(session)) if thread != session => Some(thread),
            (Some(thread), None) => Some(thread),
            _ => None,
        };
        return Ok(Event::Canonical(Parsed {
            provider: "codex".into(),
            account_scope: "unknown-local-source".into(),
            response_id,
            parent_response_id,
            root_task_family: field(payload, "root_turn_id"),
            event_at: field(&v, "timestamp"),
            model: field(payload, "model"),
            effort: field(payload, "effort"),
            input: n("input_tokens"),
            cached: n("cached_input_tokens"),
            cache_write: n("cache_write_input_tokens"),
            output: n("output_tokens"),
            reasoning: n("reasoning_output_tokens"),
            total: n("total_tokens"),
            session_id,
        }));
    }
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let n = |k: &str| v.get(k).and_then(Value::as_i64);
    Ok(Event::Canonical(Parsed {
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
        cache_write: n("cache_write_input_tokens"),
        output: n("output_tokens"),
        reasoning: n("reasoning_output_tokens"),
        total: n("total_tokens"),
        session_id: None,
    }))
}
fn hydrate_from_session(tx: &Transaction<'_>, source: &str, record: &mut Parsed) -> Result<()> {
    let Some(session_id) = record.session_id.as_deref() else {
        return Ok(());
    };
    let state: Option<(Option<String>, Option<String>, Option<String>, Option<String>)> = tx.query_row(
        "SELECT model,effort,root_task_family,parent_thread_id FROM codex_session_state WHERE source_key=?1 AND session_id=?2",
        params![source, session_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).optional()?;
    if let Some((model, effort, root, parent)) = state {
        if record.model.is_none() {
            record.model = model;
        }
        if record.effort.is_none() {
            record.effort = effort;
        }
        if record.root_task_family.is_none() {
            record.root_task_family = root;
        }
        if record.parent_response_id.is_none() {
            record.parent_response_id = parent;
        }
    }
    Ok(())
}
fn backfill_session_attribution(
    tx: &Transaction<'_>,
    source: &str,
    session_id: &str,
) -> Result<()> {
    tx.execute(
        "UPDATE canonical_responses SET model=COALESCE(model,(SELECT model FROM codex_session_state WHERE source_key=?1 AND session_id=?2)),effort=COALESCE(effort,(SELECT effort FROM codex_session_state WHERE source_key=?1 AND session_id=?2)),root_task_family=COALESCE(root_task_family,(SELECT root_task_family FROM codex_session_state WHERE source_key=?1 AND session_id=?2)),parent_response_id=COALESCE(parent_response_id,(SELECT parent_thread_id FROM codex_session_state WHERE source_key=?1 AND session_id=?2)) WHERE session_id=?2 AND account_scope=?3",
        params![source, session_id, format!("unknown-local-source:{source}")],
    )?;
    Ok(())
}
impl TokenUsage {
    fn any(self) -> bool {
        self.input.is_some()
            || self.cached.is_some()
            || self.cache_write.is_some()
            || self.output.is_some()
            || self.reasoning.is_some()
            || self.total.is_some()
    }
    fn complete(self) -> bool {
        self.input.is_some() && self.cached.is_some() && self.output.is_some()
    }
}
fn upsert_session(tx: &Transaction<'_>, source: &str, meta: &SessionMeta) -> Result<()> {
    tx.execute(
        "INSERT INTO codex_session_state(source_key,session_id,model,effort,root_task_family,parent_thread_id,cumulative_input,cumulative_cached,cumulative_output) VALUES(?1,?2,?3,?4,?5,?6,0,0,0) ON CONFLICT(source_key,session_id) DO UPDATE SET model=COALESCE(excluded.model,model),effort=COALESCE(excluded.effort,effort),root_task_family=COALESCE(excluded.root_task_family,root_task_family),parent_thread_id=COALESCE(excluded.parent_thread_id,parent_thread_id)",
        params![source, meta.session_id, meta.model, meta.effort, meta.root_task_family, meta.parent_thread_id],
    )?;
    Ok(())
}
fn active_session(tx: &Transaction<'_>, source: &str) -> Result<Option<String>> {
    Ok(tx.query_row("SELECT session_id FROM codex_session_state WHERE source_key=?1 ORDER BY rowid DESC LIMIT 1", [source], |row| row.get(0)).optional()?)
}
fn import_token_count(
    tx: &Transaction<'_>,
    source: &str,
    offset: i64,
    record_hash: &str,
    count: TokenCount,
    out: &mut ImportResult,
) -> Result<()> {
    let session_id = match count.session_id.or(active_session(tx, source)?) {
        Some(id) => id,
        None => {
            observe(
                tx,
                source,
                offset,
                record_hash,
                "quarantine",
                Some("missing_session_context"),
                None,
            )?;
            out.quarantined += 1;
            return Ok(());
        }
    };
    let state: Option<(Option<String>, Option<String>, Option<String>, Option<String>, i64, i64, Option<i64>, i64, Option<i64>, Option<i64>)> = tx.query_row(
        "SELECT model,effort,root_task_family,parent_thread_id,cumulative_input,cumulative_cached,cumulative_cache_write,cumulative_output,cumulative_reasoning,cumulative_total FROM codex_session_state WHERE source_key=?1 AND session_id=?2",
        params![source, session_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?)),
    ).optional()?;
    let Some((
        model,
        effort,
        root_task_family,
        parent_thread_id,
        old_input,
        old_cached,
        old_cache_write,
        old_output,
        old_reasoning,
        old_total,
    )) = state
    else {
        observe(
            tx,
            source,
            offset,
            record_hash,
            "quarantine",
            Some("unknown_session"),
            None,
        )?;
        out.quarantined += 1;
        return Ok(());
    };
    let (usage, next) = if count.total.complete() {
        let total = (
            count.total.input.unwrap(),
            count.total.cached.unwrap(),
            count.total.output.unwrap(),
        );
        let delta = (
            total.0 - old_input,
            total.1 - old_cached,
            total.2 - old_output,
        );
        if delta.0 >= 0 && delta.1 >= 0 && delta.2 >= 0 {
            (delta, total)
        } else if count.last.complete() {
            (
                (
                    count.last.input.unwrap(),
                    count.last.cached.unwrap(),
                    count.last.output.unwrap(),
                ),
                total,
            )
        } else {
            observe(
                tx,
                source,
                offset,
                record_hash,
                "quarantine",
                Some("counter_reset_without_last_usage"),
                None,
            )?;
            out.quarantined += 1;
            return Ok(());
        }
    } else if count.last.complete() {
        let last = (
            count.last.input.unwrap(),
            count.last.cached.unwrap(),
            count.last.output.unwrap(),
        );
        (
            last,
            (old_input + last.0, old_cached + last.1, old_output + last.2),
        )
    } else {
        observe(
            tx,
            source,
            offset,
            record_hash,
            "quarantine",
            Some("incomplete_token_usage"),
            None,
        )?;
        out.quarantined += 1;
        return Ok(());
    };
    let (cache_write, next_cache_write) = supplemental_delta(
        count.total.cache_write,
        old_cache_write,
        count.last.cache_write,
    );
    let (reasoning, next_reasoning) =
        supplemental_delta(count.total.reasoning, old_reasoning, count.last.reasoning);
    let (provider_total, next_total) =
        supplemental_delta(count.total.total, old_total, count.last.total);
    tx.execute("UPDATE codex_session_state SET cumulative_input=?1,cumulative_cached=?2,cumulative_cache_write=?3,cumulative_output=?4,cumulative_reasoning=?5,cumulative_total=?6 WHERE source_key=?7 AND session_id=?8", params![next.0,next.1,next_cache_write,next.2,next_reasoning,next_total,source,session_id])?;
    if usage == (0, 0, 0) {
        return Ok(());
    }
    // Snapshot IDs are synthesized and cannot safely deduplicate against the
    // provider's stable response records. Keep only state and a noncanonical
    // coordinate so reports can mark response coverage incomplete.
    tx.execute(
        "INSERT INTO codex_stream_events(source_key,byte_offset,event_at,session_id,kind) VALUES(?1,?2,?3,?4,'token_count_snapshot')",
        params![source, offset, count.event_at, session_id],
    )?;
    let _ = (
        usage,
        cache_write,
        reasoning,
        provider_total,
        model,
        effort,
        root_task_family,
        parent_thread_id,
    );
    out.noncanonical += 1;
    Ok(())
}
/// Extra provider counters remain `None` when the source did not report them.
/// They are never reconstructed from input/output and are reset independently.
fn supplemental_delta(
    total: Option<i64>,
    old: Option<i64>,
    last: Option<i64>,
) -> (Option<i64>, Option<i64>) {
    match total {
        Some(value) => match old {
            Some(previous) if value >= previous => (Some(value - previous), Some(value)),
            _ => (last, Some(value)),
        },
        None => (
            last,
            old.zip(last).map(|(previous, delta)| previous + delta),
        ),
    }
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
    tx.execute("INSERT INTO canonical_responses(provider,account_scope,response_id,record_hash,parent_response_id,root_task_family,event_at,model,effort,input_tokens,cached_input_tokens,cache_write_input_tokens,output_tokens,reasoning_output_tokens,total_tokens,session_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",params![r.provider,r.account_scope,r.response_id,hash,r.parent_response_id,r.root_task_family,r.event_at,r.model,r.effort,r.input,r.cached,r.cache_write,r.output,r.reasoning,r.total,r.session_id])?;
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
CREATE TABLE IF NOT EXISTS canonical_responses (provider TEXT NOT NULL,account_scope TEXT NOT NULL,response_id TEXT NOT NULL,record_hash TEXT NOT NULL,parent_response_id TEXT,root_task_family TEXT,event_at TEXT,model TEXT,effort TEXT,input_tokens INTEGER,cached_input_tokens INTEGER,cache_write_input_tokens INTEGER,output_tokens INTEGER,reasoning_output_tokens INTEGER,total_tokens INTEGER,session_id TEXT,PRIMARY KEY(provider,account_scope,response_id));
CREATE TABLE IF NOT EXISTS codex_session_state (source_key TEXT NOT NULL,session_id TEXT NOT NULL,model TEXT,effort TEXT,root_task_family TEXT,parent_thread_id TEXT,cumulative_input INTEGER NOT NULL DEFAULT 0,cumulative_cached INTEGER NOT NULL DEFAULT 0,cumulative_cache_write INTEGER,cumulative_output INTEGER NOT NULL DEFAULT 0,cumulative_reasoning INTEGER,cumulative_total INTEGER,PRIMARY KEY(source_key,session_id));
CREATE TABLE IF NOT EXISTS codex_stream_events (source_key TEXT NOT NULL,byte_offset INTEGER NOT NULL,event_at TEXT,session_id TEXT,kind TEXT NOT NULL,PRIMARY KEY(source_key,byte_offset));
CREATE INDEX IF NOT EXISTS observations_identity ON observations(provider,account_scope,response_id);CREATE INDEX IF NOT EXISTS canonical_time ON canonical_responses(event_at);CREATE INDEX IF NOT EXISTS canonical_family ON canonical_responses(root_task_family);"#;
