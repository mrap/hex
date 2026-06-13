use clap::{CommandFactory, Parser, Subcommand};
use std::io;
use std::path::PathBuf;

mod consolidate;
mod throttle;
use hex::doctor;
mod integration;
mod integration_cmd;
mod integration_check_all;
// telemetry lives in the lib (used by the in-process worker runtime too); the
// bin shares that one copy rather than compiling a second (mirrors hex::memory).
use hex::alert;
use hex::memory;
use hex::telemetry;
mod path_map;
mod env;
mod hook;
mod usage;
mod upgrade;
mod learnings;
// ops lives in the lib (the in-process worker runtime calls it too); the bin
// shares that one copy rather than compiling a second (mirrors hex::memory).
use hex::ops;
// Personal overlay (discovered, never named here). build.rs globs
// $HEX_DIR/.hex/harness-personal/integration_*.rs → OUT_DIR/personal_mods.rs,
// exposing `probe_registry() -> Vec<(&'static str, fn() -> i32)>`.
#[cfg(feature = "personal")]
mod personal_mods {
    include!(concat!(env!("OUT_DIR"), "/personal_mods.rs"));
}
#[derive(Parser)]
#[command(name = "hex", about = "Hex harness", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Integration bundle lifecycle management
    #[command(display_order = 9)]
    Integration {
        #[command(subcommand)]
        command: IntegrationCommands,
    },
    /// Behavioral and indexed memory operations
    #[command(display_order = 2)]
    Memory {
        #[command(subcommand)]
        command: MemoryCommands,
    },
    /// System health checks
    #[command(display_order = 5)]
    Doctor {
        #[command(subcommand)]
        command: DoctorCommands,
    },
    /// Environment setup utilities (Phase 5: port of env.sh non-shell logic)
    #[command(display_order = 24)]
    Env {
        #[command(subcommand)]
        command: env::EnvCommands,
    },
    /// Upgrade hex installation (native git pull + cargo build + codesign + atomic swap)
    #[command(display_order = 14)]
    Upgrade {
        /// Extra arguments forwarded to the upgrade flow (e.g. --local <path>)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Usage metrics + spend guardrails (one namespace for tracking)
    #[command(display_order = 13)]
    Usage {
        #[command(subcommand)]
        command: usage::UsageCommands,
    },
    /// Claude Code hook runners (port of .hex/hooks/scripts/*.sh)
    #[command(display_order = 13)]
    Hook {
        #[command(subcommand)]
        command: hook::HookCommands,
    },
    /// Hex harness lifecycle (single-process drain-aware host for typed Rust workers)
    #[command(display_order = 14)]
    Harness {
        #[command(subcommand)]
        command: HarnessCommands,
    },
    /// Emit hex events into the trigger substrate
    #[command(display_order = 14)]
    Triggers {
        #[command(subcommand)]
        command: TriggersCommands,
    },
    /// Read/write/delete hex key/value state from the shell (operator/debug surface)
    #[command(display_order = 14)]
    State {
        #[command(subcommand)]
        command: StateCommands,
    },
    /// Telemetry store: query and emit events from the native SQLite log
    #[command(display_order = 6)]
    Telemetry {
        #[command(subcommand)]
        command: TelemetryCommands,
    },
    /// Questions & replies: ask a structured question, reply to one by id.
    #[command(display_order = 4)]
    Messages {
        #[command(subcommand)]
        command: MessagesCommands,
    },
    /// Print resolved `claude -p` flags for a lean-run profile (spec Sf5bj7y1d).
    ///
    /// Reads built-in profiles plus optional
    /// `$HEX_DIR/.hex/config/claude-runs.toml`. Prints the flags on a single
    /// eval-safe line so shell call sites can do
    /// `claude $(hex claude-flags <profile>) -p ...`. Unknown profile names
    /// exit non-zero with a stderr explanation.
    #[command(display_order = 14, name = "claude-flags")]
    ClaudeFlags {
        /// Profile name (built-ins: default, harness_worker, eval)
        profile: String,
    },
    /// Print version
    #[command(display_order = 15)]
    Version,
    /// Generate shell completions
    #[command(display_order = 12)]
    Completions {
        shell: clap_complete::Shell,
    },
    /// Inspect auto-registered worker modules
    #[command(display_order = 14)]
    Module {
        #[command(subcommand)]
        command: ModuleCommands,
    },
    /// Ledger-anchored charter governance: register/amend/verify/log.
    /// Out-of-band edits surface as DRIFT (nonzero exit).
    #[command(display_order = 14)]
    Charter {
        #[command(subcommand)]
        command: CharterCommands,
    },
    /// Hash-chained ledger ops (append-only, tamper-evident).
    #[command(display_order = 14)]
    Ledger {
        #[command(subcommand)]
        command: LedgerCommands,
    },
    /// Lint verify-gate footguns in a BOI v2 TOML spec (shadow mode by default).
    ///
    /// Parses every verification command (contract + per-task), runs the
    /// 8-rule footgun ruleset, writes one `intent` ledger row per gate, and
    /// prints a single shadow-mode summary line. NO per-gate advice is
    /// printed until the disclosed bar clears.
    #[command(display_order = 14, name = "lint-gates")]
    LintGates {
        /// Path to a BOI v2 TOML spec.
        spec: std::path::PathBuf,
        /// Spec id to amend prior intent rows with after dispatch.
        #[arg(long = "spec-id")]
        spec_id: Option<String>,
    },
    /// Earned-autonomy dial — pure function over ledger outcome rows.
    ///
    /// Below the configured min-N matching outcomes for (agent, action_class)
    /// the dial REFUSES to print a number — it prints INSUFFICIENT. Classes
    /// flagged irreversible in the charter map always print ASK. Otherwise
    /// it prints a score in `[0, 1]`. See `src/dial.rs` for the math.
    #[command(display_order = 14)]
    Dial {
        agent: String,
        action_class: String,
        /// Minimum matching end-state outcomes required to print a number.
        /// Tuned to observed BOI dispatch volume (2-6 specs/day).
        #[arg(long, default_value_t = 3)]
        min_n: usize,
        /// Treat this class as irreversible — always print ASK.
        #[arg(long)]
        irreversible: bool,
    },
    /// Deterministic judge of the agent-infra improvement plane (P1a).
    ///
    /// `judge` runs kill gates → embedded self-test → grounded eval on the
    /// frozen held-out corpus and emits a replay-deterministic verdict
    /// (ACCEPT_FLAGGED / REJECT / INSUFFICIENT_DATA — nothing auto-lands in
    /// P1). `probe` is the verdict-store containment self-test (mode 0555;
    /// a candidate subprocess write must fail, loudly alerting otherwise).
    #[command(display_order = 14)]
    Gatekeeper {
        #[command(subcommand)]
        command: GatekeeperCommands,
    },
    /// Daily sqlite snapshots (memory/telemetry/ledger DBs) with 7-day
    /// rotation under $HEX_DIR/.hex/backups/YYYY-MM-DD/. Target of the
    /// hex-backup cron worker (04:00 daily).
    #[command(display_order = 14)]
    Backup,
    /// GitFlow release ceremony (oss-releaser). One verb: `cut`.
    #[command(display_order = 14)]
    Release {
        #[command(subcommand)]
        command: ReleaseCommands,
    },
    /// Scan the repo for personalization violations (exit 0 clean, 1 found)
    #[command(display_order = 14)]
    Sanitize {
        /// Print each matching line instead of per-category counts
        #[arg(long)]
        verbose: bool,
    },
    /// Git hook backends (invoked by the .githooks exec shims).
    #[command(name = "git-guard", hide = true)]
    GitGuard {
        #[command(subcommand)]
        command: GitGuardCommands,
    },
}

