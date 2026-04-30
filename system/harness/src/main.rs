use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

use hex::{state, wake};

#[derive(Parser)]
#[command(name = "hex", about = "Hex multi-agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Agent fleet management
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },
    /// HTTP/SSE server
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },
    /// Asset registry
    Asset {
        #[command(subcommand)]
        command: AssetCommands,
    },
    /// Unified messaging (comments, agent messages, notifications)
    Message {
        #[command(subcommand)]
        command: MessageCommands,
    },
    /// Event engine
    Events {
        #[command(subcommand)]
        command: EventsCommands,
    },
    /// SSE bus operations
    Sse {
        #[command(subcommand)]
        command: SseCommands,
    },
    /// Integration bundle lifecycle management
    Integration {
        #[command(subcommand)]
        command: IntegrationCommands,
    },
    /// Behavioral and indexed memory operations
    Memory {
        #[command(subcommand)]
        command: MemoryCommands,
    },
    /// Extension management
    Extension {
        #[command(subcommand)]
        command: ExtensionCommands,
    },
    /// System health check
    Doctor {
        #[arg(long)]
        fix: bool,
        #[arg(long)]
        smoke: bool,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print version
    Version,
}

#[derive(Subcommand)]
enum AgentCommands {
    /// Run an agent wake cycle (shift)
    Wake {
        agent_id: String,
        #[arg(long, default_value = "manual")]
        trigger: String,
        #[arg(long, default_value = "{}")]
        payload: String,
    },
    /// Show agent status
    Status { agent_id: Option<String> },
    /// Show fleet overview
    Fleet,
    /// Send async message to another agent
    Message {
        from: String,
        to: String,
        #[arg(long)]
        subject: String,
        #[arg(long)]
        body: String,
        #[arg(long)]
        initiative: Option<String>,
        #[arg(long)]
        response_requested: bool,
    },
    /// List agent IDs (one per line, machine-readable)
    List {
        #[arg(long)]
        core: bool,
    },
    /// Check core agents against reference set
    CheckCore,
    /// Restore missing core agents from reference (never overwrites existing)
    RestoreCore,
    /// Query audit trail
    Audit {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        action: Option<String>,
        #[arg(long)]
        since: Option<String>,
    },
    /// Show cost data
    Cost {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        period: Option<String>,
    },
}

#[derive(Subcommand)]
enum ServerCommands {
    /// Start the HTTP/SSE server
    Start {
        #[arg(long, default_value = "8880")]
        port: u16,
    },
    /// Check if the server is running
    Health,
}

#[derive(Subcommand)]
enum AssetCommands {
    /// Resolve asset by type:local_id
    Resolve { id: String },
    /// List assets
    List {
        #[arg(long)]
        r#type: Option<String>,
    },
    /// Search assets
    Search { query: String },
    /// Register an asset
    Register {
        #[arg(long)]
        r#type: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        path: Option<String>,
    },
    /// List asset types with counts
    Types,
}

#[derive(Subcommand)]
enum MessageCommands {
    /// Send a message
    Send {
        from: String,
        to: Vec<String>,
        #[arg(long)]
        content: String,
        #[arg(long, default_value = "agent")]
        msg_type: String,
        #[arg(long)]
        anchor: Option<String>,
    },
    /// List messages
    List {
        #[arg(long)]
        msg_type: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        anchor: Option<String>,
    },
    /// Update message status / action log
    Respond {
        id: String,
        status: String,
        action: Option<String>,
        #[arg(long)]
        assets: Vec<String>,
    },
}

#[derive(Subcommand)]
enum EventsCommands {
    /// Show event engine status
    Status,
    /// Emit an event
    Emit {
        event_type: String,
        payload: String,
    },
    /// Show full action chain for an event
    Trace {
        event_id: i64,
    },
    /// List loaded policies
    Policies,
    /// Force policy reload
    Reload,
}

#[derive(Subcommand)]
enum SseCommands {
    /// Publish an SSE event
    Publish {
        topic: String,
        r#type: String,
        payload: String,
    },
    /// List registered SSE topics
    Topics,
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
}

