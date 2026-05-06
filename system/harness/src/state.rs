use crate::types::{AgentState, Cost, CostPeriod, Queue};
use chrono::Utc;
use std::path::Path;

pub fn initialize(agent_id: &str, budget_usd: f64) -> AgentState {
    let now = Utc::now();
    AgentState {
        agent_id: agent_id.to_string(),
        version: 1,
        wake_count: 0,
        last_wake: None,
        queue: Queue {
            // Queue starts empty. On first wake, wake.rs auto-promotes charter
            // responsibilities to scheduled items. No manual seeding required.
            active: vec![],
            blocked: vec![],
            scheduled: vec![],
            backlog: vec![],
        },
        trail: vec![],
        initiatives: Default::default(),
        inbox: vec![],
        memory: serde_json::Value::Object(Default::default()),
        cost: Cost {
            lifetime_usd: 0.0,
            current_period: CostPeriod {
                start: now,
                spent_usd: 0.0,
                budget_usd,
            },
            last_wake_usd: 0.0,
        },
        cadence_overrides: Default::default(),
        last_assessment_wake: 0,
        recent_action_hashes: vec![],
        completed_backlog_ids: vec![],
        backlog_wakes_today: 0,
        backlog_wakes_date: None,
        last_trail_act_at: None,
    }
}

pub fn load(path: &Path) -> Result<AgentState, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read state at {}: {e}", path.display()))?;
    let state: AgentState = serde_json::from_str(&contents)
        .map_err(|e| format!("corrupt state.json at {}: {e}", path.display()))?;
    Ok(state)
}

pub fn save(state: &AgentState, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(&tmp, &json)
        .map_err(|e| format!("cannot write state tmp at {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("cannot rename {} to {}: {e}", tmp.display(), path.display()))?;
    Ok(())
}