#[derive(Subcommand)]
enum ReleaseCommands {
    /// Cut a GitFlow release: gate battery, bump, merge, tag, push (--dry-run stops after the battery)
    Cut {
        /// Bump tier off the latest semver tag: patch | minor | major (default patch)
        #[arg(long)]
        level: Option<String>,
        /// Explicit next version X.Y.Z (wins over --level)
        #[arg(long)]
        version: Option<String>,
        /// Cut hotfix/X.Y.Z from main instead of release/X.Y.Z from develop
        #[arg(long)]
        hotfix: bool,
        /// Run the gate battery against the pinned SHA and stop (exit 0 all green / 1 blocked)
        #[arg(long)]
        dry_run: bool,
        /// Skip the docker e2e gate (implies --skip-parity) — loud Skipped, never silent
        #[arg(long)]
        skip_e2e: bool,
        /// Skip the codex-parity gate — loud Skipped, never silent
        #[arg(long)]
        skip_parity: bool,
    },
}

#[derive(Subcommand)]
enum GitGuardCommands {
    /// Backend for the pre-push hook: reads the standard ref-update lines on
    /// stdin and blocks a refs/heads/main push unless HEX_RELEASE_PIPELINE=1.
    #[command(name = "pre-push")]
    PrePush {
        /// Hook args the shim forwards (remote name, remote URL) — unused.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum GatekeeperCommands {
    /// Judge one proposal markdown file (two fenced TOML blocks) and append
    /// its verdict block; exit 0 on any verdict, 2 on unjudgeable input.
    Judge {
        /// Proposal markdown file.
        proposal: std::path::PathBuf,
        /// Frozen corpus JSON ({train, held, ...}); judged on `held` only.
        #[arg(long)]
        corpus: std::path::PathBuf,
        /// Precision floor below which a rule can never ACCEPT
        /// (baseline-honest.md lower CI bound).
        #[arg(long, default_value_t = hex::gatekeeper::DEFAULT_PRECISION_FLOOR)]
        floor: f64,
        /// Also write the verdict JSON here.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Timestamp recorded VERBATIM in the verdict (never a clock read —
        /// determinism contract).
        #[arg(long)]
        now: Option<String>,
        /// Verdict store dir (0555 containment); when given, a copy of the
        /// verdict lands there via the chmod-up/write/chmod-down sequence.
        #[arg(long)]
        store: Option<std::path::PathBuf>,
        /// Canary registry JSON (gates/canaries.json). When given, a
        /// registered canary can never ACCEPT, and an auditor accept on one
        /// voids approvals + appends a loud ledger alert (F4).
        #[arg(long)]
        canaries: Option<std::path::PathBuf>,
        /// boi.db path for auditor identity ground truth (read-only). When
        /// given, auditor verdicts whose spec_id is not a real BOI run are
        /// voided; author-as-auditor is voided regardless.
        #[arg(long = "boi-db")]
        boi_db: Option<std::path::PathBuf>,
    },
    /// Containment write-probe: the store must reject a candidate-context
    /// subprocess write. Breach ⇒ ledger alert + exit 1.
    Probe {
        #[arg(long)]
        store: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
enum CharterCommands {
    /// Anchor a charter file's CURRENT content as v1 (genesis row).
    Register {
        /// Charter name (e.g. proposer).
        name: String,
        /// Workspace-relative path (e.g. projects/agent-infra/charters/proposer.md).
        path: String,
        #[arg(long)]
        why: String,
        #[arg(long, default_value = "hex-cli")]
        by: String,
    },
    /// Replace a charter's content via the ONLY sanctioned write path.
    /// Refuses if the on-disk file drifted from the recorded hash.
    Amend {
        name: String,
        /// File holding the complete new charter content.
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        why: String,
        #[arg(long, default_value = "hex-cli")]
        by: String,
    },
    /// Accept an out-of-band edit into the trail, explicitly (drift_accepted=true).
    Rebaseline {
        name: String,
        #[arg(long)]
        why: String,
        #[arg(long, default_value = "hex-cli")]
        by: String,
    },
    /// Recompute every registered charter's sha256 vs the ledger. Drift =
    /// loud stderr + nonzero exit; --alert also appends a ledger ALERT row.
    Verify {
        #[arg(long)]
        alert: bool,
    },
    /// Print the governance trail (oldest first), optionally for one name.
    Log {
        name: Option<String>,
    },
    /// Current registered charters: name, version, hash, path.
    Show,
}

#[derive(Subcommand)]
enum LedgerCommands {
    /// Append a validated row (kind ∈ intent|action|outcome|heartbeat|alert).
    Append {
        #[arg(long)]
        agent: String,
        #[arg(long = "action-class")]
        action_class: String,
        #[arg(long)]
        kind: String,
        /// JSON payload (defaults to {}).
        #[arg(long, default_value = "{}")]
        payload: String,
    },
    /// Walk the chain end-to-end; nonzero exit on any break.
    Verify,
    /// Print last-seen-at per agent (used by freshness alerting).
    Freshness,
    /// S1-wild join: linter intents × reconciler outcomes by gate_hash
    /// (DISTINCT, latest event wins). JSON report: per-gate rows + the
    /// linter's wild confusion matrix. The proposer's nightly feed.
    Wild {
        /// Only include gates whose wild event time (`first_started_at`)
        /// is at or after this ISO8601/RFC3339 instant.
        #[arg(long)]
        since: Option<String>,
        /// Ledger db path (defaults to $HEX_DIR/.hex/ledger/ledger.db).
        #[arg(long)]
        db: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum ModuleCommands {
    /// List every registered worker, its triggers, its source, and its
    /// enabled/disabled state
    List,
    /// Show one worker: triggers + source file + enabled/disabled state
    Status { name: String },
    /// Re-enable a disabled module (takes effect at its next fire; no restart)
    Enable { name: String },
    /// Disable a module: it stays scheduled, but every fire logs a loud skip
    /// and does nothing (takes effect at its next fire; no restart)
    Disable { name: String },
}

#[derive(Subcommand)]
enum MessagesCommands {
    /// Submit a message; prints the Result (and any question's options + ids).
    Submit { text: String },
    /// Reply to a question by id. Selection = `b` or `a,c` (option ids); plus optional --text.
    Reply {
        question_id: String,
        #[arg(default_value = "")]
        selection: String,
        #[arg(long)]
        text: Option<String>,
    },
}

#[derive(Subcommand)]
enum HarnessCommands {
    /// Install (idempotent) and load the com.hex.harness service via daemon-green.
    Start,
    /// Stop + unload the com.hex.harness service via daemon-green.
    Stop,
    /// Restart the com.hex.harness service (pick up a new binary) via daemon-green.
    Restart,
    /// List registered workers and report engine health.
    Status,
    /// Tail the last N lines of the harness service log via daemon-green.
    Logs {
        /// Number of trailing lines to print.
        #[arg(long, default_value_t = 200)]
        lines: usize,
    },
    /// Run the harness lifecycle loop (invoked by launchd; hidden from --help).
    #[command(hide = true)]
    Serve,
}

#[derive(Subcommand)]
enum TriggersCommands {
    /// Emit a hex event into the trigger substrate
    Emit {
        /// Event name (e.g. boi.spec.complete)
        event: String,
        /// JSON event payload (default `{}`)
        #[arg(long)]
        data: Option<String>,
        /// Producer attribution (defaults to $HEX_PRODUCER or "cli")
        #[arg(long)]
        producer: Option<String>,
    },
}

#[derive(Subcommand)]
enum StateCommands {
    /// Print the JSON value at <scope>/<key>; empty + exit 1 if absent
    Get { scope: String, key: String },
    /// Set <scope>/<key> to <json> (a JSON literal, e.g. '{"a":1}' or '"str"')
    Set { scope: String, key: String, json: String },
    /// Delete <scope>/<key>
    Delete { scope: String, key: String },
}

#[derive(Subcommand)]
enum TelemetryCommands {
    /// Show recent events (newest first)
    Recent {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Show non-ok events since the given window (e.g. 24h, 7d)
    Failures {
        #[arg(long, default_value = "24h")]
        since: String,
        #[arg(long)]
        json: bool,
    },
    /// Aggregated per-event status (last run + ok/error counts)
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Append a single event from the CLI (or shell scripts)
    Record {
        #[arg(long)]
        source: String,
        #[arg(long)]
        event: String,
        #[arg(long)]
        status: String,
        #[arg(long = "duration-ms")]
        duration_ms: Option<i64>,
        #[arg(long = "exit-code")]
        exit_code: Option<i64>,
        #[arg(long)]
        detail: Option<String>,
    },
    /// Delete events older than keep-days
    Prune {
        #[arg(long = "keep-days", default_value_t = 30)]
        keep_days: i64,
    },
}

#[derive(Subcommand)]
enum ConsolidateCommands {
    /// Deterministic layers only (structural + memory DB + learnings promotion). No LLM, no network. Safe to run nightly.
    Quick {
        /// Run at full (normal) OS scheduling priority instead of background-throttled.
        #[arg(long)]
        max: bool,
    },
    /// All deterministic layers + the LLM-assisted operating-model audit.
    Full {
        /// Run at full (normal) OS scheduling priority instead of background-throttled.
        #[arg(long)]
        max: bool,
    },
}

#[derive(Subcommand)]
enum IntegrationCommands {
    /// Install an integration bundle
    Install { name: String },
    /// Uninstall an integration bundle
    Uninstall { name: String },
    /// Update an integration bundle
    Update { name: String },
    /// List installed integrations
    List,
    /// Validate an integration bundle
    Validate { name: String },
    /// Show integration status
    Status { name: Option<String> },
    /// Probe an integration's connectivity
    Probe { name: String },
    /// Rotate an integration's credentials
    Rotate { name: String },
    /// Print integration health-check template to stdout (port of integrations/_template.sh)
    Template,
    /// Run integration checks for a tier in parallel (port of hex-integration-check-all.sh)
    #[command(name = "check-all")]
    CheckAll {
        /// Tier to check: critical, standard, slow, or all (default: all)
        #[arg(long, default_value = "all")]
        tier: String,
    },
}

#[derive(Subcommand)]
enum MemoryCommands {
    /// Unified consolidation (structural + memory + learnings promotion + operating-model audit)
    Consolidate {
        #[command(subcommand)]
        command: ConsolidateCommands,
    },
    /// Search indexed memory files
    Search {
        query: String,
        /// Number of results (default 10)
        #[arg(long, default_value = "10")]
        top: usize,
        /// Filter results to paths matching this pattern
        #[arg(long)]
        file: Option<String>,
        /// Compact single-line output per result
        #[arg(long)]
        compact: bool,
        /// Show N lines of context around matching terms
        #[arg(long)]
        context: Option<usize>,
        /// Exclude sensitive paths (me/, people/, raw/)
        #[arg(long)]
        private: bool,
    },
    /// Index memory files
    Index {
        #[arg(long)]
        full: bool,
        #[arg(long)]
        stats: bool,
        /// Run at full (normal) OS scheduling priority instead of background-throttled
        #[arg(long)]
        max: bool,
    },
    /// Parse Claude JSONL transcripts to markdown
    #[command(name = "parse-transcripts", hide = true)]
    ParseTranscripts {
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Retrieve workspace memory relevant to a query (FTS5 contextual recall).
    /// Internal: invoked by the memory-injection hook / BOI consumers, not humans.
    #[command(hide = true)]
    Recall {
        query: String,
        /// Apply the private filter (for BOI worker consumers)
        #[arg(long)]
        agent: bool,
    },
    /// Distill facts from a file into the memory facts layer.
    /// Internal: pipeline plumbing, not a human-facing command.
    #[command(hide = true)]
    Distill {
        /// Path to the file to distill
        path: PathBuf,
    },
    /// Show memory database statistics (facts, files, predicates, schema version)
    Stats {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Print ~10 recency-ordered pointers into the live workspace (project dirs,
    /// recent decisions, todo "Now" items). No LLM, target <200ms.
    Recent,
    /// Scheduled self-repair for memory.db: orphan-vector sweep, FTS5 optimize,
    /// transcript_files hygiene, optional VACUUM + facts backfill
    Maintain {
        #[arg(long)]
        vacuum: bool,
        #[arg(long)]
        backfill_facts: bool,
    },
}

#[derive(Subcommand)]
enum DoctorCommands {
    /// Run all registered DoctorCheck impls (Rust framework)
    Run {
        #[arg(long)]
        fix: bool,
        #[arg(long)]
        smoke: bool,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        json: bool,
        /// Only run checks whose name contains this pattern
        #[arg(long)]
        filter: Option<String>,
    },
    /// List all registered checks
    List,
    /// Scan for stale dependency-blocked items (port of stale_deps.py)
    #[command(name = "stale-deps")]
    StaleDeps {
        /// Days threshold before an item is considered stale
        #[arg(long, default_value = "2")]
        threshold: u32,
        /// Output JSON
        #[arg(long)]
        json: bool,
    },
}

fn get_hex_dir() -> PathBuf {
    if let Ok(v) = std::env::var("HEX_DIR") {
        let p = PathBuf::from(&v);
        if !p.join("CLAUDE.md").exists() {
            eprintln!(
                "ERROR: HEX_DIR={} does not contain CLAUDE.md — not a valid hex workspace",
                v
            );
            std::process::exit(1);
        }
        return p;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| {
        eprintln!("ERROR: neither HEX_DIR nor HOME is set");
        std::process::exit(1);
    });
    let p = PathBuf::from(&home).join("hex");
    if !p.join("CLAUDE.md").exists() {
        eprintln!(
            "ERROR: default hex dir {} does not contain CLAUDE.md — set HEX_DIR explicitly",
            p.display()
        );
        std::process::exit(1);
    }
    p
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Harness { command } => match command {
            HarnessCommands::Start => std::process::exit(harness_start()),
            HarnessCommands::Stop => std::process::exit(harness_stop()),
            HarnessCommands::Restart => std::process::exit(harness_restart()),
            HarnessCommands::Status => std::process::exit(harness_status()),
            HarnessCommands::Logs { lines } => std::process::exit(harness_logs(lines)),
            HarnessCommands::Serve => {
                // Bootstrap secrets before the worker runtime starts (before any
                // thread is spawned). Reads $HEX_DIR/.hex/secrets/*.env and injects
                // into the process env — no secrets appear in the plist.
                if let Ok(hex_dir) = std::env::var("HEX_DIR") {
                    bootstrap_secrets_env(std::path::Path::new(&hex_dir));
                }
                std::process::exit(hex::worker::runtime::serve(hex::workers::registry()))
            }
        },
        Commands::Triggers { command } => match command {
            TriggersCommands::Emit { event, data, producer } => {
                let parsed: serde_json::Value = match data.as_deref() {
                    None | Some("") => serde_json::Value::Object(Default::default()),
                    Some(s) => match serde_json::from_str(s) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("hex triggers emit: --data is not valid JSON: {e}");
                            std::process::exit(2);
                        }
                    },
                };
                match ops::emit(&event, parsed, producer.as_deref()) {
                    Ok(()) => std::process::exit(0),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
            }
        },
        Commands::State { command } => match command {
            StateCommands::Get { scope, key } => match ops::state_get(&scope, &key) {
                Ok(Some(v)) => {
                    println!("{}", serde_json::to_string(&v).unwrap());
                    std::process::exit(0)
                }
                Ok(None) => std::process::exit(1), // absent → empty stdout, exit 1
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2)
                }
            },
            StateCommands::Set { scope, key, json } => {
                let value: serde_json::Value = match serde_json::from_str(&json) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("hex state set: invalid JSON for value: {e}");
                        std::process::exit(2)
                    }
                };
                match ops::state_set(&scope, &key, &value) {
                    Ok(()) => std::process::exit(0),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(2)
                    }
                }
            }
            StateCommands::Delete { scope, key } => match ops::state_delete(&scope, &key) {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2)
                }
            },
        },
        Commands::Integration { command } => {
            if let IntegrationCommands::Template = command {
                integration::template();
                return;
            }
            if let IntegrationCommands::CheckAll { ref tier } = command {
                let hex_dir = get_hex_dir();
                let code = integration_check_all::run(&hex_dir, tier);
                std::process::exit(code);
            }
            // Native Rust ports of Python integration commands
            if let IntegrationCommands::List = command {
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::list(&hex_dir, false));
            }
            if let IntegrationCommands::Status { ref name } = command {
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::status(&hex_dir, name.as_deref(), false));
            }
            if let IntegrationCommands::Probe { ref name } = command {
                // Personal overlay probes (discovered, never named here) take
                // precedence; unknown names fall through to the bundle probe.
                #[cfg(feature = "personal")]
                if let Some((_, f)) =
                    personal_mods::probe_registry().iter().find(|(n, _)| n == name)
                {
                    std::process::exit(f());
                }
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::probe(&hex_dir, name, false, false));
            }
            if let IntegrationCommands::Rotate { ref name } = command {
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::rotate(&hex_dir, name, false, false));
            }
            if let IntegrationCommands::Validate { ref name } = command {
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::validate(&hex_dir, name, false, false));
            }
            if let IntegrationCommands::Update { ref name } = command {
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::update(&hex_dir, name, false, false, false, false));
            }
            let hex_dir = get_hex_dir();
            let script = hex_dir.join(".hex/scripts/hex-integration");
            let (subcmd, name_arg): (&str, Option<String>) = match &command {
                IntegrationCommands::Install { name } => ("install", Some(name.clone())),
                IntegrationCommands::Uninstall { name } => ("uninstall", Some(name.clone())),
                IntegrationCommands::Update { .. } => unreachable!(),
                IntegrationCommands::List => unreachable!(),
                IntegrationCommands::Validate { .. } => unreachable!(),
                IntegrationCommands::Status { .. } => unreachable!(),
                IntegrationCommands::Probe { .. } => unreachable!(),
                IntegrationCommands::Rotate { .. } => unreachable!(),
                IntegrationCommands::Template => unreachable!(),
                IntegrationCommands::CheckAll { .. } => unreachable!(),
            };
            let mut cmd = std::process::Command::new("bash");
            cmd.arg(&script).arg(subcmd);
            if let Some(n) = &name_arg {
                cmd.arg(n);
            }
            cmd.env("HEX_DIR", &hex_dir);
            let status = cmd.status().unwrap_or_else(|e| {
                eprintln!("hex integration: failed to run script: {e}");
                std::process::exit(1);
            });
            let exit_code = status.code().unwrap_or(1);
            std::process::exit(exit_code);
        }
        Commands::Memory { command } => {
            let hex_dir = get_hex_dir();
            let exit_code = match &command {
                MemoryCommands::Search { query, top, file, compact, context, private } => {
                    let args = memory::search::SearchArgs {
                        query: query.clone(),
                        top: *top,
                        file: file.clone(),
                        compact: *compact,
                        context: *context,
                        private: *private,
                    };
                    memory::search::run(&hex_dir, &args)
                }
                MemoryCommands::Index { full, stats, max } => {
                    // --stats is a cheap read; only throttle the heavy index path.
                    if !*stats {
                        throttle::apply("memory index", *max);
                    }
                    memory::index::run(&hex_dir, *full, *stats)
                }
                MemoryCommands::ParseTranscripts { file, dry_run, force } => {
                    let args = memory::parse_transcripts::ParseArgs {
                        file: file.clone(),
                        dry_run: *dry_run,
                        force: *force,
                    };
                    memory::parse_transcripts::run(&hex_dir, &args)
                }
                MemoryCommands::Recall { query, agent } => {
                    memory::recall::run(&hex_dir, query, *agent)
                }
                MemoryCommands::Distill { path } => {
                    let db_path = memory::db_path(&hex_dir);
                    match memory::open_db(&db_path) {
                        Ok(mut conn) => {
                            let path_str = path.to_string_lossy().to_string();
                            match memory::distill::run_on_file(&mut conn, &path_str, 500) {
                                Ok(report) => {
                                    println!(
                                        "distill: adds={} updates={} noops={} flags={}",
                                        report.adds, report.updates, report.noops, report.flags
                                    );
                                    0
                                }
                                Err(e) => {
                                    eprintln!("distill error: {}", e);
                                    1
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("open_db error: {}", e);
                            1
                        }
                    }
                }
                MemoryCommands::Stats { json } => {
                    memory::stats::run(&hex_dir, *json)
                }
                MemoryCommands::Recent => {
                    memory::recent::run(&hex_dir)
                }
                MemoryCommands::Maintain { vacuum, backfill_facts } => {
                    memory::maintain::run(&hex_dir, *vacuum, *backfill_facts)
                }
                MemoryCommands::Consolidate { command } => {
                    let (mode, max) = match command {
                        ConsolidateCommands::Quick { max } => (consolidate::Mode::Quick, *max),
                        ConsolidateCommands::Full { max } => (consolidate::Mode::Full, *max),
                    };
                    consolidate::run(mode, max, &hex_dir)
                }
            };
            std::process::exit(exit_code);
        }
        Commands::Doctor { command } => {
            let hex_dir = get_hex_dir();
            match command {
                DoctorCommands::StaleDeps { threshold, json } => {
                    let code = doctor::stale_deps(&hex_dir, threshold, json);
                    std::process::exit(code);
                }
                DoctorCommands::Run { fix, smoke: _, quiet, json, filter } => {
                    let ctx = doctor::Context::new(hex_dir.clone(), fix);
                    let runner = match &filter {
                        Some(pat) => doctor::Runner::filtered(pat),
                        None => doctor::Runner::all_checks(),
                    };
                    let results = runner.run(&ctx);
                    if json {
                        doctor::reporter::print_json(&results);
                    } else {
                        doctor::reporter::print_text(&results, quiet);
                    }
                    let exit_code = doctor::reporter::exit_code(&results);
                    std::process::exit(exit_code);
                }
                DoctorCommands::List => {
                    doctor::Runner::all_checks().list();
                }
            }
        }
        Commands::Env { command } => env::run_env_command(command),
        Commands::Backup => {
            let hex_dir = get_hex_dir();
            std::process::exit(hex::backup::run(&hex_dir));
        }
        Commands::Upgrade { args } => {
            std::process::exit(upgrade::run(&args));
        }
        Commands::Telemetry { command } => {
            std::process::exit(run_telemetry(command));
        }
        Commands::Messages { command } => {
            std::process::exit(run_messages(command));
        }
        Commands::Hook { command } => hook::run(command),
        Commands::Usage { command } => std::process::exit(usage::run(command)),
        Commands::Version => {
            println!("hex {} ({})", env!("CARGO_PKG_VERSION"), env!("HEX_GIT_SHA"));
        }
        Commands::ClaudeFlags { profile } => {
            // Read HEX_DIR optionally — claude-flags must work even when the
            // workspace is not yet bootstrapped (e.g. eval harness on a fresh
            // clone), so we do NOT go through get_hex_dir()'s CLAUDE.md gate.
            let hex_dir = std::env::var("HEX_DIR").ok().map(std::path::PathBuf::from);
            let resolved = match hex::claude_runs::resolve(&profile, hex_dir.as_deref()) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            };
            // Load workspace MCP config (lookup base for mcp_servers). Search
            // from $HEX_DIR if set, else current dir.
            let workspace = hex_dir
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
            let mcp_cfg = match hex::claude_runs::McpConfig::load(&workspace) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            };
            let flags = match resolved.to_cli_flags(&mcp_cfg) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            };
            println!("{}", hex::claude_runs::render_shell_line(&flags));
        }
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "hex", &mut io::stdout());
        }
        Commands::Module { command } => match command {
            ModuleCommands::List => {
                let hex_dir = get_hex_dir();
                let disabled = match hex::module_state::disabled_set(&hex_dir) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("hex module list: disabled-store unreadable ({e}) — states shown as enabled");
                        Default::default()
                    }
                };
                let registry = hex::workers::registry();
                for w in &registry {
                    let (kind, _path) = module_source(&w.name);
                    let state = if disabled.contains(&w.name) { "  DISABLED" } else { "" };
                    println!("{:<28} [{}]  {}{}", w.name, kind, trigger_summary(w), state);
                }
                // A disabled name with no registered worker is drift — loud.
                for name in &disabled {
                    if !registry.iter().any(|w| &w.name == name) {
                        eprintln!(
                            "hex module list: WARNING — '{name}' is in the disabled store but not in this binary's registry"
                        );
                    }
                }
                std::process::exit(0)
            }
            ModuleCommands::Status { name } => {
                match hex::workers::registry().into_iter().find(|w| w.name == name) {
                    Some(w) => {
                        let hex_dir = get_hex_dir();
                        let state = match hex::module_state::disabled_set(&hex_dir) {
                            Ok(s) if s.contains(&w.name) => "disabled",
                            Ok(_) => "enabled",
                            Err(e) => {
                                eprintln!("hex module status: disabled-store unreadable ({e})");
                                "unknown (store unreadable)"
                            }
                        };
                        let (kind, path) = module_source(&w.name);
                        println!("name:     {}", w.name);
                        println!("source:   {kind}");
                        if !path.is_empty() {
                            println!("file:     {path}");
                        }
                        println!("triggers: {}", trigger_summary(&w));
                        println!("state:    {state}");
                        std::process::exit(0)
                    }
                    None => {
                        eprintln!("hex module status: no worker named '{name}'");
                        std::process::exit(1)
                    }
                }
            }
            ModuleCommands::Enable { name } => {
                std::process::exit(module_set_enabled(&name, true));
            }
            ModuleCommands::Disable { name } => {
                std::process::exit(module_set_enabled(&name, false));
            }
        },
        Commands::Charter { command } => {
            let hex_dir = get_hex_dir();
            match command {
                CharterCommands::Register { name, path, why, by } => {
                    match hex::charter::register(&hex_dir, &name, &path, &by, &why) {
                        Ok(st) => println!("charter '{}' registered at v{} ({})", st.name, st.version, st.sha256),
                        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
                    }
                }
                CharterCommands::Amend { name, file, why, by } => {
                    match hex::charter::amend(&hex_dir, &name, &file, &by, &why) {
                        Ok(st) => println!("charter '{}' amended to v{} ({})", st.name, st.version, st.sha256),
                        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
                    }
                }
                CharterCommands::Rebaseline { name, why, by } => {
                    match hex::charter::rebaseline(&hex_dir, &name, &by, &why) {
                        Ok(st) => println!("charter '{}' rebaselined to v{} ({}) — drift accepted into the trail", st.name, st.version, st.sha256),
                        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
                    }
                }
                CharterCommands::Verify { alert } => {
                    match hex::charter::verify(&hex_dir, alert) {
                        Ok(drifts) if drifts.is_empty() => {
                            println!("charter verify OK ({} registered)", hex::charter::latest_states(&hex_dir).map(|m| m.len()).unwrap_or(0));
                        }
                        Ok(drifts) => {
                            eprintln!("charter verify: {} DRIFTED", drifts.len());
                            std::process::exit(1);
                        }
                        Err(e) => { eprintln!("{e}"); std::process::exit(2); }
                    }
                }
                CharterCommands::Log { name } => {
                    match hex::charter::log(&hex_dir, name.as_deref()) {
                        Ok(rows) => {
                            for (ts, v) in rows {
                                println!("{} {}", chrono::DateTime::from_timestamp(ts, 0).map(|d| d.to_rfc3339()).unwrap_or_else(|| ts.to_string()), v);
                            }
                        }
                        Err(e) => { eprintln!("{e}"); std::process::exit(2); }
                    }
                }
                CharterCommands::Show => {
                    match hex::charter::latest_states(&hex_dir) {
                        Ok(states) if states.is_empty() => println!("no charters registered"),
                        Ok(states) => {
                            for (_, st) in states {
                                println!("{:<12} v{:<3} {}  {}", st.name, st.version, st.sha256, st.path);
                            }
                        }
                        Err(e) => { eprintln!("{e}"); std::process::exit(2); }
                    }
                }
            }
        }
        Commands::Ledger { command } => {
            let hex_dir = get_hex_dir();
            let path = hex::ledger::default_path(&hex_dir);
            match command {
                LedgerCommands::Append { agent, action_class, kind, payload } => {
                    let payload_json: serde_json::Value = match serde_json::from_str(&payload) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("hex ledger append: --payload is not valid JSON: {e}");
                            std::process::exit(2);
                        }
                    };
                    let ledger = match hex::ledger::Ledger::open(&path) {
                        Ok(l) => l,
                        Err(e) => {
                            eprintln!("hex ledger append: open failed: {e}");
                            std::process::exit(1);
                        }
                    };
                    match ledger.append(&agent, &action_class, &kind, &payload_json) {
                        Ok(id) => {
                            println!("{}", id);
                            std::process::exit(0)
                        }
                        Err(e) => {
                            eprintln!("hex ledger append: {e}");
                            std::process::exit(1)
                        }
                    }
                }
                LedgerCommands::Verify => match hex::ledger::verify(&path) {
                    Ok(n) => {
                        println!("ledger verify OK ({} rows)", n);
                        std::process::exit(0)
                    }
                    Err(e) => {
                        eprintln!("hex ledger verify: TAMPER DETECTED — {e}");
                        std::process::exit(1)
                    }
                },
                LedgerCommands::Freshness => {
                    let ledger = match hex::ledger::Ledger::open(&path) {
                        Ok(l) => l,
                        Err(e) => {
                            eprintln!("hex ledger freshness: open failed: {e}");
                            std::process::exit(1);
                        }
                    };
                    std::process::exit(run_freshness(&ledger));
                }
                LedgerCommands::Wild { since, db } => {
                    let db_path = db.unwrap_or(path);
                    let since_epoch = match since.as_deref() {
                        None => None,
                        Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
                            Ok(dt) => Some(dt.timestamp()),
                            Err(e) => {
                                eprintln!(
                                    "hex ledger wild: --since {s:?} is not RFC3339/ISO8601 (e.g. 2026-06-10T03:30:00Z): {e}"
                                );
                                std::process::exit(2);
                            }
                        },
                    };
                    match hex::wild::wild_report(&db_path, since_epoch, since) {
                        Ok(report) => {
                            match serde_json::to_string_pretty(&report) {
                                Ok(j) => println!("{j}"),
                                Err(e) => {
                                    eprintln!("hex ledger wild: serialize failed: {e}");
                                    std::process::exit(1);
                                }
                            }
                            std::process::exit(0);
                        }
                        Err(e) => {
                            eprintln!("hex ledger wild: {e}");
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
        Commands::LintGates { spec, spec_id } => {
            let src = match std::fs::read_to_string(&spec) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("hex lint-gates: cannot read {}: {e}", spec.display());
                    std::process::exit(2);
                }
            };
            let gates = match hex::lint_gates::extract_gates_from_spec(&src) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("hex lint-gates: {e}");
                    std::process::exit(2);
                }
            };
            // Open ledger and append one intent row per gate. Shadow mode.
            let hex_dir = get_hex_dir();
            let path = hex::ledger::default_path(&hex_dir);
            let ledger = match hex::ledger::Ledger::open(&path) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("hex lint-gates: ledger open failed: {e}");
                    std::process::exit(1);
                }
            };
            for gate in &gates {
                let v = hex::lint_gates::analyze_command(gate);
                let predicted = match v.predicted {
                    hex::lint_gates::Prediction::Pass => "pass",
                    hex::lint_gates::Prediction::Fail => "fail",
                };
                let mut payload = serde_json::json!({
                    "gate_hash": v.content_hash,
                    "predicted": predicted,
                    "rules_fired": v.rules_fired,
                    "shadow": true,
                    "command": gate,
                });
                if let Some(ref sid) = spec_id {
                    payload["spec_id"] = serde_json::Value::String(sid.clone());
                }
                if let Err(e) = ledger.append("lint-gates", "verify-gate", "intent", &payload) {
                    eprintln!("hex lint-gates: ledger append failed: {e}");
                    std::process::exit(1);
                }
            }
            println!("{}", hex::lint_gates::shadow_summary(&gates));
            std::process::exit(0);
        }
        Commands::Dial { agent, action_class, min_n, irreversible } => {
            // Load all outcome rows from the ledger and feed the pure dial.
            let hex_dir = get_hex_dir();
            let path = hex::ledger::default_path(&hex_dir);
            let rows = match load_outcome_rows(&path) {
                Ok(rs) => rs,
                Err(e) => {
                    eprintln!("hex dial: load outcomes failed: {e}");
                    std::process::exit(1);
                }
            };
            let out = hex::dial::compute(&rows, &agent, &action_class, min_n, irreversible);
            match out {
                hex::dial::DialOutcome::Insufficient { n, min_n } => {
                    println!("INSUFFICIENT (n={n}, min_n={min_n})");
                    std::process::exit(0)
                }
                hex::dial::DialOutcome::Ask => {
                    println!("ASK");
                    std::process::exit(0)
                }
                hex::dial::DialOutcome::Score(s) => {
                    println!("{:.4}", s);
                    std::process::exit(0)
                }
            }
        }
        Commands::Gatekeeper { command } => match command {
            GatekeeperCommands::Judge { proposal, corpus, floor, out, now, store, canaries, boi_db } => {
                let hex_dir = get_hex_dir();
                // Dial consult — recorded in the verdict, never upgrades it
                // (P1: everything flags to Mike regardless).
                let ledger_path = hex::ledger::default_path(&hex_dir);
                let dial = match load_outcome_rows(&ledger_path) {
                    Ok(rows) => {
                        match hex::dial::compute(&rows, "proposer", "proposal.land", 3, false) {
                            hex::dial::DialOutcome::Insufficient { n, min_n } => {
                                format!("INSUFFICIENT (n={n}, min_n={min_n})")
                            }
                            hex::dial::DialOutcome::Ask => "ASK".to_string(),
                            hex::dial::DialOutcome::Score(s) => format!("{s:.4}"),
                        }
                    }
                    Err(_) => "UNAVAILABLE".to_string(),
                };
                std::process::exit(hex::gatekeeper::cli_judge(
                    &proposal,
                    &corpus,
                    floor,
                    out.as_deref(),
                    now,
                    store.as_deref(),
                    canaries.as_deref(),
                    boi_db.as_deref(),
                    &hex_dir,
                    dial,
                ));
            }
            GatekeeperCommands::Probe { store } => {
                let hex_dir = get_hex_dir();
                std::process::exit(hex::gatekeeper::cli_probe(&store, &hex_dir));
            }
        },
        Commands::Release { command } => match command {
            ReleaseCommands::Cut { level, version, hotfix, dry_run, skip_e2e, skip_parity } => {
                // `version` wins over `level`; `level` defaults to patch —
                // CutOptions owns that precedence, clap passes both raw.
                let level = match level.as_deref().map(str::parse::<hex::release::BumpLevel>) {
                    Some(Ok(l)) => Some(l),
                    Some(Err(e)) => {
                        eprintln!("hex release cut: {e}");
                        std::process::exit(1);
                    }
                    None => None,
                };
                let opts = hex::release::CutOptions {
                    level,
                    version,
                    hotfix,
                    dry_run,
                    skip: hex::release::SkipFlags { skip_e2e, skip_parity },
                };
                match hex::release::cut(&opts) {
                    Ok(()) => std::process::exit(0),
                    Err(e) => {
                        eprintln!("hex release cut: {e:#}");
                        std::process::exit(1);
                    }
                }
            }
        },
        Commands::Sanitize { verbose } => {
            // Scan the repo containing the current directory — the analog of
            // the bash script scanning its own repo root.
            let repo_root = match std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output()
            {
                Ok(out) if out.status.success() => {
                    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
                }
                _ => {
                    eprintln!("hex sanitize: not inside a git repository — run from the repo to scan");
                    std::process::exit(2);
                }
            };
            match hex::sanitize::scan(&repo_root, verbose) {
                Ok(violations) if violations.is_empty() => std::process::exit(0),
                Ok(_) => std::process::exit(1),
                Err(e) => {
                    eprintln!("hex sanitize: {e:#}");
                    std::process::exit(2);
                }
            }
        }
        Commands::GitGuard { command } => match command {
            GitGuardCommands::PrePush { args: _ } => {
                // The pre-push hook protocol: one "<local ref> <local sha>
                // <remote ref> <remote sha>" line per ref on stdin.
                let mut input = String::new();
                if let Err(e) = io::Read::read_to_string(&mut io::stdin(), &mut input) {
                    eprintln!("hex git-guard pre-push: reading stdin: {e}");
                    std::process::exit(1);
                }
                match hex::release::git_guard_pre_push(&input) {
                    Ok(()) => std::process::exit(0),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
            }
        },
    }
}

