use crate::types::{ClaudeOutput, Cost};
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

pub fn record_invocation(cost: &mut Cost, output: &ClaudeOutput) {
    let usd = output.total_cost_usd;
    cost.last_wake_usd += usd;
    cost.current_period.spent_usd += usd;
    cost.lifetime_usd += usd;
}

pub fn append_ledger(ledger_dir: &Path, agent_id: &str, output: &ClaudeOutput) {
    let path = ledger_dir.join("ledger.jsonl");
    if let Err(e) = fs::create_dir_all(ledger_dir) {
        eprintln!(
            "COST LEDGER FAILED: cannot create {}: {e}",
            ledger_dir.display()
        );
        return;
    }
    let entry = serde_json::json!({
        "ts": Utc::now().to_rfc3339(),
        "agent": agent_id,
        "cost_usd": output.total_cost_usd,
        "input_tokens": output.usage.input_tokens,
        "output_tokens": output.usage.output_tokens,
        "cache_read_tokens": output.usage.cache_read_input_tokens,
        "cache_creation_tokens": output.usage.cache_creation_input_tokens,
        "duration_ms": output.duration_ms,
    });
    let line = match serde_json::to_string(&entry) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("COST SERIALIZE FAILED: {e}");
            return;
        }
    };
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{}", line) {
                eprintln!(
                    "COST LEDGER FAILED: cannot write to {}: {e}",
                    path.display()
                );
            }
        }
        Err(e) => {
            eprintln!("COST LEDGER FAILED: cannot open {}: {e}", path.display());
        }
    }
}

pub fn shift_budget_remaining(cost: &Cost, per_shift: f64) -> f64 {
    per_shift - cost.last_wake_usd
}
