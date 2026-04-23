use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

use hex_agent::{state, wake};

#[derive(Parser)]
#[command(name = "hex-agent", about = "Hex multi-agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
    let p = PathBuf::from(&home).join("mrap-hex");
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

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Wake {
            agent_id,
            trigger,
            payload,
        } => {
            match wake::run(wake::WakeConfig {
                hex_dir: get_hex_dir(),
                agent_id,
                trigger,
                payload,
            }) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("wake failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Status { agent_id } => {
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
                eprintln!("Usage: hex-agent status <agent-id>");
                std::process::exit(1);
            }
        }
        Commands::Fleet => {
            let hex_dir = get_hex_dir();
            let agents = discover_agents(&hex_dir);

            if agents.is_empty() {
                eprintln!("ERROR: no agents found — no projects/*/charter.yaml files exist");
                std::process::exit(1);
            }

            let mut errors: Vec<String> = Vec::new();
            let mut charters: std::collections::HashMap<String, hex_agent::types::Charter> =
                std::collections::HashMap::new();

            for id in &agents {
                let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", id));
                match hex_agent::charter::load(&charter_path) {
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

            // Check core agent health
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
        Commands::List { core } => {
            let hex_dir = get_hex_dir();
            let agents = discover_agents(&hex_dir);
            for id in &agents {
                if core {
                    let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", id));
                    if let Ok(c) = hex_agent::charter::load(&charter_path) {
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
        Commands::CheckCore => {
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
                        match hex_agent::charter::load(&charter_path) {
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
                println!("Run 'hex-agent restore-core' to fix missing core agents.");
                std::process::exit(1);
            }
        }
        Commands::RestoreCore => {
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
                    "Restored {} core agent(s). Run 'hex-agent fleet' to verify.",
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
        Commands::Message {
            from,
            to,
            subject,
            body,
            initiative,
            response_requested,
        } => {
            let hex_dir = get_hex_dir();
            let msg_dir = hex_dir.join(".hex/messages");
            let msg = hex_agent::types::Message {
                id: uuid::Uuid::new_v4().to_string(),
                from: from.clone(),
                to: to.clone(),
                subject: subject.clone(),
                body,
                initiative_id: initiative,
                response_requested,
                in_reply_to: None,
                sent_at: chrono::Utc::now(),
            };
            match hex_agent::message::send(&msg_dir, &msg) {
                Ok(()) => println!("Sent message '{}' from {} to {}", subject, from, to),
                Err(e) => {
                    eprintln!("Failed to send message: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Audit { agent, .. } => {
            eprintln!("audit: {:?} (not yet implemented)", agent);
            std::process::exit(1);
        }
        Commands::Cost { agent, .. } => {
            eprintln!("cost: {:?} (not yet implemented)", agent);
            std::process::exit(1);
        }
    }
}