/// Load every `outcome`-kind row from the ledger into [`hex::dial::OutcomeRow`]s.
/// Errors loudly per S6 — no silent skip on a malformed row.
fn load_outcome_rows(
    path: &std::path::Path,
) -> Result<Vec<hex::dial::OutcomeRow>, String> {
    use rusqlite::Connection;
    let conn = Connection::open(path).map_err(|e| format!("open: {e}"))?;
    let mut stmt = conn
        .prepare("SELECT ts, agent, action_class, payload FROM ledger WHERE kind='outcome'")
        .map_err(|e| format!("prepare: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| format!("query: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        let (ts, agent, action_class, payload) =
            row.map_err(|e| format!("row read: {e}"))?;
        let v: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|e| format!("payload parse (ts={ts}): {e}"))?;
        let success = v.get("success").and_then(|x| x.as_bool()).unwrap_or(false);
        out.push(hex::dial::OutcomeRow {
            agent,
            action_class,
            success,
            ts,
        });
    }
    Ok(out)
}

// Per-agent freshness windows live in `hex::ledger::default_freshness_window_secs`
// (unit-tested there; the hex-freshness cron worker and this CLI share it).
use hex::ledger::default_freshness_window_secs;

/// Execute the freshness check loudly per S6:
///   - print one summary line per agent;
///   - on a stale agent, append an `alert` row, fire an `osascript`
///     notification, and exit non-zero.
fn run_freshness(ledger: &hex::ledger::Ledger) -> i32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let rows = match ledger.last_ts_per_agent() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("hex ledger freshness: {e}");
            return 1;
        }
    };

    let mut stale = 0usize;
    for (agent, ts) in &rows {
        let age = now - *ts;
        let window = default_freshness_window_secs(agent);
        let state = if age > window { "STALE" } else { "fresh" };
        println!("{agent}\tlast_ts={ts}\tage={age}s\twindow={window}s\t{state}");
        if age > window {
            stale += 1;
            let alert = serde_json::json!({
                "stale_agent": agent,
                "last_ts": ts,
                "age_seconds": age,
                "window_seconds": window,
            });
            if let Err(e) =
                ledger.append("freshness", "freshness.alert", "alert", &alert)
            {
                eprintln!("hex ledger freshness: alert append failed: {e}");
            }
            // macOS notification (osascript). Best-effort — log a stderr
            // miss so it's never silent.
            #[cfg(target_os = "macos")]
            {
                let msg = format!(
                    "display notification \"hex agent '{}' is stale ({}s > {}s)\" with title \"hex freshness\"",
                    agent, age, window
                );
                if let Err(e) = std::process::Command::new("osascript")
                    .arg("-e")
                    .arg(&msg)
                    .status()
                {
                    eprintln!("hex ledger freshness: osascript failed: {e}");
                }
            }
            eprintln!(
                "hex ledger freshness: ALERT — '{}' stale (age={}s, window={}s)",
                agent, age, window
            );
        }
    }
    if stale > 0 {
        eprintln!("hex ledger freshness: {} stale agent(s)", stale);
        1
    } else {
        0
    }
}

