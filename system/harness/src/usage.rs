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
use hex::usage_ledger::{ImportOptions, UsageLedger};
use hex::usage_reporting;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum UsageCommands {
    /// Import one local Codex JSONL source into the durable usage ledger
    Collect {
        /// Explicit local JSONL source. Defaults to $HEX_DIR/.hex/usage/codex.jsonl.
        #[arg(long)]
        source: Option<PathBuf>,
        /// Codex home for discovery. Defaults to $HOME/.codex.
        #[arg(long)]
        codex_root: Option<PathBuf>,
        /// Ledger path. Defaults to $HEX_DIR/.hex/usage/usage.db.
        #[arg(long)]
        ledger: Option<PathBuf>,
        /// Maximum complete records committed in this run.
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
            source,
            codex_root,
            ledger,
            max_records,
        } => collect(source, codex_root, ledger, max_records),
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
                                issues.push(format!("missing_rollout={}", p.display()))
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
fn collect(
    source: Option<PathBuf>,
    codex_root: Option<PathBuf>,
    ledger: Option<PathBuf>,
    max_records: usize,
) -> i32 {
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
    if max_records == 0 || sources.is_empty() {
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
                    issues.push(format!("unreadable={}", path.display()));
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
        for window in [hex::usage_ledger::FrozenWindow::First, hex::usage_ledger::FrozenWindow::Second] {
            read.for_each_window_page(window, 1_000, |rows| {
                accumulator.extend(rows);
                Ok(())
            })?;
        }
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
    let Ok((report, detail)) = result else {
        eprintln!("usage report: ledger unavailable: {}", ledger.display());
        return 1;
    };
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
