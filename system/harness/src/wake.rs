use crate::{audit, charter, claude, cost, gate, message, prompt, queue, state};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct WakeConfig {
    pub hex_dir: PathBuf,
    pub agent_id: String,
    pub trigger: String,
    pub payload: String,
}

pub fn run(config: WakeConfig) -> Result<i32, Box<dyn std::error::Error>> {
    let hex_dir = &config.hex_dir;
    let audit_dir = hex_dir.join(".hex/audit");
    let cost_dir = hex_dir.join(".hex/cost");
    let msg_dir = hex_dir.join(".hex/messages");

    // 1. Load charter — one canonical path, no fallbacks
    let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", config.agent_id));
    if !charter_path.exists() {
        return Err(format!(
            "no charter at {} — agent '{}' is not registered (charter.yaml IS registration)",
            charter_path.display(),
            config.agent_id
        )
        .into());
    }
    let charter_data = charter::load(&charter_path)?;
    if charter_data.id != config.agent_id {
        return Err(format!(
            "charter id mismatch: CLI arg is '{}' but charter.id is '{}' in {} — these must match exactly",
            config.agent_id, charter_data.id, charter_path.display()
        ).into());
    }
    let charter_text = std::fs::read_to_string(&charter_path)?;

    // 1b. Load fleet-wide principles (optional, hot-updateable)
    let principles_path = hex_dir.join(".hex/principles.md");
    let principles_text = std::fs::read_to_string(&principles_path).ok();

    // 1c. Load context_files declared in charter (injected into prompt)
    let mut context_files_content = String::new();
    for pattern in &charter_data.context_files {
        let expanded = shellexpand::tilde(pattern).to_string();
        let full_pattern = if Path::new(&expanded).is_absolute() {
            expanded
        } else {
            hex_dir.join(&expanded).to_string_lossy().to_string()
        };
        let matches: Vec<_> = glob::glob(&full_pattern)
            .into_iter()
            .flatten()
            .filter_map(|r| r.ok())
            .collect();
        if matches.is_empty() {
            if let Ok(content) = std::fs::read_to_string(&full_pattern) {
                context_files_content.push_str(&format!("\n## {}\n\n{}\n", pattern, content));
            }
        } else {
            for path in matches {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let rel = path.strip_prefix(hex_dir).unwrap_or(&path);
                    context_files_content.push_str(
                        &format!("\n## {}\n\n{}\n", rel.display(), content)
                    );
                }
            }
        }
    }

    // 2. HALT check
    let kill_switch = shellexpand::tilde(&charter_data.kill_switch).to_string();
    if Path::new(&kill_switch).exists() {
        audit::append(
            &audit_dir,
            &config.agent_id,
            "halted",
            &serde_json::json!({"reason": "kill_switch"}),
        );
        eprintln!(
            "[{}] HALTED: kill switch at {}",
            config.agent_id, kill_switch
        );
        return Ok(0);
    }

    // 3. Load or initialize state — same directory as charter, no fallbacks
    let state_dir = hex_dir.join(format!("projects/{}", config.agent_id));
    std::fs::create_dir_all(&state_dir)?;
    let state_path = state_dir.join("state.json");

    // 3a. Acquire exclusive lock to prevent concurrent wakes from corrupting state
    let lock_path = state_dir.join("state.json.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)
        .map_err(|e| format!("cannot open lock file {}: {e}", lock_path.display()))?;
    use fs2::FileExt;
    match lock_file.try_lock_exclusive() {
        Ok(()) => {}
        Err(_) => {
            eprintln!(
                "[{}] SKIP: another wake is already running (lock held on {})",
                config.agent_id,
                lock_path.display()
            );
            audit::append(
                &audit_dir,
                &config.agent_id,
                "wake-lock-contention",
                &serde_json::json!({"reason": "another wake holds the lock"}),
            );
            return Ok(0);
        }
    }
    // lock_file is held until this function returns (RAII drop)

    let mut agent_state = if state_path.exists() {
        state::load(&state_path)?
    } else {
        state::initialize(&config.agent_id, charter_data.budget.usd_per_day)
    };

    // 4. Reset per-shift cost, increment wake
    agent_state.cost.last_wake_usd = 0.0;
    agent_state.wake_count += 1;
    agent_state.last_wake = Some(Utc::now());

    // 5. Populate inbox
    let inbox_messages = message::receive(&msg_dir, &config.agent_id);
    agent_state.inbox = inbox_messages;
    message::clear_inbox(&msg_dir, &config.agent_id);

    // 6. Queue promotions
    let now = Utc::now();

    // 6b. Auto-promote charter responsibilities not yet in the scheduled queue.
    // On first wake (empty state) all responsibilities seed as due-now.
    // On subsequent wakes, promote_scheduled manages their cadence.
    queue::auto_seed_from_charter(
        &mut agent_state.queue,
        &charter_data.wake.responsibilities,
        &agent_state.cadence_overrides,
        now,
    );

    let sched_promoted = queue::promote_scheduled(&mut agent_state.queue, now);
    let unblocked = queue::promote_unblocked(&mut agent_state.queue);
    let inbox_items = queue::inbox_to_active(&mut agent_state);

    audit::append(
        &audit_dir,
        &config.agent_id,
        "wake-start",
        &serde_json::json!({
            "trigger": config.trigger,
            "wake_count": agent_state.wake_count,
            "scheduled_promoted": sched_promoted,
            "unblocked": unblocked,
            "inbox_items": inbox_items,
            "active_count": agent_state.queue.active.len(),
        }),
    );

    // 7. Nothing actionable?
    if agent_state.queue.active.is_empty() {
        audit::append(
            &audit_dir,
            &config.agent_id,
            "wake-skip",
            &serde_json::json!({"reason": "nothing actionable"}),
        );
        state::save(&agent_state, &state_path)?;
        return Ok(0);
    }

    // 8. Shift loop
    let shift_budget = charter_data.budget.usd_per_shift;
    let allowed_tools = ["Bash", "Read", "Write", "Edit", "Grep", "Glob"];
    let mut invocation = 0;

    // health-probe agents (charter `wake.skip_llm: true`) bypass the LLM loop —
    // they exist to validate wake plumbing without paying for a Claude call.
    // Inbox is populated; post-loop state save + audit still runs.
    if charter_data.wake.skip_llm {
        let inbox_count = agent_state.inbox.len();
        let audit_reason = if inbox_count == 0 {
            "charter wake.skip_llm=true; empty inbox"
        } else {
            "charter wake.skip_llm=true"
        };
        audit::append(
            &audit_dir,
            &config.agent_id,
            "wake-skip-llm",
            &serde_json::json!({
                "reason": audit_reason,
                "inbox_count": inbox_count,
            }),
        );
        // Drain inbox-sourced active items to prevent unbounded state.json growth.
        // health-probe charters have no responsibilities so this is safe.
        agent_state.queue.active.retain(|i| !i.id.starts_with("inbox-"));
    } else {

    loop {
        invocation += 1;

        let remaining = cost::shift_budget_remaining(&agent_state.cost, shift_budget);
        if shift_budget > 0.0 && remaining <= 0.0 {
            eprintln!(
                "WARN: shift budget exhausted (spent ${:.4}, cap ${:.2})",
                agent_state.cost.last_wake_usd, shift_budget
            );
            audit::append(
                &audit_dir,
                &config.agent_id,
                "shift-budget-hit",
                &serde_json::json!({
                    "spent": agent_state.cost.last_wake_usd,
                    "budget": shift_budget,
                    "active_remaining": agent_state.queue.active.len(),
                }),
            );
            break;
        }

        let ctx_files = if context_files_content.is_empty() {
            None
        } else {
            Some(context_files_content.as_str())
        };
        let prompt_text = prompt::build(
            &charter_text,
            &agent_state,
            &config.trigger,
            &config.payload,
            principles_text.as_deref(),
            ctx_files,
        );

        let claude_output = match claude::invoke(&prompt_text, "sonnet", &allowed_tools) {
            Ok(out) => out,
            Err(e) => {
                audit::append(
                    &audit_dir,
                    &config.agent_id,
                    "claude-error",
                    &serde_json::json!({
                        "error": e.to_string(),
                        "invocation": invocation,
                    }),
                );
                break;
            }
        };

        cost::record_invocation(&mut agent_state.cost, &claude_output);
        cost::append_ledger(&cost_dir, &config.agent_id, &claude_output);

        let response = match claude::parse_agent_response(&claude_output.result) {
            Ok(r) => r,
            Err(e) => {
                audit::append(
                    &audit_dir,
                    &config.agent_id,
                    "response-parse-error",
                    &serde_json::json!({
                        "error": e.to_string(),
                        "invocation": invocation,
                        "raw_length": claude_output.result.len(),
                    }),
                );
                break;
            }
        };

        // Validate and append trail entries
        let mut accepted_entries: Vec<crate::types::TrailEntry> = Vec::new();
        for entry in &response.trail {
            match gate::validate(entry) {
                Ok(()) => {
                    agent_state.trail.push(entry.clone());
                    accepted_entries.push(entry.clone());
                    audit::append(
                        &audit_dir,
                        &config.agent_id,
                        &format!("gate:{}", entry.entry_type),
                        &entry.detail,
                    );
                }
                Err(violation) => {
                    audit::append(
                        &audit_dir,
                        &config.agent_id,
                        "gate-violation",
                        &serde_json::json!({
                            "type": entry.entry_type,
                            "violation": violation,
                        }),
                    );
                }
            }
        }

        // Loop detection: check accepted observe/verify entries for repetition
        let interval_seconds = if charter_data.budget.wakes_per_hour > 0 {
            3600 / charter_data.budget.wakes_per_hour as u64
        } else {
            3600
        };
        if check_and_handle_loop(
            &mut agent_state,
            &accepted_entries,
            interval_seconds,
            hex_dir,
            &audit_dir,
        ) {
            state::save(&agent_state, &state_path)?;
            return Ok(0);
        }

        // Apply queue updates
        agent_state
            .queue
            .active
            .retain(|item| !response.queue_updates.completed.contains(&item.id));
        for item in response.queue_updates.added_active {
            agent_state.queue.active.push(item);
        }
        for item in response.queue_updates.moved_to_blocked {
            agent_state.queue.blocked.push(item);
        }

        // Apply memory updates
        if let Some(updates) = response.memory_updates {
            if let (Some(mem), Some(upd)) =
                (agent_state.memory.as_object_mut(), updates.as_object())
            {
                for (k, v) in upd {
                    mem.insert(k.clone(), v.clone());
                }
            }
        }

        // Deliver outbound messages
        for msg in &response.outbound_messages {
            match message::send(&msg_dir, msg) {
                Ok(()) => {
                    audit::append(
                        &audit_dir,
                        &config.agent_id,
                        "message-sent",
                        &serde_json::json!({
                            "to": msg.to,
                            "subject": msg.subject,
                        }),
                    );
                    if msg.response_requested {
                        for recipient in &msg.to {
                            auto_wake_target(hex_dir, recipient, &config.agent_id, &audit_dir);
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[{}] MESSAGE SEND FAILED to {}: {e}",
                        config.agent_id, msg.to.join(", ")
                    );
                    audit::append(
                        &audit_dir,
                        &config.agent_id,
                        "message-send-failed",
                        &serde_json::json!({
                            "to": msg.to,
                            "subject": msg.subject,
                            "error": e.to_string(),
                        }),
                    );
                }
            }
        }

        if response.active_drained || agent_state.queue.active.is_empty() {
            break;
        }
    }

    // 9. Self-assessment phase (runs every N wakes, respects shift budget)
    let assess_interval = charter_data
        .assessment
        .as_ref()
        .map(|a| a.every_n_wakes)
        .unwrap_or_else(|| crate::types::AssessmentConfig::default().every_n_wakes);
    let wakes_since_assessment = agent_state
        .wake_count
        .saturating_sub(agent_state.last_assessment_wake);
    let budget_remaining = cost::shift_budget_remaining(&agent_state.cost, shift_budget);
    let has_budget = shift_budget == 0.0 || budget_remaining > 0.0;

    if assess_interval > 0 && wakes_since_assessment >= assess_interval && has_budget {
        audit::append(
            &audit_dir,
            &config.agent_id,
            "assessment-start",
            &serde_json::json!({
                "wake_count": agent_state.wake_count,
                "last_assessment_wake": agent_state.last_assessment_wake,
                "interval": assess_interval,
            }),
        );

        let assess_prompt =
            prompt::build_assessment(&charter_data, &agent_state, principles_text.as_deref());

        match claude::invoke(&assess_prompt, "sonnet", &["Bash", "Read", "Grep", "Glob"]) {
            Ok(assess_output) => {
                cost::record_invocation(&mut agent_state.cost, &assess_output);
                cost::append_ledger(&cost_dir, &config.agent_id, &assess_output);

                match claude::parse_assessment_response(&assess_output.result) {
                    Ok(assessment) => {
                        // Validate and append assessment trail entries
                        for entry in &assessment.trail {
                            match gate::validate(entry) {
                                Ok(()) => {
                                    agent_state.trail.push(entry.clone());
                                    audit::append(
                                        &audit_dir,
                                        &config.agent_id,
                                        &format!("gate:{}", entry.entry_type),
                                        &entry.detail,
                                    );
                                }
                                Err(violation) => {
                                    audit::append(
                                        &audit_dir,
                                        &config.agent_id,
                                        "gate-violation",
                                        &serde_json::json!({"type": entry.entry_type, "violation": violation}),
                                    );
                                }
                            }
                        }

                        // Apply cadence overrides
                        for change in &assessment.cadence_overrides {
                            agent_state
                                .cadence_overrides
                                .insert(change.responsibility.clone(), change.new_interval);
                            let scheduled_id = format!("s-{}", change.responsibility);
                            if let Some(item) = agent_state.queue.scheduled.iter_mut()
                                .find(|s| s.id == scheduled_id) {
                                item.interval_seconds = change.new_interval;
                            }
                            audit::append(
                                &audit_dir,
                                &config.agent_id,
                                "cadence-change",
                                &serde_json::json!({
                                    "responsibility": change.responsibility,
                                    "old_interval": change.old_interval,
                                    "new_interval": change.new_interval,
                                    "reason": change.reason,
                                }),
                            );
                        }

                        // Apply strategy updates to working memory
                        if let Some(ref updates) = assessment.strategy_updates {
                            if let (Some(mem), Some(upd)) =
                                (agent_state.memory.as_object_mut(), updates.as_object())
                            {
                                for (k, v) in upd {
                                    mem.insert(k.clone(), v.clone());
                                }
                            }
                        }

                        // Log recommendations
                        if !assessment.recommendations.is_empty() {
                            audit::append(
                                &audit_dir,
                                &config.agent_id,
                                "assessment-recommendations",
                                &serde_json::json!({"recommendations": assessment.recommendations}),
                            );
                        }

                        agent_state.last_assessment_wake = agent_state.wake_count;

                        audit::append(
                            &audit_dir,
                            &config.agent_id,
                            "assessment-complete",
                            &serde_json::json!({
                                "cadence_changes": assessment.cadence_overrides.len(),
                                "recommendations": assessment.recommendations.len(),
                                "has_strategy_updates": assessment.strategy_updates.is_some(),
                            }),
                        );
                    }
                    Err(e) => {
                        audit::append(
                            &audit_dir,
                            &config.agent_id,
                            "assessment-parse-error",
                            &serde_json::json!({"error": e.to_string()}),
                        );
                        agent_state.last_assessment_wake = agent_state.wake_count;
                    }
                }
            }
            Err(e) => {
                audit::append(
                    &audit_dir,
                    &config.agent_id,
                    "assessment-claude-error",
                    &serde_json::json!({"error": e.to_string()}),
                );
                agent_state.last_assessment_wake = agent_state.wake_count;
            }
        }
    }

    } // end if !skip_llm

    // 10. Save state
    state::save(&agent_state, &state_path)?;

    audit::append(
        &audit_dir,
        &config.agent_id,
        "wake-complete",
        &serde_json::json!({
            "invocations": invocation,
            "shift_cost_usd": agent_state.cost.last_wake_usd,
            "trail_entries": agent_state.trail.len(),
            "active_remaining": agent_state.queue.active.len(),
        }),
    );

    Ok(0)
}

/// Spawn a background wake for a target agent when response_requested is true.
/// Fire-and-forget: the current wake doesn't block on the target's wake.
pub fn auto_wake_target(hex_dir: &Path, target_id: &str, sender_id: &str, audit_dir: &Path) {
    let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", target_id));
    if !charter_path.exists() {
        eprintln!(
            "[{}] SKIP auto-wake: target '{}' has no charter",
            sender_id, target_id
        );
        return;
    }
    let binary = hex_dir.join(".hex/bin/hex-agent");
    if !binary.exists() {
        eprintln!(
            "[{}] SKIP auto-wake: hex-agent binary not found at {}",
            sender_id,
            binary.display()
        );
        return;
    }
    let trigger = format!("inbox-from-{}", sender_id);
    match std::process::Command::new(&binary)
        .arg("wake")
        .arg(target_id)
        .arg("--trigger")
        .arg(&trigger)
        .env("HEX_DIR", hex_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => {
            audit::append(
                audit_dir,
                sender_id,
                "auto-wake-spawned",
                &serde_json::json!({
                    "target": target_id,
                    "trigger": trigger,
                }),
            );
        }
        Err(e) => {
            eprintln!(
                "[{}] auto-wake FAILED for '{}': {e}",
                sender_id, target_id
            );
            audit::append(
                audit_dir,
                sender_id,
                "auto-wake-failed",
                &serde_json::json!({
                    "target": target_id,
                    "error": e.to_string(),
                }),
            );
        }
    }
}

pub fn compute_action_hash(agent_id: &str, trail_type: &str, detail: &serde_json::Value) -> String {
    let sorted_detail = if let Some(obj) = detail.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        let sorted: serde_json::Map<String, serde_json::Value> =
            keys.iter().map(|k| (k.to_string(), obj[*k].clone())).collect();
        serde_json::to_string(&serde_json::Value::Object(sorted)).unwrap_or_default()
    } else {
        detail.to_string()
    };
    let input = format!("{}:{}:{}", agent_id, trail_type, sorted_detail);
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex_bytes::encode(result)[..16].to_string()
}