/// The reverse-DNS label of the harness service. Single source of truth — all
/// daemon-green calls go through this constant so a rename only touches one
/// line.
const HARNESS_LABEL: &str = "com.hex.harness";

/// Build the platform-neutral `ServiceSpec` that daemon-green renders into the
/// per-user launchd plist (macOS) or systemd --user unit (Linux). Reproduces
/// the exact behavior of the old `render_harness_plist` template:
///   - program           = $HEX_DIR/.hex/bin/hex
///   - args              = ["harness", "serve"]
///   - working_dir       = $HEX_DIR
///   - env               = HEX_DIR, III_URL, PATH (homebrew prepended),
///                         GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND=file
///   - keep_alive        = true (restart on crash)
///   - run_at_load       = true (start at login)
///   - log_path          = $HEX_DIR/.hex/logs/com.hex.harness.log
/// daemon-green guarantees the rendered plist omits the launchd login-session
/// detach key (verified 2026-06-05: when present, keychain reads fail rc=36;
/// when absent, rc=0). We deliberately do NOT — and CANNOT — set it here.
fn build_harness_spec(hex_dir: &std::path::Path) -> daemon_green::ServiceSpec {
    let hex_bin = hex_dir.join(".hex").join("bin").join("hex");
    let log_path = hex_dir
        .join(".hex")
        .join("logs")
        .join("com.hex.harness.log");
    // launchd hands the agent a minimal env — guarantee homebrew is on PATH so
    // the folded-in workers (gws, cargo, etc.) resolve.
    let base_path =
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
    let path_env = if base_path.split(':').any(|p| p == "/opt/homebrew/bin") {
        base_path
    } else {
        format!("/opt/homebrew/bin:{base_path}")
    };
    let spec = daemon_green::ServiceSpec::new(HARNESS_LABEL, hex_bin)
        .args(["harness", "serve"])
        .env("HEX_DIR", hex_dir.to_string_lossy().into_owned())
        .env("III_URL", "ws://127.0.0.1:49134")
        .env("PATH", path_env)
        .env("GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND", "file")
        .working_dir(hex_dir)
        .keep_alive(true)
        .run_at_load(true)
        .log_path(log_path);
    // Secrets are NOT baked into the plist. The harness reads
    // $HEX_DIR/.hex/secrets/*.env at serve startup via bootstrap_secrets_env()
    // (called in main() before the worker runtime starts). The plist carries
    // only non-secret config: HEX_DIR, PATH, III_URL, log path.
    spec
}