#[derive(Subcommand)]
enum MemoryCommands {
    /// Query behavioral memory for relevant corrections
    CheckBehavior { query: String },
    /// Store a behavioral correction
    Store {
        text: String,
        #[arg(long)]
        rule: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Bootstrap memory from feedback files
    Bootstrap,
    /// Show memory health stats
    Health,
    /// Search indexed memory files
    Search {
        query: String,
        #[arg(long)]
        top: Option<usize>,
    },
    /// Index memory files
    Index {
        #[arg(long)]
        full: bool,
    },
}

#[derive(Subcommand)]
enum ExtensionCommands {
    /// List installed extensions
    List,
    /// Validate an extension manifest
    Validate { path: PathBuf },
    /// Show full manifest for an extension
    Info { name: String },
    /// Enable a disabled extension
    Enable { name: String },
    /// Disable an installed extension
    Disable { name: String },
}

/// Parse a single top-level `key: value` from raw YAML text (no nesting).
fn yaml_get<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{}: ", key);
    text.lines()
        .find(|l| l.starts_with(&prefix))
        .map(|l| l[prefix.len()..].trim_matches('"').trim_matches('\'').trim())
}

/// Scan extension dirs and return (path, enabled) pairs.
fn scan_extension_dirs(hex_dir: &Path) -> Vec<(PathBuf, bool)> {
    let search_dirs = [
        hex_dir.join("extensions"),
        hex_dir.join(".hex/extensions"),
    ];
    let mut results = Vec::new();
    for base in &search_dirs {
        let entries = match std::fs::read_dir(base) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            // .disabled suffix marks disabled extensions
            let enabled = !name.ends_with(".disabled");
            // Only consider dirs that contain extension.yaml (or would after enabling)
            let manifest = if enabled {
                path.join("extension.yaml")
            } else {
                path.join("extension.yaml")
            };
            if manifest.exists() {
                results.push((path, enabled));
            }
        }
    }
    results
}

