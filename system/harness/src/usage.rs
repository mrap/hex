//! `hex usage` — Claude usage metrics namespace. `hex usage burn` = spend guardrail (credit-burn P0, decision 2026-06-12).
//!
//! Reads Claude Code transcripts RECURSIVELY (subagent transcripts live in
//! `<project>/<session>/subagents/agent-*.jsonl` — a one-level scan undercounts
//! by 15–40%), dedupes by requestId, prices at current list rates, and computes
//! the trailing-window burn rate. Above threshold → loud alert (stderr +
//! telemetry + macOS notification via `alert::notify`, 6h dedupe). Never a
//! silent cap (S6): the guardrail only observes and alerts.
//!
//! Recurring cadence: the `hex-burn-guard` worker runs `hex usage burn` every 10m.
//! Future metrics (daily totals, by-model, by-session) belong in this namespace.

use chrono::{DateTime, Duration, TimeZone, Utc};
use clap::Subcommand;
use hex::usage_ledger::{CanonicalRow, ImportOptions, ImportResult, UsageLedger};
use hex::usage_reporting;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum UsageCommands {
    /// Import usage into the durable ledger from one or (by default) every
    /// known source kind: `codex-jsonl` (local Codex JSONL transcripts) and
    /// `claude-transcripts` (local Claude Code transcripts under
    /// $HOME/.claude/projects, provider `claude-code`). Bare `hex usage
    /// collect` runs every kind in sequence so no LLM path silently drops out
    /// of the ledger (decision usage-ledger-provider-agnostic-2026-09-14).
    Collect {
        /// Restrict this run to one source kind. Omit to run all kinds.
        #[arg(long, value_parser = ["codex-jsonl", "claude-transcripts"])]
        source_kind: Option<String>,
        /// Explicit local JSONL source (codex-jsonl only). Defaults to
        /// $HEX_DIR/.hex/usage/codex.jsonl.
        #[arg(long)]
        source: Option<PathBuf>,
        /// Codex home for discovery (codex-jsonl only). Defaults to $HOME/.codex.
        #[arg(long)]
        codex_root: Option<PathBuf>,
        /// Claude Code projects root to scan recursively (claude-transcripts
        /// only). Defaults to $HOME/.claude/projects.
        #[arg(long)]
        claude_root: Option<PathBuf>,
        /// Ledger path. Defaults to $HEX_DIR/.hex/usage/usage.db.
        #[arg(long)]
        ledger: Option<PathBuf>,
        /// Maximum complete records committed per source kind in this run.
        #[arg(long, default_value_t = 1_000)]
        max_records: usize,
    },
    /// Write a deterministic local JSON usage summary
    Report {
        /// Ledger path. Defaults to $HEX_DIR/.hex/usage/usage.db.
        #[arg(long)]
        ledger: Option<PathBuf>,
        /// Report path. Defaults to $HEX_DIR/.hex/usage/report.json.
        #[arg(long)]
        output: Option<PathBuf>,
        /// RFC3339 UTC cutoff. Omit to use the current time.
        #[arg(long)]
        cutoff: Option<String>,
        /// Locally query contributor IDs by `model`, `family`, or `child`.
        #[arg(long)]
        detail_dimension: Option<String>,
        /// Model or family key for a contributor detail query.
        #[arg(long)]
        detail_key: Option<String>,
        /// Number of matching response IDs to skip in a detail query.
        #[arg(long, default_value_t = 0)]
        detail_offset: usize,
        /// Maximum response IDs returned by a detail query.
        #[arg(long, default_value_t = 100)]
        detail_limit: usize,
    },
    /// Trailing-window burn rate; alert if above threshold
    Burn {
        /// Alert threshold in USD per hour
        #[arg(long, default_value_t = 100.0)]
        threshold: f64,
        /// Trailing window in minutes
        #[arg(long, default_value_t = 60)]
        window_mins: i64,
        /// Claude Code projects dir (transcript root)
        #[arg(long)]
        projects_dir: Option<PathBuf>,
    },
}

/// List prices per MTok: (input, output, cache_read, cache_write_5m).
/// Source: claude-api reference 2026-06 (Opus 4.x $5/$25; Fable 5 $10/$50;
/// cache read = 0.1x input, cache write = 1.25x input). Unknown claude models
/// fall back to Fable rates — overcounting beats a silent $0 (OBS-024).
fn price(model: &str) -> Option<(f64, f64, f64, f64)> {
    let m = model.to_ascii_lowercase();
    if m.is_empty() || m == "<synthetic>" {
        return None;
    }
    Some(if m.contains("haiku") {
        (1.0, 5.0, 0.1, 1.25)
    } else if m.contains("sonnet") {
        (3.0, 15.0, 0.3, 3.75)
    } else if m.contains("opus") {
        (5.0, 25.0, 0.5, 6.25)
    } else if m.contains("fable") || m.contains("mythos") || m.contains("claude") {
        (10.0, 50.0, 1.0, 12.5)
    } else {
        return None;
    })
}

#[derive(Debug, Default)]
pub struct WindowSpend {
    pub usd: f64,
    pub turns: usize,
    pub files: usize,
}

/// Sum deduped spend for assistant turns timestamped within (now - window, now].
pub fn window_spend(projects_dir: &Path, now: DateTime<Utc>, window: Duration) -> WindowSpend {
    let cutoff = now - window;
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = WindowSpend::default();
    for entry in walkdir::WalkDir::new(projects_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_file() && e.path().extension().map(|x| x == "jsonl").unwrap_or(false)
        })
    {
        // Skip files untouched since before the window — cheap and safe (a
        // file containing in-window turns must have been written in-window).
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if DateTime::<Utc>::from(modified) < cutoff {
                    continue;
                }
            }
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        out.files += 1;
        for line in content.lines() {
            if !line.contains("\"usage\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
                continue;
            }
            let Some(req) = v.get("requestId").and_then(|r| r.as_str()) else {
                continue;
            };
            let Some(ts) = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            else {
                continue;
            };
            let ts = ts.with_timezone(&Utc);
            if ts <= cutoff || ts > now || seen.contains(req) {
                continue;
            }
            let msg = &v["message"];
            let Some(p) = msg.get("model").and_then(|m| m.as_str()).and_then(price) else {
                continue;
            };
            let u = &msg["usage"];
            let tok = |k: &str| u.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
            seen.insert(req.to_string());
            out.turns += 1;
            out.usd += (tok("input_tokens") * p.0
                + tok("output_tokens") * p.1
                + tok("cache_read_input_tokens") * p.2
                + tok("cache_creation_input_tokens") * p.3)
                / 1e6;
        }
    }
    out
}

fn default_projects_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    Path::new(&home).join(".claude/projects")
}

/// Result of one `claude-transcripts` collection pass.
#[derive(Debug, Default, PartialEq, Eq)]
struct ClaudeTranscriptImport {
    /// New canonical ledger rows written this pass (one per distinct
    /// `requestId`, deduped across files — including nested subagent
    /// transcripts under `<session>/subagents/agent-*.jsonl`).
    accepted: u64,
    duplicates: u64,
    conflicts: u64,
    /// The scan found more candidate rows than `max_records` allowed
    /// committing this pass. Never silent (S6): new rows are prioritized
    /// over already-canonical ones (see `import_claude_transcripts`) so a
    /// sustained backlog still converges, but a caller must still surface
    /// this rather than reporting a quiet, permanently-partial "ok".
    truncated: bool,
    /// Set when the ledger import itself failed (e.g. rebuild required).
    /// A loud, non-zero-exit failure per S6 — never a silent skip.
    error: Option<String>,
}

/// One deduped Claude Code assistant turn, keyed by `requestId`, pending
/// translation into a `CanonicalRow`.
struct ClaudeTurnCandidate {
    event_at: String,
    session_id: Option<String>,
    model: Option<String>,
    input_tokens: i64,
    cached_input_tokens: i64,
    cache_write_input_tokens: i64,
    output_tokens: i64,
}