/// Load every `*.env` file from `$HEX_DIR/.hex/secrets/` into the process
/// environment. Called at `hex harness serve` startup, before any thread is
/// spawned. Follows symlinks (metadata()) so symlinked secrets files work.
///
/// # Safety
/// Must be called before any thread is spawned. std::env::set_var is unsound
/// in a multi-threaded context.
#[allow(unsafe_code)]
fn bootstrap_secrets_env(hex_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let dir = hex_dir.join(".hex").join("secrets");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("env") {
            continue;
        }
        // metadata() follows symlinks — symlinked secrets files are intentional.
        let mode = path
            .metadata()
            .map(|m| m.permissions().mode() & 0o077)
            .unwrap_or(0o077);
        if mode != 0 {
            eprintln!(
                "hex harness: skipping {} — unsafe permissions (run: chmod 600 {})",
                path.display(),
                path.display()
            );
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("hex harness: skipping {}: {e}", path.display());
                continue;
            }
        };
        for raw in content.lines() {
            let line = raw.trim().strip_prefix("export").unwrap_or(raw.trim()).trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                let v = v.trim().trim_matches('\'').trim_matches('"');
                if !k.is_empty() {
                    // SAFETY: called before worker runtime spawns threads.
                    unsafe { std::env::set_var(k, v) };
                    eprintln!("hex harness: loaded {k} from {}", path.display());
                }
            }
        }
    }
}

