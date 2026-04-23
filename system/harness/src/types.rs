use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Queue Items ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveItem {
    pub id: String,
    pub summary: String,
    pub priority: i32,
    pub created: DateTime<Utc>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedItem {
    pub id: String,
    pub summary: String,
    pub priority: i32,
    pub blocked_on: String,
    pub blocked_type: String,
    pub blocked_ref: Option<String>,
    pub blocked_since: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledItem {
    pub id: String,
    pub summary: String,
    pub interval_seconds: u64,
    pub last_run: Option<DateTime<Utc>>,
    pub next_due: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
    pub active: Vec<ActiveItem>,
    pub blocked: Vec<BlockedItem>,
    pub scheduled: Vec<ScheduledItem>,
}

// ── Trail ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrailEntry {
    pub ts: DateTime<Utc>,
    #[serde(rename = "type")]
    pub entry_type: String,
    pub detail: serde_json::Value,
    pub queue_item: Option<String>,
}

// ── Messages ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub initiative_id: Option<String>,
    pub response_requested: bool,
    pub in_reply_to: Option<String>,
    pub sent_at: DateTime<Utc>,
}

// ── Cost ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostPeriod {
    pub start: DateTime<Utc>,
    pub spent_usd: f64,
    pub budget_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cost {
    pub lifetime_usd: f64,
    pub current_period: CostPeriod,
    pub last_wake_usd: f64,
}

// ── Agent State ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    pub agent_id: String,
    pub version: u32,
    pub wake_count: u64,
    pub last_wake: Option<DateTime<Utc>>,
    pub queue: Queue,
    pub trail: Vec<TrailEntry>,
    pub initiatives: HashMap<String, serde_json::Value>,
    pub inbox: Vec<Message>,
    pub memory: serde_json::Value,
    pub cost: Cost,
    #[serde(default)]
    pub cadence_overrides: HashMap<String, u64>,
    #[serde(default)]
    pub last_assessment_wake: u64,
}

// ── Claude Output ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeOutput {
    pub result: String,
    #[serde(default)]
    pub total_cost_usd: f64,
    pub usage: ClaudeUsage,
    pub duration_ms: u64,
    pub stop_reason: String,
    pub session_id: Option<String>,
}

// ── Agent Structured Response ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub trail: Vec<TrailEntry>,
    pub queue_updates: QueueUpdates,
    pub memory_updates: Option<serde_json::Value>,
    pub outbound_messages: Vec<Message>,
    pub active_drained: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueUpdates {
    #[serde(default)]
    pub completed: Vec<String>,
    #[serde(default)]
    pub added_active: Vec<ActiveItem>,
    #[serde(default)]
    pub moved_to_blocked: Vec<BlockedItem>,
    #[serde(default)]
    pub parked: Vec<BlockedItem>,
}

// ── Self-Assessment ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CadenceChange {
    pub responsibility: String,
    pub old_interval: u64,
    pub new_interval: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssessmentResponse {
    pub trail: Vec<TrailEntry>,
    #[serde(default)]
    pub cadence_overrides: Vec<CadenceChange>,
    pub strategy_updates: Option<serde_json::Value>,
    #[serde(default)]
    pub recommendations: Vec<String>,
}

// ── Charter ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Responsibility {
    pub name: String,
    #[serde(default)]
    pub interval: Option<u64>,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeConfig {
    pub triggers: Vec<String>,
    pub responsibilities: Vec<Responsibility>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    pub wakes_per_hour: u32,
    pub usd_per_day: f64,
    pub usd_per_shift: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub max_size_kb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HooksConfig {
    pub on_find: Option<String>,
    pub on_decide: Option<String>,
    pub on_act: Option<String>,
    pub on_verify: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorityTiers {
    pub green: Vec<String>,
    pub yellow: Vec<String>,
    pub red: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssessmentConfig {
    #[serde(default = "default_assess_interval")]
    pub every_n_wakes: u64,
}

fn default_assess_interval() -> u64 {
    10
}

impl Default for AssessmentConfig {
    fn default() -> Self {
        AssessmentConfig {
            every_n_wakes: default_assess_interval(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Charter {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub role: String,
    pub scope: Option<String>,
    pub parent: Option<String>,
    pub objective: Option<String>,
    pub kpis: Option<Vec<String>>,
    pub wake: WakeConfig,
    pub authority: AuthorityTiers,
    pub budget: Budget,
    pub memory: Option<MemoryConfig>,
    pub hooks: Option<HooksConfig>,
    pub assessment: Option<AssessmentConfig>,
    pub escalation_channel: Option<String>,
    pub kill_switch: String,
    #[serde(default)]
    pub core: bool,
}