/// `claude-transcripts` source (task Txv7phcj8): recursively scans
/// `claude_root` (default `$HOME/.claude/projects`) for assistant-turn usage
/// blocks — including nested subagent transcripts under
/// `<session>/subagents/agent-*.jsonl`, the same shape `window_spend` (`hex
/// usage burn`) already walks recursively — and imports one canonical
/// `claude-code`/`local-claude-code` ledger row per distinct `requestId` via
/// `UsageLedger::import_canonical_rows`. Re-import is idempotent: an
/// unchanged transcript re-scan reports `accepted: 0`.
fn import_claude_transcripts(
    claude_root: &Path,
    ledger: &mut UsageLedger,
    max_records: usize,
) -> ClaudeTranscriptImport {
    if !claude_root.exists() {
        return ClaudeTranscriptImport::default();
    }
    let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(claude_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl")
        })
        .map(|e| e.into_path())
        .collect();
    // Deterministic scan order: with several files sharing a requestId (a
    // resumed session copies its transcript), the first file visited wins.
    paths.sort();

    let mut by_request: BTreeMap<String, ClaudeTurnCandidate> = BTreeMap::new();
    for path in paths {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines() {
            if !line.contains("\"usage\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
                continue;
            }
            let Some(request_id) = v.get("requestId").and_then(|r| r.as_str()) else {
                continue;
            };
            if by_request.contains_key(request_id) {
                continue;
            }
            let Some(event_at) = v.get("timestamp").and_then(|t| t.as_str()) else {
                continue;
            };
            let msg = &v["message"];
            let usage = &msg["usage"];
            let tok = |k: &str| usage.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
            // Claude's `cache_read_input_tokens` is a SIBLING of
            // `input_tokens`, but the ledger's `cached_input_tokens` must be
            // a SUBSET of `input_tokens` (summary_groups's validity
            // predicate requires input_tokens>=cached_input_tokens), so fold
            // cache reads into the ledger's input total.
            let raw_input = tok("input_tokens");
            let cache_read = tok("cache_read_input_tokens");
            by_request.insert(
                request_id.to_string(),
                ClaudeTurnCandidate {
                    event_at: event_at.to_string(),
                    session_id: v.get("sessionId").and_then(|s| s.as_str()).map(str::to_string),
                    model: msg.get("model").and_then(|m| m.as_str()).map(str::to_string),
                    input_tokens: raw_input + cache_read,
                    cached_input_tokens: cache_read,
                    cache_write_input_tokens: tok("cache_creation_input_tokens"),
                    output_tokens: tok("output_tokens"),
                },
            );
        }
    }
    // A backlog larger than `max_records` must not truncate in whatever
    // order `by_request` happens to enumerate: prioritize requestIds not yet
    // canonical (so a sustained backlog eventually covers every request
    // instead of re-selecting the same lexicographically-first slice every
    // run), then within each bucket prefer the most recent activity first.
    let existing = ledger
        .response_ids("claude-code", "local-claude-code")
        .unwrap_or_default();
    let (mut new_candidates, mut known_candidates): (Vec<_>, Vec<_>) = by_request
        .into_iter()
        .partition(|(request_id, _)| !existing.contains(request_id));
    let by_recency = |a: &(String, ClaudeTurnCandidate), b: &(String, ClaudeTurnCandidate)| {
        b.1.event_at.cmp(&a.1.event_at)
    };
    new_candidates.sort_by(by_recency);
    known_candidates.sort_by(by_recency);
    let total_candidates = new_candidates.len() + known_candidates.len();
    let selected: Vec<_> = new_candidates
        .into_iter()
        .chain(known_candidates)
        .take(max_records)
        .collect();
    let truncated = total_candidates > selected.len();

    let rows: Vec<CanonicalRow> = selected
        .into_iter()
        .map(|(request_id, c)| CanonicalRow {
            provider: "claude-code".into(),
            account_scope: "local-claude-code".into(),
            response_id: request_id,
            parent_response_id: None,
            root_task_family: c.session_id,
            event_at: Some(c.event_at),
            model: c.model,
            effort: None,
            input_tokens: Some(c.input_tokens),
            cached_input_tokens: Some(c.cached_input_tokens),
            cache_write_input_tokens: Some(c.cache_write_input_tokens),
            output_tokens: Some(c.output_tokens),
            reasoning_output_tokens: None,
            total_tokens: None,
        })
        .collect();
    match ledger.import_canonical_rows("claude-transcripts", rows) {
        Ok(r) => ClaudeTranscriptImport {
            accepted: r.accepted,
            duplicates: r.duplicates,
            conflicts: r.conflicts,
            truncated,
            error: None,
        },
        Err(e) => ClaudeTranscriptImport {
            error: Some(format!("{e:?}")),
            ..Default::default()
        },
    }
}

/// Class-selection seam for the burn guardrail's two alerts, keyed by the alert
/// key each call site uses. The spend-rate breach (`burn-guard`) is the
/// operator's spend signal → the `Spend` rail (push urgent + email). The
/// misconfiguration alert (`burn-guard-config`) — like every other historical
/// alert — stays `Default` (push only).
///
/// Pure so the mapping is unit-testable in this BIN target, where the lib's
/// `cfg(test)` delivery sink is NOT linked (the `hex` lib is a plain dependency
/// of the binary, compiled without `cfg(test)`, so calling `notify*` in a bin
/// test would hit the real curl/gws arms). The class→rails half is proven in
/// `alert.rs::email_classes_send_both_rails` / `default_class_sends_push_only…`;
/// this seam pins the class SELECTION, and the two compose to the full proof.
fn burn_alert_class(key: &str) -> crate::alert::AlertClass {
    match key {
        "burn-guard" => crate::alert::AlertClass::Spend,
        _ => crate::alert::AlertClass::Default,
    }
}

pub fn run(cmd: UsageCommands) -> i32 {
    match cmd {
        UsageCommands::Collect {
            source_kind,
            source,
            codex_root,
            claude_root,
            ledger,
            max_records,
        } => collect(source_kind, source, codex_root, claude_root, ledger, max_records),
        UsageCommands::Report {
            ledger,
            output,
            cutoff,
            detail_dimension,
            detail_key,
            detail_offset,
            detail_limit,
        } => report(
            ledger,
            output,
            cutoff,
            detail_dimension,
            detail_key,
            detail_offset,
            detail_limit,
        ),
        UsageCommands::Burn {
            threshold,
            window_mins,
            projects_dir,
        } => {
            let dir = projects_dir.unwrap_or_else(default_projects_dir);
            if !dir.exists() {
                // S6: a missing transcript root is a config bug, not "zero spend".
                crate::alert::notify_with_class(
                    "burn-guard-config",
                    "burn guardrail misconfigured",
                    &format!("projects dir not found: {}", dir.display()),
                    burn_alert_class("burn-guard-config"),
                );
                return 1;
            }
            let spend = window_spend(&dir, Utc::now(), Duration::minutes(window_mins));
            let rate = spend.usd * 60.0 / window_mins as f64;
            println!(
                "burn: ${:.2} over last {window_mins}m (${rate:.2}/hr) — {} turns, {} files scanned, threshold ${threshold:.0}/hr",
                spend.usd, spend.turns, spend.files
            );
            let _ = crate::telemetry::record(&crate::telemetry::TelemetryEvent {
                source: "burn-guard".into(),
                event: "check".into(),
                status: if rate > threshold { "alert" } else { "ok" }.into(),
                duration_ms: None,
                exit_code: None,
                detail: Some(format!("rate_usd_hr={rate:.2} window_mins={window_mins}")),
            });
            if rate > threshold {
                crate::alert::notify_with_class(
                    "burn-guard",
                    "Claude burn rate over threshold",
                    &format!(
                        "${rate:.0}/hr over the last {window_mins}m (threshold ${threshold:.0}/hr). \
                         Check active sessions/subagents."
                    ),
                    burn_alert_class("burn-guard"),
                );
            }
            0
        }
    }
}

fn usage_dir() -> PathBuf {
    PathBuf::from(std::env::var("HEX_DIR").unwrap_or_else(|_| ".".into()))
        .join(".hex")
        .join("usage")
}
fn default_ledger() -> PathBuf {
    usage_dir().join("usage.db")
}
fn default_report() -> PathBuf {
    usage_dir().join("report.json")
}

fn health(status: &str, detail: String) {
    // Failures are coalesced locally: repeat the same last collector failure
    // does not fill telemetry. No alert or outbound transport is invoked.
    let repeated = status == "error"
        && hex::telemetry::recent(50)
            .ok()
            .and_then(|rows| {
                rows.into_iter()
                    .find(|r| r.source == "usage-tracking" && r.event == "collect")
            })
            .map(|r| r.status == status)
            .unwrap_or(false);
    if !repeated {
        let _ = hex::telemetry::record(&hex::telemetry::TelemetryEvent {
            source: "usage-tracking".into(),
            event: "collect".into(),
            status: status.into(),
            duration_ms: None,
            exit_code: Some(if status == "ok" { 0 } else { 1 }),
            detail: Some(detail),
        });
    }
}

/// A failed bounded collection makes the ledger's next report visibly stale
/// until a later successful collection replaces that health event. This keeps
/// the report honest even when the underlying source file remains readable.
fn collector_is_stale() -> bool {
    let status = hex::telemetry::recent(50)
        .ok()
        .and_then(|rows| {
            rows.into_iter()
                .find(|row| row.source == "usage-tracking" && row.event == "collect")
        })
        .map(|row| row.status);
    collector_status_is_stale(status.as_deref())
}

fn collector_status_is_stale(status: Option<&str>) -> bool {
    status.is_some_and(|status| status != "ok")
}

fn discover(codex_root: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let mut paths = Vec::new();
    let mut issues = Vec::new();
    for dir in [
        codex_root.join("sessions"),
        codex_root.join("archived_sessions"),
    ] {
        if dir.exists() {
            paths.extend(
                walkdir::WalkDir::new(&dir)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|e| {
                        e.file_type().is_file()
                            && e.path().extension().is_some_and(|x| x == "jsonl")
                    })
                    .map(|e| e.into_path()),
            );
        }
    }
    let state = codex_root.join("state_5.sqlite");
    if state.exists() {
        match rusqlite::Connection::open_with_flags(
            &state,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Ok(db) => match db
                .prepare("SELECT rollout_path FROM threads WHERE rollout_path IS NOT NULL")
            {
                Ok(mut s) => {
                    if let Ok(rows) = s.query_map([], |r| r.get::<_, String>(0)) {
                        for row in rows.flatten() {
                            let p = PathBuf::from(row);
                            if p.is_file() {
                                paths.push(p)
                            } else {
                                // state_5.sqlite can retain a rollout path after Codex
                                // removes the session. This is a normal discovery race, not
                                // a collector failure. Keep it visible without making the
                                // scheduled worker unhealthy.
                                eprintln!("usage collect: skipped missing rollout={}", p.display());
                            }
                        }
                    }
                }
                Err(e) => issues.push(format!("state_schema={e}")),
            },
            Err(e) => issues.push(format!("state_unreadable={e}")),
        }
    }
    paths.sort();
    paths.dedup();
    (paths, issues)
}
/// One eligible `phase_runs` row from the **boi-phase-runs** usage source
/// (decision: usage-ledger-provider-agnostic-2026-09-14, task Tsgmstjxk).
///
/// Attribution mapping: `account_scope` is always `"boi"`; `response_id` is
/// `phase_runs.id`; `root_task_family` is `phase_runs.spec_id`; `event_at`
/// is `phase_runs.started_at` (as stored, unparsed). `model` is resolved
/// from `<recipes_dir>/recipe-<id>.yaml` (`settings.goose_model`) when that
/// recipe file exists, else `None` — never a schema migration, since every
/// field maps onto ledger columns that already exist.
// Only `mod tests` calls into this source kind's discovery function today;
// the `collect()` dispatch arm and `--source-kind` CLI plumbing land in the
// sibling task (Txv7phcj8) that also wires up "run all kinds" — allow the
// otherwise-correct dead-code warning until that lands.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct BoiPhaseRunRecord {
    response_id: String,
    provider: String,
    root_task_family: Option<String>,
    event_at: Option<String>,
    model: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