pub fn check_and_handle_loop(
    agent_state: &mut crate::types::AgentState,
    new_entries: &[crate::types::TrailEntry],
    interval_seconds: u64,
    hex_dir: &Path,
    audit_dir: &Path,
) -> bool {
    let now_unix = Utc::now().timestamp() as u64;

    for entry in new_entries {
        if entry.entry_type == "observe" || entry.entry_type == "verify" {
            let hash = compute_action_hash(&agent_state.agent_id, &entry.entry_type, &entry.detail);
            agent_state.recent_action_hashes.push((hash, now_unix));
        }
    }

    let prune_cutoff = now_unix.saturating_sub(interval_seconds * 10);
    agent_state.recent_action_hashes.retain(|(_, ts)| *ts >= prune_cutoff);

    let loop_cutoff = now_unix.saturating_sub(interval_seconds * 6);
    let hashes_snapshot: Vec<String> = agent_state
        .recent_action_hashes
        .iter()
        .map(|(h, _)| h.clone())
        .collect();

    for candidate_hash in &hashes_snapshot {
        let count = agent_state
            .recent_action_hashes
            .iter()
            .filter(|(h, ts)| h == candidate_hash && *ts >= loop_cutoff)
            .count();
        if count >= 3 {
            let home = std::env::var("HOME").unwrap_or_default();
            let halt_path = format!("{}/.hex-{}-HALT-loop", home, agent_state.agent_id);
            let _ = std::fs::write(&halt_path, "Loop detected: same action repeated 3x. Manual review required.");

            let emit_script = hex_dir.join(".hex/bin/hex-emit.sh");
            let payload = serde_json::json!({
                "agent_id": agent_state.agent_id,
                "action_hash": candidate_hash,
                "count": count,
            });
            let _ = std::process::Command::new(&emit_script)
                .arg("hex.agent.loop.detected")
                .arg(payload.to_string())
                .status();

            audit::append(
                audit_dir,
                &agent_state.agent_id,
                "loop-halt",
                &serde_json::json!({
                    "hash": candidate_hash,
                    "count": count,
                    "action_sample": format!("observe/verify:{}", candidate_hash),
                }),
            );
            return true;
        }
    }
    false
}