/// `hex harness start` — install (idempotent) and load the per-user service.
///
/// On macOS this is a gui-domain LaunchAgent (NOT a system daemon) because the
/// harness spawns `claude` for per-task reasoning, and Claude Code auth lives
/// in the LOGIN keychain — reachable only from a login session. On Linux it is
/// a `systemd --user` unit. daemon-green owns the launchctl plumbing
/// (bootstrap/kickstart, asuser fallback, wait-out-bootout retry).
fn harness_start() -> i32 {
    let hex_dir = get_hex_dir();
    // launchd / systemd won't create the log dir for us.
    let _ = std::fs::create_dir_all(hex_dir.join(".hex").join("logs"));
    let spec = build_harness_spec(&hex_dir);
    let mgr = daemon_green::native();
    if let Err(e) = mgr.install(&spec) {
        eprintln!("hex harness start: install failed: {e}");
        return 1;
    }
    match mgr.start(HARNESS_LABEL) {
        Ok(()) => {
            eprintln!("hex harness start: {HARNESS_LABEL} loaded");
            0
        }
        Err(e) => {
            eprintln!("hex harness start: start failed: {e}");
            1
        }
    }
}

/// `hex harness stop` — stop + unload the per-user service via daemon-green.
fn harness_stop() -> i32 {
    let mgr = daemon_green::native();
    match mgr.stop(HARNESS_LABEL) {
        Ok(()) => {
            eprintln!("hex harness stop: {HARNESS_LABEL} stopped");
            0
        }
        Err(e) => {
            eprintln!("hex harness stop: {e}");
            1
        }
    }
}