/// Reads eligible **boi-phase-runs** rows: a read-only SQLite open of
/// `boi_db` (`SQLITE_OPEN_READ_ONLY` — never the live write lock), filtered
/// to worker rows (`provider != 'deterministic'`) with measured token usage
/// (`tokens_in > 0 OR tokens_out > 0`). See `BoiPhaseRunRecord` for the
/// attribution mapping. `ORDER BY id` makes the result — and therefore
/// `collect_boi_phase_runs`'s staged JSONL bytes — deterministic across
/// calls: a query plan is otherwise free to reorder rows, which would
/// silently break the ledger's byte-cursor incremental-import assumption
/// (a stable prefix + newly appended rows) that `collect_boi_phase_runs`
/// relies on.
#[allow(dead_code)]
fn discover_boi_phase_runs(
    boi_db: &Path,
    recipes_dir: &Path,
) -> std::result::Result<Vec<BoiPhaseRunRecord>, String> {
    // S6: always a read-only open — this is someone else's live, actively
    // written database (never our write lock).
    let conn = rusqlite::Connection::open_with_flags(
        boi_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("boi_db_unreadable={e}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, spec_id, provider, tokens_in, tokens_out, started_at \
             FROM phase_runs \
             WHERE provider IS NOT NULL AND provider != 'deterministic' \
               AND (COALESCE(tokens_in, 0) > 0 OR COALESCE(tokens_out, 0) > 0) \
             ORDER BY id",
        )
        .map_err(|e| format!("boi_db_schema={e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(|e| format!("boi_db_query={e}"))?;
    let mut records = Vec::new();
    for row in rows {
        let (id, spec_id, provider, tokens_in, tokens_out, started_at) =
            row.map_err(|e| format!("boi_db_row={e}"))?;
        let model = recipe_model_for(recipes_dir, &id);
        records.push(BoiPhaseRunRecord {
            response_id: id,
            provider,
            root_task_family: spec_id,
            event_at: started_at,
            model,
            input_tokens: tokens_in,
            output_tokens: tokens_out,
        });
    }
    Ok(records)
}

/// Resolves `model` for a `boi-phase-runs` record from
/// `<recipes_dir>/recipe-<id>.yaml`'s `settings.goose_model`. Returns `None`
/// when the recipe file is missing, unreadable, or lacks that key — a
/// missing recipe is expected (e.g. deleted after the run) and must not fail
/// the whole source.
#[allow(dead_code)]
fn recipe_model_for(recipes_dir: &Path, id: &str) -> Option<String> {
    let path = recipes_dir.join(format!("recipe-{id}.yaml"));
    let content = std::fs::read_to_string(path).ok()?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&content).ok()?;
    doc.get("settings")?
        .get("goose_model")?
        .as_str()
        .map(|s| s.to_string())
}

/// Default **boi-phase-runs** source path: `$HOME/.boi/v2/boi.db`, per spec
/// ("boi-phase-runs: source $HOME/.boi/v2/boi.db (read-only, path
/// overridable)"). Overridable at the call site (e.g. a future
/// `--boi-db` flag), never hardcoded past this one function.
#[allow(dead_code)]
fn default_boi_db() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    Path::new(&home).join(".boi/v2/boi.db")
}

/// Default recipe directory for the **boi-phase-runs** model lookup:
/// `$HOME/.boi/v2/recipes` (`recipe_model_for` joins `recipe-<id>.yaml`).
#[allow(dead_code)]
fn default_boi_recipes_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    Path::new(&home).join(".boi/v2/recipes")
}

/// The `--source-kind` value for this source (task Tsgmstjxk). The CLI enum
/// and "run all kinds" dispatch land in sibling task Txv7phcj8; this
/// constant is the shared literal both sides key off.
#[allow(dead_code)]
const BOI_PHASE_RUNS_SOURCE_KIND: &str = "boi-phase-runs";

/// Renders one `BoiPhaseRunRecord` as a line in the ledger's existing
/// provider-agnostic ingestion format (`usage_ledger::parse_event`'s flat
/// `"type":"token_usage_record"` branch, used whenever `payload` is absent).
/// This is the bridge from a SQLite row to the ledger's JSONL import path —
/// no new ledger insert API, no schema migration. `account_scope` is always
/// `"boi"` per the attribution mapping; `provider` carries the worker's own
/// value (e.g. `claude_code`, `codex`) through unchanged.
#[allow(dead_code)]
fn boi_phase_run_ledger_line(record: &BoiPhaseRunRecord) -> String {
    json!({
        "type": "token_usage_record",
        "provider": record.provider,
        "account_scope": "boi",
        "response_id": record.response_id,
        "root_task_family": record.root_task_family,
        "event_at": record.event_at,
        "model": record.model,
        "input_tokens": record.input_tokens,
        "output_tokens": record.output_tokens,
    })
    .to_string()
}

/// Collects the **boi-phase-runs** source end to end: reads eligible rows
/// from `boi_db` (read-only), resolves each row's model from `recipes_dir`,
/// stages them at `staging_path` in the ledger's generic JSONL format, and
/// imports that staging file into `ledger` — reusing the existing
/// requestId-equivalent (provider+scope+response_id) dedupe so re-collection
/// is idempotent. `staging_path` is rewritten on every call; re-imports of
/// already-accepted rows land as `duplicate`, not a second insert, because
/// `import_parsed` dedupes on the canonical (provider, account_scope,
/// response_id) key regardless of which generation of the staging file
/// carried them.
#[allow(dead_code)]
fn collect_boi_phase_runs(
    boi_db: &Path,
    recipes_dir: &Path,
    staging_path: &Path,
    ledger: &mut UsageLedger,
    max_records: usize,
) -> std::result::Result<ImportResult, String> {
    let records = discover_boi_phase_runs(boi_db, recipes_dir)?;
    let mut body = String::new();
    for record in &records {
        body.push_str(&boi_phase_run_ledger_line(record));
        body.push('\n');
    }
    if let Some(parent) = staging_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("boi_staging_dir={e}"))?;
    }
    std::fs::write(staging_path, body).map_err(|e| format!("boi_staging_write={e}"))?;
    ledger
        .import_jsonl(staging_path, ImportOptions { max_records, abort_before_commit: false })
        .map_err(|e| format!("boi_ledger_import={e:?}"))
}


/// Dispatch `hex usage collect` across one or every source kind. `--source
/// <path>` and `--codex-root <dir>` are codex-jsonl-specific overrides — their
/// presence (without an explicit `--claude-root`) narrows a bare invocation to
/// codex-jsonl only, so existing codex-only callers and tests keep their exact
/// prior behavior. A truly bare `hex usage collect` (no override flags at all)
/// runs every known kind in sequence, per decision
/// usage-ledger-provider-agnostic-2026-09-14: no LLM path should silently sit
/// outside the ledger. A failure in one kind is reported and reflected in the
/// exit code, but never skips the remaining kinds.
fn collect(
    source_kind: Option<String>,
    source: Option<PathBuf>,
    codex_root: Option<PathBuf>,
    claude_root: Option<PathBuf>,
    ledger: Option<PathBuf>,
    max_records: usize,
) -> i32 {
    let codex_explicit = source.is_some() || codex_root.is_some();
    let claude_explicit = claude_root.is_some();
    let kinds: Vec<&str> = match source_kind.as_deref() {
        Some("codex-jsonl") => vec!["codex-jsonl"],
        Some("claude-transcripts") => vec!["claude-transcripts"],
        Some(other) => {
            eprintln!("usage collect: unknown --source-kind {other}");
            return 1;
        }
        None if codex_explicit && !claude_explicit => vec!["codex-jsonl"],
        None if claude_explicit && !codex_explicit => vec!["claude-transcripts"],
        None => vec!["codex-jsonl", "claude-transcripts"],
    };
    let mut exit = 0;
    for kind in kinds {
        let code = match kind {
            "codex-jsonl" => {
                collect_codex_jsonl(source.clone(), codex_root.clone(), ledger.clone(), max_records)
            }
            "claude-transcripts" => {
                collect_claude_transcripts(claude_root.clone(), ledger.clone(), max_records)
            }
            _ => unreachable!("kinds is built from a closed set above"),
        };
        if code != 0 {
            exit = code;
        }
    }
    exit
}

