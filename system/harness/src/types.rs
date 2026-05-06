use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

fn default_now() -> DateTime<Utc> {
    Utc::now()
}

fn default_now_string() -> String {
    Utc::now().to_rfc3339()
}

fn default_new_status() -> String {
    "new".to_string()
}

fn default_agent_msg_type() -> MessageType {
    MessageType::Agent
}

/// Deserialize a Vec<T>, skipping items that fail to parse instead of failing the whole response.
pub fn deserialize_lenient_vec<'de, T, D>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    T: serde::de::DeserializeOwned,
    D: Deserializer<'de>,
{
    let values: Vec<serde_json::Value> = Vec::deserialize(deserializer).unwrap_or_default();
    let mut result = Vec::new();
    for val in values {
        match serde_json::from_value::<T>(val.clone()) {
            Ok(item) => result.push(item),
            Err(e) => eprintln!("[harness] lenient-parse: skipped malformed item ({e}): {val}"),
        }
    }
    Ok(result)
}

// ── Queue Items ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_now")]
    pub created: DateTime<Utc>,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub blocked_on: String,
    #[serde(default)]
    pub blocked_type: String,
    pub blocked_ref: Option<String>,
    #[serde(default = "default_now")]
    pub blocked_since: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub summary: String,
    pub interval_seconds: u64,
    pub last_run: Option<DateTime<Utc>>,
    pub next_due: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacklogItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
    pub active: Vec<ActiveItem>,
    pub blocked: Vec<BlockedItem>,
    pub scheduled: Vec<ScheduledItem>,
    #[serde(default)]
    pub backlog: Vec<BacklogItem>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Comment,
    Agent,
    Notification,
}

impl std::fmt::Display for MessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageType::Comment => write!(f, "comment"),
            MessageType::Agent => write!(f, "agent"),
            MessageType::Notification => write!(f, "notification"),
        }
    }
}

impl std::str::FromStr for MessageType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "comment" => Ok(MessageType::Comment),
            "agent" => Ok(MessageType::Agent),
            "notification" => Ok(MessageType::Notification),
            other => Err(format!("unknown msg_type '{}'; expected comment, agent, or notification", other)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionEntry {
    pub ts: String,
    pub action: String,
    #[serde(default)]
    pub related_assets: Vec<String>,
}

/// Deserializes the `to` field accepting both a JSON string and a JSON array of strings.
/// Agents emit `"to": "agent-id"` (single string); Store A stores `"to": ["agent-id"]` (array).
pub fn deserialize_to_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ToField {
        Single(String),
        Multiple(Vec<String>),
    }
    match ToField::deserialize(deserializer)? {
        ToField::Single(s) => Ok(vec![s]),
        ToField::Multiple(v) => Ok(v),
    }
}

/// The canonical message struct — the single source of truth for all message surfaces:
/// CLI send, HTTP create, inter-agent emit, agent inbox, audit log, and dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    // Audit log fields (always present in messages.json)
    #[serde(default)]
    pub id: String,
    #[serde(default = "default_agent_msg_type")]
    pub msg_type: MessageType,
    #[serde(default)]
    pub from: String,
    #[serde(default, deserialize_with = "deserialize_to_vec")]
    pub to: Vec<String>,
    /// Canonical content: `[subject] body` format. Populated by harness on send.
    #[serde(default)]
    pub content: String,
    pub anchor: Option<String>,
    #[serde(default = "default_new_status")]
    pub status: String,
    #[serde(default = "default_now_string")]
    pub created_at: String,
    #[serde(default)]
    pub action_log: Vec<ActionEntry>,
    #[serde(default)]
    pub routed_to: Vec<String>,
    // Agent-interaction fields (used in outbound_messages and agent inbox)
    /// Subject line. Agents set this when sending; harness populates at receive from content.
    #[serde(default)]
    pub subject: String,
    /// Message body. Agents set this when sending; harness populates at receive from content.
    #[serde(default)]
    pub body: String,
    pub initiative_id: Option<String>,
    #[serde(default)]
    pub response_requested: bool,
    pub in_reply_to: Option<String>,
    /// Backward-compat alias for created_at; defaulted to now on old records.
    #[serde(default = "default_now")]
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
    #[serde(default)]
    pub recent_action_hashes: Vec<(String, u64)>,
    /// Stable IDs of backlog items the agent has completed; prevents re-seeding from backlog.md.
    #[serde(default)]
    pub completed_backlog_ids: Vec<String>,
    /// How many backlog-driven wakes fired today; resets at date rollover.
    #[serde(default)]
    pub backlog_wakes_today: u32,
    /// Date string (YYYY-MM-DD) for the current backlog_wakes_today window.
    #[serde(default)]
    pub backlog_wakes_date: Option<String>,
    /// Timestamp of the most recent accepted `act` trail entry; used for idle-gate.
    #[serde(default)]
    pub last_trail_act_at: Option<DateTime<Utc>>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentResponse {
    #[serde(default)]
    pub trail: Vec<TrailEntry>,
    #[serde(default)]
    pub queue_updates: QueueUpdates,
    pub memory_updates: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "deserialize_lenient_vec")]
    pub outbound_messages: Vec<Message>,
    #[serde(default)]
    pub active_drained: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueUpdates {
    #[serde(default)]
    pub completed: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_lenient_vec")]
    pub added_active: Vec<ActiveItem>,
    #[serde(default, deserialize_with = "deserialize_lenient_vec")]
    pub moved_to_blocked: Vec<BlockedItem>,
    #[serde(default, deserialize_with = "deserialize_lenient_vec")]
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
    /// When true, the wake skips the Claude shift loop entirely. Used by
    /// health-probe agents that exercise the wake plumbing (inbox → audit →
    /// mark_delivered) without paying for an LLM call.
    #[serde(default)]
    pub skip_llm: bool,
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
    #[serde(default)]
    pub context_files: Vec<String>,
    /// Proactive work this agent may self-assign when active queue is empty.
    /// Empty = reactive-only agent; never auto-promotes from backlog.
    #[serde(default)]
    pub proactive_initiatives: Vec<String>,
}