/// `hex harness restart` — restart the per-user service (e.g. to pick up a new
/// binary) via daemon-green.
fn harness_restart() -> i32 {
    let mgr = daemon_green::native();
    match mgr.restart(HARNESS_LABEL) {
        Ok(()) => {
            eprintln!("hex harness restart: {HARNESS_LABEL} restarted");
            0
        }
        Err(e) => {
            eprintln!("hex harness restart: {e}");
            1
        }
    }
}

/// `hex harness logs` — tail the last N lines of the service's combined log.
fn harness_logs(lines: usize) -> i32 {
    let mgr = daemon_green::native();
    match mgr.logs(HARNESS_LABEL, lines) {
        Ok(s) => {
            print!("{s}");
            if !s.ends_with('\n') {
                println!();
            }
            0
        }
        Err(e) => {
            eprintln!("hex harness logs: {e}");
            1
        }
    }
}

/// `hex harness status` — print registered workers + engine health.
fn harness_status() -> i32 {
    let workers = hex::workers::registry();
    println!("Registered workers ({}):", workers.len());
    for w in &workers {
        println!("  - {} ({} handler(s))", w.name, w.handlers.len());
    }
    let ctx = doctor::check::Context {
        hex_dir: get_hex_dir(),
        home: PathBuf::from(std::env::var("HOME").unwrap_or_default()),
        fix: false,
    };
    use doctor::check::DoctorCheck;
    let result = doctor::checks::iii_engine_health::IiiEngineHealth.run(&ctx);
    println!("iii engine: {:?} — {}", result.status, result.message);
    match result.status {
        doctor::check::Status::Pass | doctor::check::Status::Skip => 0,
        _ => 1,
    }
}