/// `claude-transcripts` CLI wrapper: resolves defaults, opens the ledger, and
/// reports the result the same way `collect_codex_jsonl` does (stdout summary
/// line + `usage-tracking`/`collect` telemetry health event) so a failure here
/// is exactly as loud as a Codex collection failure (S6).
fn collect_claude_transcripts(
    claude_root: Option<PathBuf>,
    ledger: Option<PathBuf>,
    max_records: usize,
) -> i32 {
    let root = claude_root.unwrap_or_else(default_projects_dir);
    let ledger_path = ledger.unwrap_or_else(default_ledger);
    if let Some(parent) = ledger_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("usage collect: cannot create ledger directory: {e}");
            return 1;
        }
    }
    let mut opened = match UsageLedger::open(&ledger_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("usage collect: failed: {e:?}");
            health("error", "kind=claude-transcripts ledger_open_failed".into());
            return 1;
        }
    };
    let result = import_claude_transcripts(&root, &mut opened, max_records);
    if let Some(err) = &result.error {
        eprintln!("usage collect: claude-transcripts import failed: {err}");
        health("error", format!("kind=claude-transcripts import_failed={err}"));
        return 1;
    }
    println!(
        "usage collect: source=claude-transcripts accepted={} duplicates={} conflicts={} truncated={}",
        result.accepted, result.duplicates, result.conflicts, result.truncated
    );
    health(
        "ok",
        format!(
            "kind=claude-transcripts accepted={} duplicates={} conflicts={} truncated={}",
            result.accepted, result.duplicates, result.conflicts, result.truncated
        ),
    );
    0
}

fn collect_codex_jsonl(
    source: Option<PathBuf>,
    codex_root: Option<PathBuf>,
    ledger: Option<PathBuf>,
    max_records: usize,
) -> i32 {
    let discovered_sources = source.is_none();
    let (sources, mut issues) = match source {
        Some(path) => (vec![path], Vec::new()),
        None => {
            let root = codex_root.unwrap_or_else(|| {
                PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".codex")
            });
            discover(&root)
        }
    };
    let ledger = ledger.unwrap_or_else(default_ledger);
    if max_records == 0 {
        eprintln!("usage collect: no local sources discovered");
        health("error", "no_sources".into());
        return 1;
    }
    // A configured local Codex root can legitimately have no live sessions,
    // including when every state-db rollout pointer has gone stale. A periodic
    // collection then completes as a visible no-op, not an unhealthy worker.
    if discovered_sources && sources.is_empty() {
        println!("usage collect: accepted=0 backlog=false issues=0");
        health("ok", "backlog=false accepted=0 issues=".into());
        return 0;
    }
    if sources.is_empty() {
        eprintln!("usage collect: no local sources discovered");
        health("error", "no_sources".into());
        return 1;
    }
    if let Some(parent) = ledger.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("usage collect: cannot create ledger directory: {e}");
            return 1;
        }
    }
    match UsageLedger::open(&ledger) {
        Ok(mut l) => {
            let mut remaining = max_records;
            let mut accepted = 0;
            let mut backlog = false;
            for path in sources {
                if remaining == 0 {
                    backlog = true;
                    break;
                }
                if !path.is_file() {
                    if discovered_sources {
                        // A session can disappear after discovery but before this
                        // bounded import. Treat that as the same stale-pointer
                        // warning rather than poisoning the worker health state.
                        eprintln!("usage collect: skipped vanished source={}", path.display());
                    } else {
                        issues.push(format!("unreadable={}", path.display()));
                    }
                    continue;
                }
                match l.import_jsonl(
                    &path,
                    ImportOptions {
                        max_records: remaining,
                        abort_before_commit: false,
                    },
                ) {
                    Ok(r) => {
                        accepted += r.accepted;
                        backlog |= r.backlog;
                        remaining = remaining.saturating_sub(
                            (r.accepted + r.duplicates + r.conflicts + r.quarantined) as usize,
                        )
                    }
                    Err(e) => issues.push(format!("import_failed={}: {e:?}", path.display())),
                }
            }
            println!(
                "usage collect: accepted={accepted} backlog={backlog} issues={}",
                issues.len()
            );
            health(
                if issues.is_empty() { "ok" } else { "error" },
                format!(
                    "backlog={} accepted={} issues={}",
                    backlog,
                    accepted,
                    issues.join(";")
                ),
            );
            if issues.is_empty() {
                0
            } else {
                1
            }
        }
        Err(e) => {
            eprintln!("usage collect: failed: {e:?}");
            health("error", "ledger_open_failed".into());
            1
        }
    }
}

fn report(
    ledger: Option<PathBuf>,
    output: Option<PathBuf>,
    cutoff: Option<String>,
    detail_dimension: Option<String>,
    detail_key: Option<String>,
    detail_offset: usize,
    detail_limit: usize,
) -> i32 {
    let ledger = ledger.unwrap_or_else(default_ledger);
    let output = output.unwrap_or_else(default_report);
    let cutoff = match cutoff {
        Some(value) => match value.parse::<DateTime<Utc>>() {
            Ok(time) => time,
            Err(_) => {
                eprintln!("usage report: cutoff must be RFC3339 UTC");
                return 1;
            }
        },
        None => Utc::now(),
    };
    let end = Utc.from_utc_datetime(
        &cutoff.date_naive().and_hms_opt(0, 0, 0).expect("midnight is valid"),
    );
    let start = end - Duration::days(1);
    let preceding_start = start - Duration::days(1);
    if detail_limit > 1_000 {
        eprintln!("usage report: detail limit must not exceed 1000");
        return 1;
    }
    let detail_dimension = match detail_dimension.as_deref() {
        None => None,
        Some("model") => Some(hex::usage_ledger::ContributorDimension::Model),
        Some("family") => Some(hex::usage_ledger::ContributorDimension::Family),
        Some("child") => Some(hex::usage_ledger::ContributorDimension::Child),
        Some(_) => {
            eprintln!("usage report: detail dimension must be model, family, or child");
            return 1;
        }
    };
    let result = UsageLedger::open(&ledger).and_then(|mut ledger| {
        let coverage = ledger.coverage()?;
        let read = ledger.frozen_read([
            hex::usage_ledger::HalfOpenUtcWindow { start, end },
            hex::usage_ledger::HalfOpenUtcWindow { start: preceding_start, end: start },
        ])?;
        let mut accumulator = usage_reporting::ReportAccumulator::new(coverage, start, end, false);
        accumulator.extend_summary_groups(&read.summary_groups(hex::usage_ledger::FrozenWindow::First)?, false);
        accumulator.extend_summary_groups(&read.summary_groups(hex::usage_ledger::FrozenWindow::Second)?, true);
        let detail = match detail_dimension {
            None => None,
            Some(dimension) => {
                let key = match (dimension, detail_key.as_deref()) {
                    (hex::usage_ledger::ContributorDimension::Child, None) => None,
                    (_, Some(key)) => Some(key),
                    _ => return Err(hex::usage_ledger::LedgerError::InvalidWindow),
                };
                let mut after = None;
                let mut skipped = 0usize;
                let mut response_ids = Vec::new();
                let (total_matches, has_more) = loop {
                    let page = read.contributor_detail_page(hex::usage_ledger::FrozenWindow::First, dimension, key, after.as_ref(), 1_000)?;
                    let mut page_has_unreturned_rows = false;
                    for row in &page.rows {
                        if skipped < detail_offset {
                            skipped += 1;
                        } else if response_ids.len() < detail_limit {
                            response_ids.push(row.response_id.clone());
                        } else {
                            page_has_unreturned_rows = true;
                            break;
                        }
                    }
                    if response_ids.len() == detail_limit {
                        break (page.total_matches, page_has_unreturned_rows || page.next_cursor().is_some());
                    }
                    let Some(next) = page.next_cursor() else { break (page.total_matches, false) };
                    after = Some(next);
                };
                Some(json!({"dimension": match dimension { hex::usage_ledger::ContributorDimension::Model => "model", hex::usage_ledger::ContributorDimension::Family => "family", hex::usage_ledger::ContributorDimension::Child => "child" }, "key": key.unwrap_or("child_responses"), "total_matches": total_matches, "offset": detail_offset, "response_ids": response_ids, "has_more": has_more}))
            }
        };
        Ok((accumulator.finish(), detail))
    });
    let Ok((mut report, detail)) = result else {
        eprintln!("usage report: ledger unavailable: {}", ledger.display());
        return 1;
    };
    if collector_is_stale() {
        report.coverage.stale_sources = report.coverage.stale_sources.saturating_add(1);
        report.incomplete_labels.insert("collector_failure".into());
    }
    let contributor = |item: &hex::usage_reporting::Contributor| json!({"key":item.key,"tokens":item.measured.total().to_string(),"credits_micro":item.credits.value.0.to_string()});
    let optional_total = |value: i128, missing: &str| (!report.incomplete_labels.contains(missing)).then(|| value.to_string());
    let measured = |measured: &hex::usage_reporting::MeasuredTokens| json!({"responses":measured.responses,"input_tokens":measured.input.to_string(),"cached_input_tokens":measured.cached_input.to_string(),"output_tokens":measured.output.to_string(),"cache_write_input_tokens":optional_total(measured.cache_write_input,"missing_cache_write_input_tokens"),"reasoning_output_tokens":optional_total(measured.reasoning_output,"missing_reasoning_output_tokens"),"provider_total_tokens":measured.provider_total.map(|value|value.to_string())});
    let unknown_fields = report.incomplete_labels.iter().filter(|label| label.starts_with("unknown_")).cloned().collect::<Vec<_>>();
    let body=json!({"schema":"hex.usage-report.v2","cutoff":cutoff.to_rfc3339(),"windows":{"current":{"start":report.start.to_rfc3339(),"end":report.end.to_rfc3339(),"measured":measured(&report.measured)},"preceding":{"start":preceding_start.to_rfc3339(),"end":start.to_rfc3339(),"measured":measured(&report.preceding_measured)},"change_tokens":report.preceding_change_tokens.map(|value|value.to_string())},"modeled_credits":{"unit":"credit_equivalent","rate_version":report.modeled_credits.rate_version,"rate_source":report.modeled_credits.rate_source,"total_micro":report.modeled_credits.value.0.to_string(),"fresh_micro":report.modeled_credit_components.fresh.0.to_string(),"cached_micro":report.modeled_credit_components.cached.0.to_string(),"output_micro":report.modeled_credit_components.output.0.to_string(),"incomplete":report.modeled_credits.incomplete,"labels":report.modeled_credits.labels},"actual_billed_debited":{"value":serde_json::Value::Null,"unit":"api_usd","labels":report.actual_billed_debited.labels},"coverage":{"accepted":report.coverage.accepted,"noncanonical":report.coverage.noncanonical,"duplicates":report.coverage.duplicates,"conflicts":report.coverage.conflicts,"quarantined":report.coverage.quarantined,"source_backlog":report.coverage.pending_sources,"stale_sources":report.coverage.stale_sources,"unknown_fields":unknown_fields},"completeness":if report.incomplete_labels.is_empty(){"complete"}else{"incomplete"},"incomplete":report.incomplete_labels,"by_model":report.by_model.iter().map(contributor).collect::<Vec<_>>(),"by_family":report.by_family.iter().map(contributor).collect::<Vec<_>>(),"child_coordination":{"share_millionths":report.child_coordination_share_millionths},"contributor_detail":detail}).to_string()+"\n";
    if let Some(parent) = output.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("usage report: cannot create output directory: {e}");
            return 1;
        }
    }
    let temp = output.with_extension("tmp");
    match std::fs::write(&temp, body).and_then(|_| std::fs::rename(&temp, &output)) {
        Ok(()) => {
            println!("usage report: {}", output.display());
            0
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            eprintln!("usage report: write failed: {e}");
            1
        }
    }
}