fn run_extension_command(command: ExtensionCommands) {
    let hex_dir = get_hex_dir();
    match command {
        ExtensionCommands::List => {
            let exts = scan_extension_dirs(&hex_dir);
            if exts.is_empty() {
                println!("No extensions found.");
                return;
            }
            println!("{:<24} {:<10} {:<10} {}", "NAME", "VERSION", "TYPE", "STATUS");
            println!("{}", "-".repeat(60));
            for (path, enabled) in &exts {
                let manifest_path = path.join("extension.yaml");
                let text = std::fs::read_to_string(&manifest_path).unwrap_or_default();
                let name = yaml_get(&text, "name").unwrap_or("?").to_string();
                let version = yaml_get(&text, "version").unwrap_or("?").to_string();
                let ext_type = yaml_get(&text, "type").unwrap_or("?").to_string();
                let status = if *enabled { "enabled" } else { "disabled" };
                println!("{:<24} {:<10} {:<10} {}", name, version, ext_type, status);
            }
            println!("\n{} extension(s)", exts.len());
        }

        ExtensionCommands::Validate { path } => {
            let script = hex_dir.join(".hex/scripts/extension-validate.py");
            let status = std::process::Command::new("python3")
                .arg(&script)
                .arg(&path)
                .env("HEX_DIR", &hex_dir)
                .status()
                .unwrap_or_else(|e| {
                    eprintln!("hex extension validate: failed to run validator: {e}");
                    std::process::exit(1);
                });
            std::process::exit(status.code().unwrap_or(1));
        }

        ExtensionCommands::Info { name } => {
            let exts = scan_extension_dirs(&hex_dir);
            let found = exts.iter().find(|(path, _)| {
                let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let bare = dir_name.trim_end_matches(".disabled");
                // Match by directory name or by manifest name field
                bare == name || {
                    let text = std::fs::read_to_string(path.join("extension.yaml")).unwrap_or_default();
                    yaml_get(&text, "name").unwrap_or("") == name
                }
            });
            match found {
                Some((path, enabled)) => {
                    let manifest_path = path.join("extension.yaml");
                    let text = std::fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
                        eprintln!("Cannot read extension manifest: {e}");
                        std::process::exit(1);
                    });
                    let status = if *enabled { "enabled" } else { "disabled" };
                    println!("# Extension: {} ({})\n", name, status);
                    println!("{}", text);
                }
                None => {
                    eprintln!("Extension '{}' not found.", name);
                    std::process::exit(1);
                }
            }
        }

        ExtensionCommands::Disable { name } => {
            let exts = scan_extension_dirs(&hex_dir);
            let found = exts.iter().find(|(path, enabled)| {
                if !enabled {
                    return false;
                }
                let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if dir_name == name {
                    return true;
                }
                let text = std::fs::read_to_string(path.join("extension.yaml")).unwrap_or_default();
                yaml_get(&text, "name").unwrap_or("") == name
            });
            match found {
                Some((path, _)) => {
                    let disabled_path = {
                        let parent = path.parent().unwrap_or(path);
                        let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        parent.join(format!("{}.disabled", dir_name))
                    };
                    std::fs::rename(path, &disabled_path).unwrap_or_else(|e| {
                        eprintln!("Cannot disable extension '{}': {e}", name);
                        std::process::exit(1);
                    });
                    println!("Extension '{}' disabled.", name);
                }
                None => {
                    eprintln!("Extension '{}' not found or already disabled.", name);
                    std::process::exit(1);
                }
            }
        }

        ExtensionCommands::Enable { name } => {
            // Find a .disabled directory matching the name
            let search_dirs = [
                hex_dir.join("extensions"),
                hex_dir.join(".hex/extensions"),
            ];
            let mut found_path: Option<PathBuf> = None;
            'outer: for base in &search_dirs {
                let entries = match std::fs::read_dir(base) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    if !dir_name.ends_with(".disabled") {
                        continue;
                    }
                    let bare = dir_name.trim_end_matches(".disabled");
                    let manifest = path.join("extension.yaml");
                    let manifest_name = if manifest.exists() {
                        let text = std::fs::read_to_string(&manifest).unwrap_or_default();
                        yaml_get(&text, "name").unwrap_or("").to_string()
                    } else {
                        String::new()
                    };
                    if bare == name || manifest_name == name {
                        found_path = Some(path);
                        break 'outer;
                    }
                }
            }
            match found_path {
                Some(path) => {
                    let enabled_path = {
                        let parent = path.parent().unwrap_or(&path);
                        let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        parent.join(dir_name.trim_end_matches(".disabled"))
                    };
                    std::fs::rename(&path, &enabled_path).unwrap_or_else(|e| {
                        eprintln!("Cannot enable extension '{}': {e}", name);
                        std::process::exit(1);
                    });
                    println!("Extension '{}' enabled.", name);
                }
                None => {
                    eprintln!("No disabled extension '{}' found.", name);
                    std::process::exit(1);
                }
            }
        }
    }
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

/// Discover all agents by scanning projects/*/charter.yaml.
/// Charter file IS the registration. No hardcoded lists.
fn discover_agents(hex_dir: &Path) -> Vec<String> {
    let projects_dir = hex_dir.join("projects");
    let mut agents: Vec<String> = Vec::new();
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "ERROR: cannot read projects directory {}: {e}",
                projects_dir.display()
            );
            std::process::exit(1);
        }
    };
    for entry in entries {
        match entry {
            Ok(e) => {
                if e.path().join("charter.yaml").exists() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !is_safe_agent_id(&name) {
                        eprintln!(
                            "ERROR: agent directory '{}' contains unsafe characters — skipping",
                            name
                        );
                        continue;
                    }
                    agents.push(name);
                }
            }
            Err(e) => {
                eprintln!("WARN: cannot read entry in {}: {e}", projects_dir.display());
            }
        }
    }
    agents.sort();
    agents
}

fn is_safe_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !id.contains("..")
}

