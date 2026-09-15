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
use hex::usage_ledger::{CanonicalRow, ImportOptions, UsageLedger};
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

    #[test]
    fn failed_collector_makes_report_stale_until_success() {
        assert!(!collector_status_is_stale(None));
        assert!(collector_status_is_stale(Some("error")));
        assert!(collector_status_is_stale(Some("alert")));
        assert!(!collector_status_is_stale(Some("ok")));
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