/// Default events.db path for the harness-llm-cost source:
/// `$HEX_DIR/.hex/telemetry/events.db` (see `telemetry.rs`'s own store path).
/// Not yet wired to a CLI flag — that lands with the `--source-kind` plumbing
/// in task Txv7phcj8 ("bare `hex usage collect` runs all kinds"), which will
/// call `import_harness_llm_cost(&default_llm_cost_events_db(), ...)` when no
/// explicit path is given.
#[allow(dead_code)]
fn default_llm_cost_events_db() -> PathBuf {
    PathBuf::from(std::env::var("HEX_DIR").unwrap_or_else(|_| ".".into()))
        .join(".hex")
        .join("telemetry")
        .join("events.db")
}

/// harness-llm-cost source (task Th19d8qvp): reads `source = "llm-cost"` rows
/// out of a harness telemetry `events.db` (default `$HEX_DIR/.hex/telemetry/events.db`,
/// see `llm_cost.rs::record_llm_cost` for the row shape this reads) and imports
/// them into the durable usage ledger.
///
/// `event` is `"<transport>::<use_case>"` — `provider` is the transport prefix
/// (openrouter, claude-cli, …), `account_scope` is always `"harness"`, and
/// `root_task_family` is the use case after `::`. `response_id` is the
/// `events.id` row id. Tokens (`in_tokens`/`out_tokens`, `out_tokens` may
/// legitimately be `0` per the caveat documented in `llm_cost.rs`) and `model`
/// come from the `detail` JSON blob.
///
/// The events db is opened strictly read-only (`SQLITE_OPEN_READ_ONLY`) — this
/// collector must never take a write lock on the live telemetry store.
///
/// Two things make this safe to call every 5 minutes (hex-usage-tracking
/// worker cadence) forever:
///
/// 1. **High-water mark.** The `SELECT` below is `id > <cursor>`, not
///    `ORDER BY id LIMIT ?` from the start. The cursor is a plain-text
///    sibling file (`harness-llm-cost.cursor`, next to the staging JSONL —
///    never fed into `import_jsonl`) holding the highest `events.id` this
///    collector has ever looked at, updated after every batch that sees at
///    least one row. Without this, once the live events.db held more than
///    `max_records` llm-cost rows, rows past the first `max_records` could
///    never be reached — a silent `accepted=0`/`backlog=false` forever.
/// 2. **Stable staging file.** Each matching row is re-expressed as one line
///    of the ledger's existing generic canonical-event JSON schema (the
///    `token_usage_record` fallback branch in `usage_ledger::parse_event`,
///    keyed by `provider`+`account_scope`+`response_id`) and *appended* to
///    one stable file living next to the ledger's own database
///    (`UsageLedger::db_path`'s directory), never a fresh tempfile per call.
///    A fresh tempfile gives `import_jsonl` a brand-new file identity every
///    run, so it inserts a new `source_files` row and re-observes every
///    already-canonical line as a fresh `'duplicate'` — `UsageLedger::coverage()`
///    then shows `duplicates` growing by N on every worker fire forever.
///    Appending to one stable path lets the ledger's own byte cursor for
///    that file identity do the dedupe instead: a re-collect that appends
///    nothing new never calls `import_jsonl` at all, so it is a true no-op.
///
/// Not yet called from `main`/the CLI dispatch — that wiring lands with the
/// `--source-kind` plumbing in task Txv7phcj8 (bare `hex usage collect` runs
/// all kinds), which will call this directly. Until then it's only exercised
/// by its own fixture test below.
#[allow(dead_code)]
pub fn import_harness_llm_cost(
    events_db: &Path,
    ledger: &mut UsageLedger,
    max_records: usize,
) -> hex::usage_ledger::Result<hex::usage_ledger::ImportResult> {
    use std::io::Write as _;
    let conn = rusqlite::Connection::open_with_flags(
        events_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    let staging_path = ledger
        .db_path()?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("harness-llm-cost.jsonl");
    let cursor_path = llm_cost_cursor_path(&staging_path);
    let high_water = read_llm_cost_cursor(&cursor_path)?;

    let mut stmt = conn.prepare(
        "SELECT id, ts, event, detail FROM events WHERE source = 'llm-cost' AND id > ?1 ORDER BY id LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![high_water, max_records as i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    let mut lines: Vec<String> = Vec::new();
    let mut fetched = 0usize;
    let mut last_id = high_water;
    for row in rows {
        let (id, ts, event, detail) = row?;
        fetched += 1;
        // Advance the cursor past every row we looked at, including ones
        // skipped below for having the wrong `event` shape — otherwise a
        // single malformed row at the tail of a batch would be re-fetched
        // (harmlessly, but pointlessly) on every subsequent call forever.
        last_id = id;
        let Some((provider, use_case)) = event.split_once("::") else {
            // Not the "<transport>::<use_case>" shape record_llm_cost writes —
            // skip rather than fabricate a provider.
            continue;
        };
        let detail_value: serde_json::Value = detail
            .as_deref()
            .and_then(|d| serde_json::from_str(d).ok())
            .unwrap_or(serde_json::Value::Null);
        let model = detail_value.get("model").and_then(serde_json::Value::as_str);
        let input_tokens = detail_value.get("in_tokens").and_then(serde_json::Value::as_i64);
        let output_tokens = detail_value.get("out_tokens").and_then(serde_json::Value::as_i64);
        // "type":"token_usage_record" with no "payload" object routes this
        // through parse_event's flat, provider-agnostic fallback schema
        // rather than the Codex-specific `payload.response_id` branch.
        let line = json!({
            "type": "token_usage_record",
            "provider": provider,
            "account_scope": "harness",
            "response_id": id.to_string(),
            "root_task_family": use_case,
            "event_at": ts,
            "model": model,
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        });
        lines.push(line.to_string());
    }
    // The SQL LIMIT capped us at max_records rows fetched — if we hit that
    // cap there may be more rows beyond this batch still to collect.
    let sql_backlog = fetched >= max_records;

    if fetched > 0 {
        std::fs::write(&cursor_path, last_id.to_string())?;
    }

    if lines.is_empty() {
        return Ok(hex::usage_ledger::ImportResult {
            backlog: sql_backlog,
            ..Default::default()
        });
    }

    {
        let mut staging = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&staging_path)?;
        for line in &lines {
            writeln!(staging, "{line}")?;
        }
        staging.flush()?;
    }
    let mut result = ledger.import_jsonl(
        &staging_path,
        ImportOptions {
            max_records: max_records.max(lines.len()),
            abort_before_commit: false,
        },
    )?;
    result.backlog = result.backlog || sql_backlog;
    Ok(result)
}

/// Sibling cursor-marker path for the harness-llm-cost stable staging file —
/// see `import_harness_llm_cost`'s doc comment. Plain integer text, never fed
/// into `UsageLedger::import_jsonl`.
fn llm_cost_cursor_path(staging_path: &Path) -> PathBuf {
    staging_path.with_extension("cursor")
}

/// Read the harness-llm-cost high-water mark; `0` (import everything) if the
/// cursor file has never been written yet.
fn read_llm_cost_cursor(path: &Path) -> std::io::Result<i64> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s.trim().parse().unwrap_or(0)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// One assistant turn = `usd` dollars of pure output tokens at Fable rates
    /// ($50/MTok output → tokens = usd / 50 * 1e6), timestamped `mins_ago`
    /// minutes before `now()`. Relative timestamps keep the production
    /// invariant (file mtime >= contained turn timestamps) true in fixtures.
    fn turn(req: &str, mins_ago: i64, model: &str, usd: f64) -> String {
        let out_tok = (usd / 50.0 * 1e6) as u64;
        let ts = (now() - Duration::minutes(mins_ago)).to_rfc3339();
        format!(
            r#"{{"type":"assistant","requestId":"{req}","timestamp":"{ts}","message":{{"model":"{model}","usage":{{"input_tokens":0,"output_tokens":{out_tok},"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}"#
        )
    }

    fn write_jsonl(path: &Path, lines: &[String]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    /// Claude Code transcript line, field-for-field matching real
    /// `~/.claude/projects/<proj>/<session>.jsonl` records (confirmed by
    /// reading a live transcript during orientation, not a test read):
    /// top-level `requestId`/`type`/`sessionId`/`timestamp`, and
    /// `message.model`/`message.usage.{input_tokens,cache_creation_input_tokens,
    /// cache_read_input_tokens,output_tokens}`.
    fn claude_turn(
        request_id: &str,
        session_id: &str,
        mins_ago: i64,
        model: &str,
        input_tokens: u64,
        cache_creation_input_tokens: u64,
        cache_read_input_tokens: u64,
        output_tokens: u64,
    ) -> String {
        let ts = (now() - Duration::minutes(mins_ago)).to_rfc3339();
        format!(
            r#"{{"type":"assistant","requestId":"{request_id}","timestamp":"{ts}","sessionId":"{session_id}","message":{{"model":"{model}","usage":{{"input_tokens":{input_tokens},"cache_creation_input_tokens":{cache_creation_input_tokens},"cache_read_input_tokens":{cache_read_input_tokens},"output_tokens":{output_tokens}}}}}}}"#
        )
    }

    /// Task Txv7phcj8 (claude-transcripts source kind): a recursive scan of a
    /// fixture `~/.claude/projects`-shaped tree must import one canonical
    /// `claude-code`/`local-claude-code` ledger row per distinct `requestId`
    /// — including the nested `<session>/subagents/agent-*.jsonl` shape that
    /// the 2026-06-12 burn-guard root-cause proved load-bearing — dedupe the
    /// same requestId appearing in two files, attribute `root_task_family` to
    /// the session id, and map Claude's `cache_read_input_tokens` (a SIBLING
    /// of `input_tokens`) into the ledger's `cached_input_tokens` (a SUBSET of
    /// `input_tokens`, enforced by `summary_groups`'s validity predicate) by
    /// folding cache reads into the ledger's `input_tokens` total. Re-import
    /// must be idempotent (no duplicate canonical rows).
    ///
    /// `import_claude_transcripts` is fully implemented (see its doc comment
    /// above its definition): this test is GREEN, exercising the recursive
    /// scan, nested-subagent discovery, requestId dedupe, and idempotent
    /// re-import against a fixture tree — never the live `~/.claude/projects`.
    #[test]
    fn import_claude_transcripts_collects_assistant_usage_with_dedupe() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_root = tmp.path().join("claude-projects");
        let session = "sess-uuid-1";

        // Main session transcript: one turn (req-1).
        write_jsonl(
            &claude_root.join("-proj/").join(format!("{session}.jsonl")),
            &[claude_turn(
                "req-1", session, 10, "claude-sonnet-4-6", 100, 7_000, 18_000, 55,
            )],
        );
        // Nested subagent transcript for the SAME session (req-2). Subagent
        // transcripts live under `<session>/subagents/agent-*.jsonl` — a
        // one-level scan would miss this, same failure mode as burn's.
        write_jsonl(
            &claude_root
                .join("-proj")
                .join(session)
                .join("subagents")
                .join("agent-abc.jsonl"),
            &[claude_turn(
                "req-2", session, 5, "claude-haiku-4-5", 20, 100, 200, 10,
            )],
        );
        // req-1 repeated in a different file — must dedupe to one row.
        write_jsonl(
            &claude_root.join("-proj/other.jsonl"),
            &[claude_turn(
                "req-1", session, 10, "claude-sonnet-4-6", 100, 7_000, 18_000, 55,
            )],
        );

        let mut ledger = UsageLedger::open(tmp.path().join("usage.db")).unwrap();

        let result = import_claude_transcripts(&claude_root, &mut ledger, 1_000);
        assert_eq!(
            result.accepted, 2,
            "one accepted row per distinct requestId across main + nested subagent transcripts, req-1 deduped"
        );

        let rows = ledger.rows(100, 0).unwrap();
        let claude_rows: Vec<_> = rows.iter().filter(|r| r.provider == "claude-code").collect();
        assert_eq!(
            claude_rows.len(),
            2,
            "expected 2 distinct claude-code ledger rows, got {claude_rows:?}"
        );

        let req1 = claude_rows
            .iter()
            .find(|r| r.response_id == "req-1")
            .expect("req-1 present as a canonical row");
        assert_eq!(req1.account_scope, "local-claude-code");
        assert_eq!(
            req1.root_task_family.as_deref(),
            Some(session),
            "root_task_family must be the Claude Code session id"
        );
        assert_eq!(req1.model.as_deref(), Some("claude-sonnet-4-6"));
        // input_tokens(ledger) = raw input + cache_read (cache_read is a
        // SIBLING in Claude's schema, but must become a SUBSET for the
        // ledger's summary_groups validity predicate below).
        assert_eq!(req1.input_tokens, Some(100 + 18_000));
        assert_eq!(req1.cached_input_tokens, Some(18_000));
        assert_eq!(req1.cache_write_input_tokens, Some(7_000));
        assert_eq!(req1.output_tokens, Some(55));
        assert!(
            req1.input_tokens.unwrap() >= req1.cached_input_tokens.unwrap(),
            "must satisfy summary_groups's input_tokens>=cached_input_tokens validity predicate"
        );

        // Re-import must be idempotent: no duplicate canonical rows.
        let result2 = import_claude_transcripts(&claude_root, &mut ledger, 1_000);
        assert_eq!(
            result2.accepted, 0,
            "re-import of unchanged transcripts must not add new canonical rows"
        );
        let rows_after = ledger.rows(100, 0).unwrap();
        assert_eq!(
            rows_after.iter().filter(|r| r.provider == "claude-code").count(),
            2,
            "re-import must not duplicate canonical claude-code rows"
        );
    }

    /// Synthetic spike fixture: $120 of Fable output inside the window →
    /// the rate computation MUST cross the $100/hr threshold (red on a
    /// guardrail that undercounts; this is the ISSUE.md regression gate).
    #[test]
    fn synthetic_spike_crosses_threshold() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("-proj/session.jsonl"),
            &[
                turn("r1", 30, "claude-fable-5", 60.0),
                turn("r2", 15, "claude-fable-5", 60.0),
            ],
        );
        let s = window_spend(tmp.path(), now(), Duration::minutes(60));
        assert_eq!(s.turns, 2);
        assert!((s.usd - 120.0).abs() < 0.01, "got ${}", s.usd);
        let rate = s.usd; // 60-min window → rate == usd
        assert!(rate > 100.0, "spike must cross the $100/hr threshold");
    }

    /// Subagent transcripts are NESTED (`<session>/subagents/agent-*.jsonl`).
    /// The 2026-06-12 root-cause found a one-level scan missing 1,784 such
    /// files (40% of the worst day's spend). Recursive scan is load-bearing.
    #[test]
    fn nested_subagent_files_are_counted() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("-proj/sess.jsonl"),
            &[turn("main1", 10, "claude-fable-5", 10.0)],
        );
        write_jsonl(
            &tmp.path().join("-proj/sess-uuid/subagents/agent-abc.jsonl"),
            &[turn("sub1", 5, "claude-fable-5", 30.0)],
        );
        let s = window_spend(tmp.path(), now(), Duration::minutes(60));
        assert_eq!(s.turns, 2, "must include the nested subagent turn");
        assert!((s.usd - 40.0).abs() < 0.01, "got ${}", s.usd);
    }

    /// Dedupe by requestId — Claude Code copies transcripts on resume, so the
    /// same request appears in multiple files (naive summation ≈ 2x).
    #[test]
    fn duplicate_request_ids_counted_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let t = turn("same-req", 20, "claude-fable-5", 25.0);
        write_jsonl(&tmp.path().join("-proj/a.jsonl"), std::slice::from_ref(&t));
        write_jsonl(&tmp.path().join("-proj/b.jsonl"), &[t]);
        let s = window_spend(tmp.path(), now(), Duration::minutes(60));
        assert_eq!(s.turns, 1);
        assert!((s.usd - 25.0).abs() < 0.01);
    }

    /// Turns outside the trailing window don't count.
    #[test]
    fn old_turns_excluded() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("-proj/s.jsonl"),
            &[
                turn("old", 90, "claude-fable-5", 500.0),
                turn("new", 30, "claude-fable-5", 5.0),
            ],
        );
        let s = window_spend(tmp.path(), now(), Duration::minutes(60));
        assert_eq!(s.turns, 1);
        assert!((s.usd - 5.0).abs() < 0.01, "got ${}", s.usd);
    }

    /// Quiet hour → rate stays under threshold (green side of the gate).
    #[test]
    fn quiet_window_stays_under_threshold() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("-proj/s.jsonl"),
            &[turn("r1", 30, "claude-sonnet-4-6", 12.0)],
        );
        let s = window_spend(tmp.path(), now(), Duration::minutes(60));
        assert!(s.usd < 100.0);
    }

    /// Call-site mapping (task Tbnve3dk9): the burn guardrail's spend-threshold
    /// breach must select the `Spend` class (→ email + urgent push), while the
    /// misconfiguration alert — like every other historical notify — stays
    /// `Default` (push only). Non-vacuous: pins BOTH halves of "map the spend
    /// site, leave all others default". The class→rails delivery is proven in
    /// `alert.rs::email_classes_send_both_rails`; this proves the SELECTION.
    #[test]
    fn burn_alert_class_maps_spend_and_leaves_config_default() {
        use crate::alert::AlertClass;
        assert_eq!(
            burn_alert_class("burn-guard"),
            AlertClass::Spend,
            "the spend-rate breach must ride the Spend rail"
        );
        assert_eq!(
            burn_alert_class("burn-guard-config"),
            AlertClass::Default,
            "the misconfig alert stays Default (all other notify calls unchanged)"
        );
    }

    /// harness-llm-cost (task Th19d8qvp): the default events.db path always
    /// resolves under `.hex/telemetry/events.db`, whatever `$HEX_DIR` is set
    /// to in this process — checked by suffix only (no env mutation; other
    /// tests in this binary run concurrently and share the process env).
    #[test]
    fn default_llm_cost_events_db_targets_hex_telemetry_store() {
        let path = default_llm_cost_events_db();
        assert!(
            path.ends_with(".hex/telemetry/events.db"),
            "got {}",
            path.display()
        );
    }

    #[test]
    fn failed_collector_makes_report_stale_until_success() {
        assert!(!collector_status_is_stale(None));
        assert!(collector_status_is_stale(Some("error")));
        assert!(collector_status_is_stale(Some("alert")));
        assert!(!collector_status_is_stale(Some("ok")));
    }

    /// RED (task Tsgmstjxk): the **boi-phase-runs** source must read
    /// `phase_runs` from a fixture boi.db, excluding the `deterministic`
    /// worker and the zero-token row, and must resolve `model` from the
    /// matching `recipe-<id>.yaml`'s `settings.goose_model`. Currently fails
    /// because `discover_boi_phase_runs` is an unimplemented stub — this
    /// pins the required filtering + attribution behavior for execute().
    #[test]
    fn boi_phase_runs_filters_and_resolves_model_from_recipe() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("boi.db");
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE phase_runs (id TEXT PRIMARY KEY, spec_id TEXT, provider TEXT, tokens_in INTEGER, tokens_out INTEGER, started_at TEXT);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO phase_runs (id, spec_id, provider, tokens_in, tokens_out, started_at) VALUES ('run1','specA','claude_code',1200,340,'2026-09-14T10:00:00Z')",
                [],
            )
            .unwrap();
            // Deterministic worker: excluded regardless of tokens (spec: "a
            // worker provider (not \"deterministic\")").
            conn.execute(
                "INSERT INTO phase_runs (id, spec_id, provider, tokens_in, tokens_out, started_at) VALUES ('run2','specA','deterministic',500,500,'2026-09-14T10:05:00Z')",
                [],
            )
            .unwrap();
            // Zero-token worker row: excluded (spec: "tokens_in or tokens_out
            // > 0").
            conn.execute(
                "INSERT INTO phase_runs (id, spec_id, provider, tokens_in, tokens_out, started_at) VALUES ('run3','specA','codex',0,0,'2026-09-14T10:10:00Z')",
                [],
            )
            .unwrap();
        }
        let recipes_dir = tmp.path().join("recipes");
        std::fs::create_dir_all(&recipes_dir).unwrap();
        std::fs::write(
            recipes_dir.join("recipe-run1.yaml"),
            "settings:\n  goose_provider: claude-code\n  goose_model: claude-opus-4-8\n",
        )
        .unwrap();
        // run3 has no recipe file — model must come back None, not an error.

        let records = discover_boi_phase_runs(&db_path, &recipes_dir)
            .expect("boi-phase-runs discovery should succeed against a read-only fixture db");

        assert_eq!(
            records.len(),
            1,
            "only the non-deterministic, token-bearing row is eligible: {records:?}"
        );
        let r = &records[0];
        assert_eq!(r.response_id, "run1");
        assert_eq!(r.provider, "claude_code");
        assert_eq!(r.root_task_family.as_deref(), Some("specA"));
        assert_eq!(r.event_at.as_deref(), Some("2026-09-14T10:00:00Z"));
        assert_eq!(
            r.model.as_deref(),
            Some("claude-opus-4-8"),
            "model must resolve from recipe-run1.yaml settings.goose_model"
        );
        assert_eq!(r.input_tokens, Some(1200));
        assert_eq!(r.output_tokens, Some(340));
    }

    /// End-to-end (task Tsgmstjxk): `collect_boi_phase_runs` must actually
    /// land the eligible fixture row in a real ledger — proving the source
    /// kind is wired to the ledger's existing generic JSONL import path
    /// (`usage_ledger::parse_event`'s flat `token_usage_record` branch), not
    /// just discoverable-but-unused. Uses only temp-dir fixtures per the
    /// spec's hard constraint (never the live boi.db, recipes dir, or
    /// ledger).
    #[test]
    fn collect_boi_phase_runs_writes_eligible_rows_into_the_ledger() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("boi.db");
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE phase_runs (id TEXT PRIMARY KEY, spec_id TEXT, provider TEXT, tokens_in INTEGER, tokens_out INTEGER, started_at TEXT);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO phase_runs (id, spec_id, provider, tokens_in, tokens_out, started_at) VALUES ('run1','specA','claude_code',1200,340,'2026-09-14T10:00:00Z')",
                [],
            )
            .unwrap();
        }
        let recipes_dir = tmp.path().join("recipes");
        std::fs::create_dir_all(&recipes_dir).unwrap();
        std::fs::write(
            recipes_dir.join("recipe-run1.yaml"),
            "settings:\n  goose_model: claude-opus-4-8\n",
        )
        .unwrap();
        let staging_path = tmp.path().join("staging.jsonl");
        let ledger_path = tmp.path().join("usage.db");
        let mut ledger = UsageLedger::open(&ledger_path).unwrap();

        let result =
            collect_boi_phase_runs(&db_path, &recipes_dir, &staging_path, &mut ledger, 1_000)
                .expect("collect_boi_phase_runs should succeed against fixture-only paths");
        assert_eq!(result.accepted, 1, "the one eligible row must be accepted: {result:?}");

        let rows = ledger.rows(10, 0).unwrap();
        assert_eq!(rows.len(), 1, "ledger must contain exactly the imported row: {rows:?}");
        let row = &rows[0];
        assert_eq!(row.provider, "claude_code");
        assert_eq!(row.account_scope, "boi", "account_scope must always be \"boi\"");
        assert_eq!(row.response_id, "run1");
        assert_eq!(row.root_task_family.as_deref(), Some("specA"));
        assert_eq!(row.event_at.as_deref(), Some("2026-09-14T10:00:00Z"));
        assert_eq!(row.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(row.input_tokens, Some(1200));
        assert_eq!(row.output_tokens, Some(340));

        // Re-collecting against an UNCHANGED boi.db must be a true no-op: the
        // staging file's bytes are identical to the prior run, so the
        // ledger's byte-cursor importer recognizes there is nothing new to
        // scan (not even a duplicate observation) and the row count stays 1.
        let result2 =
            collect_boi_phase_runs(&db_path, &recipes_dir, &staging_path, &mut ledger, 1_000)
                .expect("re-collection should succeed");
        assert_eq!(result2.accepted, 0, "re-collection over unchanged data must not re-accept: {result2:?}");
        assert_eq!(ledger.rows(10, 0).unwrap().len(), 1, "row count must stay 1 after a no-op re-collection");

        // Growth (a new eligible phase_run appears) must be picked up
        // incrementally, without disturbing the already-imported row.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO phase_runs (id, spec_id, provider, tokens_in, tokens_out, started_at) VALUES ('run4','specB','codex',400,120,'2026-09-14T11:00:00Z')",
                [],
            )
            .unwrap();
        }
        let result3 =
            collect_boi_phase_runs(&db_path, &recipes_dir, &staging_path, &mut ledger, 1_000)
                .expect("collection after growth should succeed");
        assert_eq!(result3.accepted, 1, "the newly-eligible row must be accepted: {result3:?}");
        let rows3 = ledger.rows(10, 0).unwrap();
        assert_eq!(rows3.len(), 2, "ledger must now hold both rows: {rows3:?}");
        assert!(
            rows3.iter().any(|r| r.response_id == "run1"),
            "the original row must still be present: {rows3:?}"
        );
        assert!(
            rows3.iter().any(|r| r.response_id == "run4" && r.provider == "codex"),
            "the newly-collected row must be present: {rows3:?}"
        );
    }

    /// Build a fixture events.db with the same shape as telemetry::open()'s
    /// schema, so the harness-llm-cost collector reads real column names.
    fn write_events_fixture(path: &Path, rows: &[(i64, &str, &str, &str, &str)]) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE events (
                 id          INTEGER PRIMARY KEY,
                 ts          TEXT    NOT NULL,
                 source      TEXT    NOT NULL,
                 event       TEXT    NOT NULL,
                 status      TEXT    NOT NULL,
                 duration_ms INTEGER,
                 exit_code   INTEGER,
                 detail      TEXT
             )",
        )
        .unwrap();
        for (id, ts, source, event, detail) in rows {
            conn.execute(
                "INSERT INTO events (id, ts, source, event, status, duration_ms, exit_code, detail)
                 VALUES (?1, ?2, ?3, ?4, 'ok', NULL, NULL, ?5)",
                rusqlite::params![id, ts, source, event, detail],
            )
            .unwrap();
        }
    }

    /// harness-llm-cost (task Th19d8qvp): source `$HEX_DIR/.hex/telemetry/events.db`
    /// rows with `source = "llm-cost"` (`llm_cost.rs::record_llm_cost`'s own
    /// shape — `event = "<transport>::<use_case>"`, `detail` = {in_tokens,
    /// out_tokens, cost_usd, model}). provider = the event prefix before
    /// "::", account_scope = "harness", root_task_family = the use case
    /// after "::", response_id = the events.id, tokens from detail JSON.
    ///
    /// This collector must NOT open the live telemetry events.db through
    /// `hex::telemetry` (that module's `open()`/`health()` seam always
    /// resolves `$HEX_DIR/.hex/telemetry/events.db` and has no read-only or
    /// `#[cfg(test)]`-isolated mode reachable from this bin target — see
    /// `burn_alert_class`'s doc comment on why pure/explicit-path seams are
    /// used for bin-level unit tests here). Instead it takes an explicit
    /// events-db path and an already-open ledger, entirely by fixture.
    #[test]
    fn harness_llm_cost_source_imports_events_db_rows_idempotently() {
        let tmp = tempfile::TempDir::new().unwrap();
        let events_db = tmp.path().join("events.db");
        write_events_fixture(
            &events_db,
            &[
                (
                    7,
                    "2026-09-10T00:00:00Z",
                    "llm-cost",
                    "openrouter::extract",
                    r#"{"in_tokens":1200,"out_tokens":340,"cost_usd":0.0425,"model":"anthropic/claude-fable-5"}"#,
                ),
                (
                    8,
                    "2026-09-10T00:05:00Z",
                    "llm-cost",
                    "claude-cli::proposer",
                    // out_tokens=0 is the documented llm_cost.rs caveat (some
                    // paths report 0) — must be recorded as-is, not dropped
                    // or treated as "no output tokens observed" (None).
                    r#"{"in_tokens":500,"out_tokens":0,"cost_usd":0.01,"model":"claude-sonnet-4-6"}"#,
                ),
                (
                    9,
                    "2026-09-10T00:06:00Z",
                    "other-source",
                    "openrouter::extract",
                    r#"{"in_tokens":999,"out_tokens":999,"cost_usd":9.0,"model":"ignored"}"#,
                ),
            ],
        );
        let ledger_path = tmp.path().join("usage.db");
        let mut ledger = UsageLedger::open(&ledger_path).unwrap();

        let first = import_harness_llm_cost(&events_db, &mut ledger, 1_000).unwrap();
        assert_eq!(first.accepted, 2, "only the two llm-cost rows are canonical");

        let rows = ledger.rows(10, 0).unwrap();
        assert_eq!(rows.len(), 2, "the other-source row must not be imported");

        let extract = rows
            .iter()
            .find(|r| r.response_id == "7")
            .expect("openrouter::extract row (events.id=7) imported");
        assert_eq!(extract.provider, "openrouter");
        assert_eq!(extract.account_scope, "harness");
        assert_eq!(extract.root_task_family.as_deref(), Some("extract"));
        assert_eq!(extract.event_at.as_deref(), Some("2026-09-10T00:00:00Z"));
        assert_eq!(extract.model.as_deref(), Some("anthropic/claude-fable-5"));
        assert_eq!(extract.input_tokens, Some(1200));
        assert_eq!(extract.output_tokens, Some(340));

        let proposer = rows
            .iter()
            .find(|r| r.response_id == "8")
            .expect("claude-cli::proposer row (events.id=8) imported");
        assert_eq!(proposer.provider, "claude-cli");
        assert_eq!(proposer.account_scope, "harness");
        assert_eq!(proposer.root_task_family.as_deref(), Some("proposer"));
        assert_eq!(proposer.input_tokens, Some(500));
        assert_eq!(
            proposer.output_tokens,
            Some(0),
            "out_tokens=0 must be recorded as-is (llm_cost.rs caveat), not dropped or nulled"
        );

        assert!(
            !rows.iter().any(|r| r.response_id == "9"),
            "the other-source row (events.id=9) must never reach the ledger"
        );

        // Re-import must be idempotent: same two rows, no duplication, no
        // conflict-driven deletion. A non-deterministic synthesized record
        // (e.g. a collection-time timestamp baked into the hashed line)
        // would make the second import's hash differ from the first and
        // take import_parsed's conflict arm, which DELETEs the canonical
        // row instead of deduping it — so this must hold record-for-record.
        // Whether the implementation recognizes the two rows as duplicates
        // (fresh staging file each run) or as an already-advanced cursor
        // (stable staging file, re-collected with nothing new to read) is a
        // mechanism detail. Either is a valid idempotent strategy. What must
        // hold regardless: nothing new accepted, no conflict-driven delete,
        // and the ledger content itself is unchanged row-for-row.
        let second = import_harness_llm_cost(&events_db, &mut ledger, 1_000).unwrap();
        assert_eq!(second.accepted, 0, "second import must add nothing new");
        assert_eq!(second.conflicts, 0, "re-import must never conflict-delete a canonical row");
        let rows_after = ledger.rows(10, 0).unwrap();
        assert_eq!(rows_after, rows, "ledger content must be unchanged by re-import");

        // The high-water mark plus stable staging file must make a re-collect
        // a TRUE no-op: nothing new appended to the staging file means
        // import_jsonl is never even called, so coverage().duplicates must
        // stay at 0 forever, not grow by 2 on every 5-minute worker fire.
        let coverage = ledger.coverage().unwrap();
        assert_eq!(
            coverage.duplicates, 0,
            "a re-collect with nothing new must not re-observe already-canonical rows as duplicates"
        );
    }

    /// The high-water mark must let a re-collect reach rows past the first
    /// `max_records` llm-cost rows ever recorded, instead of restarting the
    /// `SELECT ... LIMIT` at the lowest id forever (the original defect: once
    /// events.db held more than `max_records` llm-cost rows, newer rows could
    /// never be imported and collect silently returned accepted=0/backlog=false).
    #[test]
    fn harness_llm_cost_high_water_mark_reaches_remaining_rows_on_next_call() {
        let tmp = tempfile::TempDir::new().unwrap();
        let events_db = tmp.path().join("events.db");
        write_events_fixture(
            &events_db,
            &[
                (
                    1,
                    "2026-09-10T00:00:00Z",
                    "llm-cost",
                    "openrouter::extract",
                    r#"{"in_tokens":100,"out_tokens":10,"cost_usd":0.001,"model":"m1"}"#,
                ),
                (
                    2,
                    "2026-09-10T00:01:00Z",
                    "llm-cost",
                    "openrouter::extract",
                    r#"{"in_tokens":100,"out_tokens":10,"cost_usd":0.001,"model":"m1"}"#,
                ),
                (
                    3,
                    "2026-09-10T00:02:00Z",
                    "llm-cost",
                    "openrouter::extract",
                    r#"{"in_tokens":100,"out_tokens":10,"cost_usd":0.001,"model":"m1"}"#,
                ),
            ],
        );
        let ledger_path = tmp.path().join("usage.db");
        let mut ledger = UsageLedger::open(&ledger_path).unwrap();

        // max_records=2 is smaller than the 3-row fixture: the first call can
        // only ever see the lowest 2 ids.
        let first = import_harness_llm_cost(&events_db, &mut ledger, 2).unwrap();
        assert_eq!(first.accepted, 2, "first call is capped at max_records rows");
        assert!(first.backlog, "hitting the max_records cap must report backlog=true");
        let rows_after_first = ledger.rows(10, 0).unwrap();
        assert_eq!(rows_after_first.len(), 2);
        assert!(!rows_after_first.iter().any(|r| r.response_id == "3"));

        // A second call with the SAME max_records must reach the row the
        // first call couldn't — not restart at id=1 and see the same two
        // rows again (the original bug: LIMIT from the lowest id forever).
        let second = import_harness_llm_cost(&events_db, &mut ledger, 2).unwrap();
        assert_eq!(second.accepted, 1, "second call must reach the remaining row (id=3)");
        assert!(!second.backlog, "no rows remain beyond this call");
        let rows_after_second = ledger.rows(10, 0).unwrap();
        assert_eq!(rows_after_second.len(), 3, "all three rows must be reachable across calls");
        assert!(rows_after_second.iter().any(|r| r.response_id == "3"));
    }

    /// Models are priced per their own table; synthetic/unknown rows skipped.
    #[test]
    fn model_pricing_and_synthetic_skip() {
        assert!(price("claude-opus-4-8").unwrap().0 == 5.0);
        assert!(price("claude-fable-5").unwrap().1 == 50.0);
        assert!(price("claude-sonnet-4-6").unwrap().0 == 3.0);
        assert!(price("<synthetic>").is_none());
        assert!(price("").is_none());
        // Unknown future claude model: falls back to top-tier rates, never $0.
        assert!(price("claude-zephyr-6").unwrap().0 == 10.0);
    }
}