fn parse_since(s: &str) -> Result<chrono::Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty --since value".into());
    }
    let (num_str, unit) = match s.chars().last().unwrap() {
        'h' | 'd' => (&s[..s.len() - 1], s.chars().last().unwrap()),
        _ => (s, 'h'),
    };
    let n: i64 = num_str
        .parse()
        .map_err(|e| format!("invalid --since number `{num_str}`: {e}"))?;
    Ok(match unit {
        'd' => chrono::Duration::days(n),
        _ => chrono::Duration::hours(n),
    })
}

fn print_event_table(rows: &[telemetry::EventRow]) {
    println!(
        "{:<25} {:<16} {:<32} {:<8} {:>8}",
        "TS", "SOURCE", "EVENT", "STATUS", "DUR_MS"
    );
    for r in rows {
        let dur = r
            .duration_ms
            .map(|d| d.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<25} {:<16} {:<32} {:<8} {:>8}",
            r.ts, r.source, r.event, r.status, dur
        );
    }
}

fn print_event_json(rows: &[telemetry::EventRow]) {
    let items: Vec<_> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "ts": r.ts,
                "source": r.source,
                "event": r.event,
                "status": r.status,
                "duration_ms": r.duration_ms,
                "exit_code": r.exit_code,
                "detail": r.detail,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&items).unwrap());
}

fn run_messages(command: MessagesCommands) -> i32 {
    let hex_dir = get_hex_dir();
    let db = hex::memory::db_path(&hex_dir);
    let conn = match hex::memory::open_db(&db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hex messages: cannot open {}: {e}", db.display());
            return 1;
        }
    };
    let event = match &command {
        MessagesCommands::Submit { text } => hex::messages::build_submit_event(text),
        MessagesCommands::Reply {
            question_id,
            selection,
            text,
        } => {
            let ids: Vec<&str> = selection
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            hex::messages::build_reply_event(question_id, &ids, text.clone())
        }
    };
    match hex::harness::submit(&conn, &event, hex::worker::run::run_worker) {
        Ok(r) => {
            if let Some(p) = &r.prompt {
                println!("hex asks (question {}): {}", p.id, p.text);
                for o in &p.options {
                    println!("  [{}] {} — {}", o.id, o.label, o.description);
                }
                println!("(reply: hex messages reply {} <id[,id]> [--text ...])", p.id);
            } else {
                println!("{}", r.output);
            }
            0
        }
        Err(e) => {
            eprintln!("hex messages: {e}");
            1
        }
    }
}

fn run_telemetry(command: TelemetryCommands) -> i32 {
    match command {
        TelemetryCommands::Recent { limit, json } => match telemetry::recent(limit) {
            Ok(rows) => {
                if json {
                    print_event_json(&rows);
                } else {
                    print_event_table(&rows);
                }
                0
            }
            Err(e) => {
                eprintln!("telemetry recent: {e}");
                1
            }
        },
        TelemetryCommands::Failures { since, json } => {
            let dur = match parse_since(&since) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("telemetry failures: {e}");
                    return 2;
                }
            };
            let cutoff = chrono::Utc::now() - dur;
            match telemetry::failures(cutoff) {
                Ok(rows) => {
                    if json {
                        print_event_json(&rows);
                    } else {
                        print_event_table(&rows);
                    }
                    0
                }
                Err(e) => {
                    eprintln!("telemetry failures: {e}");
                    1
                }
            }
        }
        TelemetryCommands::Status { json } => match telemetry::status() {
            Ok(rows) => {
                if json {
                    let items: Vec<_> = rows
                        .iter()
                        .map(|r| {
                            serde_json::json!({
                                "event": r.event,
                                "last_ts": r.last_ts,
                                "last_status": r.last_status,
                                "last_duration_ms": r.last_duration_ms,
                                "run_count": r.run_count,
                                "ok_count": r.ok_count,
                                "error_count": r.error_count,
                            })
                        })
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&items).unwrap());
                } else {
                    println!(
                        "{:<32} {:<25} {:<8} {:>5} {:>5} {:>5} {:>8}",
                        "EVENT", "LAST_TS", "LAST", "RUNS", "OK", "ERR", "LAST_MS"
                    );
                    for r in &rows {
                        let dur = r
                            .last_duration_ms
                            .map(|d| d.to_string())
                            .unwrap_or_else(|| "-".into());
                        println!(
                            "{:<32} {:<25} {:<8} {:>5} {:>5} {:>5} {:>8}",
                            r.event,
                            r.last_ts,
                            r.last_status,
                            r.run_count,
                            r.ok_count,
                            r.error_count,
                            dur
                        );
                    }
                }
                0
            }
            Err(e) => {
                eprintln!("telemetry status: {e}");
                1
            }
        },
        TelemetryCommands::Record {
            source,
            event,
            status,
            duration_ms,
            exit_code,
            detail,
        } => {
            let ev = telemetry::TelemetryEvent {
                source,
                event,
                status,
                duration_ms,
                exit_code,
                detail,
            };
            match telemetry::record(&ev) {
                Ok(()) => {
                    println!("recorded");
                    0
                }
                Err(e) => {
                    eprintln!("telemetry record: failed: {e}");
                    1
                }
            }
        }
        TelemetryCommands::Prune { keep_days } => match telemetry::prune(keep_days) {
            Ok(n) => {
                println!("pruned {} events (kept last {}d)", n, keep_days);
                0
            }
            Err(e) => {
                eprintln!("telemetry prune: {e}");
                1
            }
        },
    }
}

fn trigger_summary(w: &hex::worker::Worker) -> String {
    use hex::worker::TriggerSpec::*;
    w.handlers
        .iter()
        .map(|(spec, _)| match spec {
            Cron { expression } => format!("cron({expression})"),
            State { scope, key } => format!("state({scope}/{key})"),
            Queue { queue } => format!("queue({queue})"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn module_source(name: &str) -> (&'static str, String) {
    for (n, path) in hex::workers::hex_modules::module_paths() {
        if n == name {
            let kind = if path.contains("/src/modules/") {
                "core"
            } else if path.contains("/.hex/modules/") {
                "personal"
            } else {
                "module"
            };
            return (kind, path.to_string());
        }
    }
    ("builtin", String::new())
}

/// `hex module enable|disable <name>` — mutate the disabled store. The name
/// must exist in THIS binary's registry (typo protection; a personal module
/// missing from a non-overlay build errors here rather than silently storing
/// a name nothing honors). Idempotent; takes effect at the module's next fire.
fn module_set_enabled(name: &str, enable: bool) -> i32 {
    let verb = if enable { "enable" } else { "disable" };
    if !hex::workers::registry().iter().any(|w| w.name == name) {
        eprintln!(
            "hex module {verb}: no worker named '{name}' in this binary's registry (see: hex module list)"
        );
        return 1;
    }
    let hex_dir = get_hex_dir();
    match hex::module_state::set_disabled(&hex_dir, name, !enable) {
        Ok(true) => {
            println!(
                "hex module {verb}: '{name}' {} (effective at its next fire; no restart needed)",
                if enable { "enabled" } else { "disabled" }
            );
            0
        }
        Ok(false) => {
            println!(
                "hex module {verb}: '{name}' already {}",
                if enable { "enabled" } else { "disabled" }
            );
            0
        }
        Err(e) => {
            eprintln!("hex module {verb}: {e} — fix or delete the state db first");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap_complete::Shell;

    #[test]
    fn completions_zsh_nonempty_and_contains_hex() {
        let mut buf = Vec::new();
        clap_complete::generate(Shell::Zsh, &mut Cli::command(), "hex", &mut buf);
        assert!(!buf.is_empty(), "zsh completions must not be empty");
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("_hex"),
            "zsh completions must contain '_hex', got: {}",
            &output[..200.min(output.len())]
        );
    }
}