fn run_agent_command(command: AgentCommands) {
    match command {
        AgentCommands::Wake {
            agent_id,
            trigger,
            payload,
        } => {
            let hex_dir = get_hex_dir();
            match wake::run(wake::WakeConfig {
                hex_dir,
                agent_id: agent_id.clone(),
                trigger,
                payload,
            }) {
                Ok(code) => {
                    let home = std::env::var("HOME").unwrap_or_default();
                    let halt_path = format!("{}/.hex-{}-HALT-loop", home, agent_id);
                    if std::path::Path::new(&halt_path).exists() {
                        eprintln!(
                            "[{}] WARNING: loop.detected — HALT-loop file present, agent halted pending review",
                            agent_id
                        );
                    }
                    std::process::exit(code)
                }
                Err(e) => {
                    eprintln!("wake failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        AgentCommands::Status { agent_id } => {
            let hex_dir = get_hex_dir();
            if let Some(id) = agent_id {
                let state_path = hex_dir.join(format!("projects/{}/state.json", id));
                match state::load(&state_path) {
                    Ok(s) => {
                        println!("Agent: {}", s.agent_id);
                        println!("Wakes: {}", s.wake_count);
                        println!(
                            "Last wake: {}",
                            s.last_wake
                                .map(|t| t.to_rfc3339())
                                .unwrap_or("never".into())
                        );
                        println!("Active queue: {} items", s.queue.active.len());
                        println!("Blocked: {} items", s.queue.blocked.len());
                        println!("Scheduled: {} items", s.queue.scheduled.len());
                        println!("Inbox: {} messages", s.inbox.len());
                        println!("Trail: {} entries", s.trail.len());
                        println!("Cost (lifetime): ${:.4}", s.cost.lifetime_usd);
                        println!(
                            "Cost (period): ${:.4} / ${:.2}",
                            s.cost.current_period.spent_usd, s.cost.current_period.budget_usd
                        );
                    }
                    Err(e) => {
                        eprintln!("Cannot load state for '{}': {e}", id);
                        std::process::exit(1);
                    }
                }
            } else {
                eprintln!("Usage: hex agent status <agent-id>");
                std::process::exit(1);
            }
        }
        AgentCommands::Fleet => {
            let hex_dir = get_hex_dir();
            let agents = discover_agents(&hex_dir);

            if agents.is_empty() {
                eprintln!("ERROR: no agents found — no projects/*/charter.yaml files exist");
                std::process::exit(1);
            }

            let mut errors: Vec<String> = Vec::new();
            let mut charters: std::collections::HashMap<String, hex::types::Charter> =
                std::collections::HashMap::new();

            for id in &agents {
                let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", id));
                match hex::charter::load(&charter_path) {
                    Ok(c) => {
                        if c.id != *id {
                            errors.push(format!(
                                "ERROR: agent '{}' charter.id is '{}' — must match directory name exactly",
                                id, c.id
                            ));
                        }
                        charters.insert(id.clone(), c);
                    }
                    Err(e) => {
                        errors.push(format!("ERROR: agent '{}' has invalid charter: {e}", id));
                    }
                }
            }

            if !errors.is_empty() {
                for err in &errors {
                    eprintln!("{}", err);
                }
                std::process::exit(1);
            }

            println!(
                "{:<20} {:>4} {:>6} {:>12} {:>8} {:>8} {:>10}",
                "AGENT", "CORE", "WAKES", "LAST WAKE", "ACTIVE", "BLOCKED", "COST/DAY"
            );
            println!("{}", "-".repeat(74));
            for id in &agents {
                let is_core = charters.get(id).map(|c| c.core).unwrap_or(false);
                let core_flag = if is_core { "  ●" } else { "" };
                let state_path = hex_dir.join(format!("projects/{}/state.json", id));
                if let Ok(s) = state::load(&state_path) {
                    let last = s
                        .last_wake
                        .map(|t| t.format("%H:%M:%S").to_string())
                        .unwrap_or("never".into());
                    println!(
                        "{:<20} {:>4} {:>6} {:>12} {:>8} {:>8} ${:>9.4}",
                        id,
                        core_flag,
                        s.wake_count,
                        last,
                        s.queue.active.len(),
                        s.queue.blocked.len(),
                        s.cost.current_period.spent_usd
                    );
                } else {
                    println!(
                        "{:<20} {:>4} {:>6} {:>12} {:>8} {:>8} {:>10}",
                        id, core_flag, 0, "never", 0, 0, "new"
                    );
                }
            }

            println!("\n{} agents", agents.len());

            let core_agents: Vec<&String> = agents
                .iter()
                .filter(|id| charters.get(*id).map(|c| c.core).unwrap_or(false))
                .collect();
            if !core_agents.is_empty() {
                let mut core_warnings: Vec<String> = Vec::new();
                for id in &core_agents {
                    let kill_switch = charters
                        .get(*id)
                        .map(|c| shellexpand::tilde(&c.kill_switch).to_string())
                        .unwrap_or_default();
                    if !kill_switch.is_empty() && Path::new(&kill_switch).exists() {
                        core_warnings.push(format!(
                            "WARN: core agent '{}' is HALTED — system self-healing may be degraded",
                            id
                        ));
                    }
                }
                if !core_warnings.is_empty() {
                    eprintln!();
                    for w in &core_warnings {
                        eprintln!("{}", w);
                    }
                }
            }
        }
        AgentCommands::List { core } => {
            let hex_dir = get_hex_dir();
            let agents = discover_agents(&hex_dir);
            for id in &agents {
                if core {
                    let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", id));
                    if let Ok(c) = hex::charter::load(&charter_path) {
                        if !c.core {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
                println!("{}", id);
            }
        }
        AgentCommands::CheckCore => {
            let hex_dir = get_hex_dir();
            let ref_dir = hex_dir.join(".hex/reference/core-agents");
            if !ref_dir.exists() {
                eprintln!("ERROR: no reference core agents at {}", ref_dir.display());
                std::process::exit(1);
            }
            let mut missing: Vec<String> = Vec::new();
            let mut broken: Vec<String> = Vec::new();
            let mut ok: Vec<String> = Vec::new();
            let entries = match std::fs::read_dir(&ref_dir) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!(
                        "ERROR: cannot read reference directory {}: {e}",
                        ref_dir.display()
                    );
                    std::process::exit(1);
                }
            };
            {
                for entry in entries {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            eprintln!("WARN: cannot read reference entry: {e}");
                            continue;
                        }
                    };
                    let fname = entry.file_name().to_string_lossy().to_string();
                    if !fname.ends_with(".yaml") {
                        continue;
                    }
                    let agent_id = fname.trim_end_matches(".yaml").to_string();
                    let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", agent_id));
                    if !charter_path.exists() {
                        missing.push(agent_id);
                    } else {
                        match hex::charter::load(&charter_path) {
                            Ok(c) => {
                                if !c.core {
                                    broken.push(format!(
                                        "{} (exists but core: false — should be core: true)",
                                        agent_id
                                    ));
                                } else if c.id != agent_id {
                                    broken.push(format!(
                                        "{} (charter.id '{}' doesn't match directory)",
                                        agent_id, c.id
                                    ));
                                } else {
                                    ok.push(agent_id);
                                }
                            }
                            Err(e) => {
                                broken.push(format!("{} (invalid charter: {})", agent_id, e));
                            }
                        }
                    }
                }
            }
            let total = ok.len() + missing.len() + broken.len();
            println!("Core agents: {}/{} healthy", ok.len(), total);
            for id in &ok {
                println!("  ✓ {}", id);
            }
            if !missing.is_empty() {
                println!();
                for id in &missing {
                    println!("  MISSING: {} — not found in projects/", id);
                }
            }
            if !broken.is_empty() {
                println!();
                for desc in &broken {
                    println!("  BROKEN: {}", desc);
                }
            }
            if !missing.is_empty() || !broken.is_empty() {
                println!();
                println!("Run 'hex agent restore-core' to fix missing core agents.");
                std::process::exit(1);
            }
        }
        AgentCommands::RestoreCore => {
            let hex_dir = get_hex_dir();
            let ref_dir = hex_dir.join(".hex/reference/core-agents");
            if !ref_dir.exists() {
                eprintln!("ERROR: no reference core agents at {}", ref_dir.display());
                std::process::exit(1);
            }
            let mut restored = 0;
            let mut skipped = 0;
            let mut failed = 0;
            let entries = match std::fs::read_dir(&ref_dir) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!(
                        "ERROR: cannot read reference directory {}: {e}",
                        ref_dir.display()
                    );
                    std::process::exit(1);
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("  ERROR: cannot read reference entry: {e}");
                        failed += 1;
                        continue;
                    }
                };
                let fname = entry.file_name().to_string_lossy().to_string();
                if !fname.ends_with(".yaml") {
                    continue;
                }
                let agent_id = fname.trim_end_matches(".yaml").to_string();
                let target_dir = hex_dir.join(format!("projects/{}", agent_id));
                let target_charter = target_dir.join("charter.yaml");
                if target_charter.exists() {
                    println!(
                        "  SKIP: {} — charter already exists (not overwriting)",
                        agent_id
                    );
                    skipped += 1;
                    continue;
                }
                if let Err(e) = std::fs::create_dir_all(&target_dir) {
                    eprintln!("  ERROR: cannot create {}: {e}", target_dir.display());
                    failed += 1;
                    continue;
                }
                match std::fs::copy(entry.path(), &target_charter) {
                    Ok(_) => {
                        println!("  RESTORED: {} — charter created from reference", agent_id);
                        restored += 1;
                    }
                    Err(e) => {
                        eprintln!("  ERROR: cannot copy charter for {}: {e}", agent_id);
                        failed += 1;
                    }
                }
            }
            println!();
            if restored > 0 {
                println!(
                    "Restored {} core agent(s). Run 'hex agent fleet' to verify.",
                    restored
                );
            } else if skipped > 0 {
                println!("All core agents already present ({} checked).", skipped);
            } else {
                println!("No reference charters found.");
            }
            if failed > 0 {
                eprintln!("ERROR: {} operation(s) failed during restore", failed);
                std::process::exit(1);
            }
        }
        AgentCommands::Message {
            from,
            to,
            subject,
            body,
            initiative,
            response_requested,
        } => {
            let hex_dir = get_hex_dir();
            let bus = hex::sse::SseBus::new();
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let handler = hex::messaging::MessagingHandler::new(&hex_dir, bus, telemetry);
            let content = format!("[{}] {}", subject, body);
            handler.cli_send(&from, vec![to.clone()], &content, "agent", initiative.as_deref());
            if response_requested {
                let audit_dir = hex_dir.join(".hex/audit");
                wake::auto_wake_target(&hex_dir, &to, &from, &audit_dir);
                println!("Auto-waking {} for live response", to);
            }
        }
        AgentCommands::Audit { agent, .. } => {
            eprintln!("audit: {:?} (not yet implemented)", agent);
            std::process::exit(1);
        }
        AgentCommands::Cost { agent, .. } => {
            eprintln!("cost: {:?} (not yet implemented)", agent);
            std::process::exit(1);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let binary_name = Path::new(&args[0])
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let effective_args = if binary_name == "hex-agent" {
        let mut new_args = vec![args[0].clone(), "agent".to_string()];
        new_args.extend(args[1..].to_vec());
        new_args
    } else {
        args
    };
    let cli = Cli::parse_from(effective_args);

    match cli.command {
        Commands::Agent { command } => run_agent_command(command),
        Commands::Extension { command } => run_extension_command(command),
        Commands::Server { command } => match command {
            ServerCommands::Start { port } => {
                let hex_dir = get_hex_dir();
                let bus = hex::sse::SseBus::new();
                let topics_dir = hex_dir.join("system/sse/topics");
                bus.load_manifests(&topics_dir);
                let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
                let events = hex::events::EventEngine::new(
                    &hex_dir,
                    std::sync::Arc::clone(&telemetry),
                    std::sync::Arc::clone(&bus),
                ).unwrap_or_else(|e| {
                    eprintln!("hex server: events engine init failed: {e}");
                    std::process::exit(1);
                });
                let messaging = hex::messaging::MessagingHandler::new(
                    &hex_dir,
                    std::sync::Arc::clone(&bus),
                    std::sync::Arc::clone(&telemetry),
                );
                let assets = hex::assets::AssetsHandler::new(
                    &hex_dir,
                    std::sync::Arc::clone(&bus),
                    std::sync::Arc::clone(&telemetry),
                );
                let ext_db = hex::extensions::ExtensionDb::open(&hex_dir)
                    .unwrap_or_else(|e| {
                        eprintln!("hex server: extension db init failed: {e}");
                        std::process::exit(1);
                    });
                ext_db.scan_and_migrate(&hex_dir);
                let server = hex::server::HexServer::new(port, hex_dir, bus, telemetry, events, messaging, assets, ext_db);
                server.start();
            }
            ServerCommands::Health => {
                let port = 8880u16;
                if hex::server::HexServer::check_health(port) {
                    println!("hex server is running on port {}", port);
                } else {
                    eprintln!("hex server is not running on port {}", port);
                    std::process::exit(1);
                }
            }
        },
        Commands::Asset { command } => {
            let hex_dir = get_hex_dir();
            let bus = hex::sse::SseBus::new();
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let handler = hex::assets::AssetsHandler::new(&hex_dir, bus, telemetry);
            match command {
                AssetCommands::Resolve { id } => handler.cli_resolve(&id),
                AssetCommands::List { r#type } => handler.cli_list(r#type.as_deref()),
                AssetCommands::Search { query } => handler.cli_search(&query),
                AssetCommands::Register { r#type, id, title, path } => {
                    handler.cli_register(&r#type, &id, &title, path.as_deref())
                }
                AssetCommands::Types => handler.cli_types(),
            }
        }
        Commands::Message { command } => {
            let hex_dir = get_hex_dir();
            let bus = hex::sse::SseBus::new();
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let handler = hex::messaging::MessagingHandler::new(&hex_dir, bus, telemetry);
            match command {
                MessageCommands::Send { from, to, content, msg_type, anchor } => {
                    handler.cli_send(&from, to, &content, &msg_type, anchor.as_deref());
                }
                MessageCommands::List { msg_type, status, anchor } => {
                    handler.cli_list(msg_type.as_deref(), status.as_deref(), anchor.as_deref());
                }
                MessageCommands::Respond { id, status, action, assets } => {
                    handler.cli_respond(&id, &status, action.as_deref(), assets);
                }
            }
        }
        Commands::Events { command } => {
            let hex_dir = get_hex_dir();
            let bus = hex::sse::SseBus::new();
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let engine = hex::events::EventEngine::new(&hex_dir, telemetry, bus)
                .unwrap_or_else(|e| {
                    eprintln!("events engine init failed: {e}");
                    std::process::exit(1);
                });
            match command {
                EventsCommands::Status => engine.cli_status(),
                EventsCommands::Emit { event_type, payload } => engine.cli_emit(&event_type, &payload),
                EventsCommands::Trace { event_id } => engine.cli_trace(event_id),
                EventsCommands::Policies => engine.cli_policies(),
                EventsCommands::Reload => engine.cli_reload(),
            }
        }
        Commands::Sse { command } => match command {
            SseCommands::Publish { topic, r#type, payload } => {
                eprintln!(
                    "hex sse publish {} {} {} (not yet implemented)",
                    topic, r#type, payload
                );
                std::process::exit(1);
            }
            SseCommands::Topics => {
                eprintln!("hex sse topics (not yet implemented)");
                std::process::exit(1);
            }
        },
        Commands::Integration { command } => {
            let hex_dir = get_hex_dir();
            let script = hex_dir.join(".hex/scripts/hex-integration");
            let (subcmd, name_arg): (&str, Option<String>) = match &command {
                IntegrationCommands::Install { name } => ("install", Some(name.clone())),
                IntegrationCommands::Uninstall { name } => ("uninstall", Some(name.clone())),
                IntegrationCommands::Update { name } => ("update", Some(name.clone())),
                IntegrationCommands::List => ("list", None),
                IntegrationCommands::Validate { name } => ("validate", Some(name.clone())),
                IntegrationCommands::Status { name } => ("status", name.clone()),
                IntegrationCommands::Probe { name } => ("probe", Some(name.clone())),
                IntegrationCommands::Rotate { name } => ("rotate", Some(name.clone())),
            };
            let start = std::time::Instant::now();
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
            let duration_ms = start.elapsed().as_millis() as u64;
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let integration_name = name_arg.as_deref().unwrap_or("(all)");
            telemetry.emit(&format!("hex.integration.{}", subcmd), &serde_json::json!({
                "integration": integration_name,
                "exit_code": exit_code,
                "duration_ms": duration_ms,
            }));
            std::process::exit(exit_code);
        }
        Commands::Memory { command } => {
            let hex_dir = get_hex_dir();
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let subcmd_name = match &command {
                MemoryCommands::CheckBehavior { .. } => "check-behavior",
                MemoryCommands::Store { .. } => "store",
                MemoryCommands::Bootstrap => "bootstrap",
                MemoryCommands::Health => "health",
                MemoryCommands::Search { .. } => "search",
                MemoryCommands::Index { .. } => "index",
            };
            let start = std::time::Instant::now();
            let exit_code = match &command {
                MemoryCommands::Search { query, top } => {
                    let script = hex_dir.join("system/skills/memory/scripts/memory_search.py");
                    let mut cmd = std::process::Command::new("python3");
                    cmd.arg(&script).arg(query);
                    if let Some(t) = top {
                        cmd.arg("--top").arg(t.to_string());
                    }
                    cmd.env("HEX_DIR", &hex_dir);
                    cmd.status().map(|s| s.code().unwrap_or(1)).unwrap_or(1)
                }
                MemoryCommands::Index { full } => {
                    let script = hex_dir.join("system/skills/memory/scripts/memory_index.py");
                    let mut cmd = std::process::Command::new("python3");
                    cmd.arg(&script);
                    if *full {
                        cmd.arg("--full");
                    }
                    cmd.env("HEX_DIR", &hex_dir);
                    cmd.status().map(|s| s.code().unwrap_or(1)).unwrap_or(1)
                }
                _ => {
                    let hex_memory = hex_dir.join(".hex/scripts/bin/hex-memory");
                    let mut cmd = std::process::Command::new("bash");
                    cmd.arg(&hex_memory);
                    match &command {
                        MemoryCommands::CheckBehavior { query } => {
                            cmd.arg("check-behavior").arg(query);
                        }
                        MemoryCommands::Store { text, rule, session } => {
                            cmd.arg("store").arg(text);
                            if let Some(r) = rule {
                                cmd.arg("--rule").arg(r);
                            }
                            if let Some(s) = session {
                                cmd.arg("--session").arg(s);
                            }
                        }
                        MemoryCommands::Bootstrap => {
                            cmd.arg("bootstrap");
                        }
                        MemoryCommands::Health => {
                            cmd.arg("health");
                        }
                        _ => unreachable!(),
                    }
                    cmd.env("HEX_DIR", &hex_dir);
                    cmd.status().map(|s| s.code().unwrap_or(1)).unwrap_or(1)
                }
            };
            let duration_ms = start.elapsed().as_millis() as u64;
            telemetry.emit(
                &format!("hex.memory.{}", subcmd_name),
                &serde_json::json!({
                    "exit_code": exit_code,
                    "duration_ms": duration_ms,
                }),
            );
            std::process::exit(exit_code);
        }
        Commands::Doctor { fix, smoke, quiet, json } => {
            let hex_dir = get_hex_dir();
            let script = hex_dir.join(".hex/scripts/hex-doctor");
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let start = std::time::Instant::now();
            let mut cmd = std::process::Command::new("bash");
            cmd.arg(&script);
            if fix { cmd.arg("--fix"); }
            if smoke { cmd.arg("--smoke"); }
            if quiet { cmd.arg("--quiet"); }
            if json { cmd.arg("--json"); }
            cmd.env("HEX_DIR", &hex_dir);
            let output = cmd.output().unwrap_or_else(|e| {
                eprintln!("hex doctor: failed to run script: {e}");
                std::process::exit(1);
            });
            let exit_code = output.status.code().unwrap_or(1);
            let duration_ms = start.elapsed().as_millis() as u64;
            // Forward stdout/stderr to terminal
            let _ = std::io::Write::write_all(&mut std::io::stdout(), &output.stdout);
            let _ = std::io::Write::write_all(&mut std::io::stderr(), &output.stderr);
            telemetry.emit("hex.doctor.run", &serde_json::json!({
                "fix": fix,
                "smoke": smoke,
                "quiet": quiet,
                "json": json,
                "exit_code": exit_code,
                "duration_ms": duration_ms,
            }));
            if exit_code != 0 {
                let stderr_str = String::from_utf8_lossy(&output.stderr);
                let stdout_str = String::from_utf8_lossy(&output.stdout);
                telemetry.emit("hex.doctor.failed", &serde_json::json!({
                    "exit_code": exit_code,
                    "stdout": stdout_str.chars().take(2000).collect::<String>(),
                    "stderr": stderr_str.chars().take(2000).collect::<String>(),
                }));
            }
            std::process::exit(exit_code);
        }
        Commands::Version => {
            println!("hex {} ({})", env!("HEX_VERSION"), env!("HEX_GIT_SHA"));
        }
    }
}
