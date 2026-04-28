use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::server::{Request, Response};
use crate::sse::SseBus;
use crate::telemetry::Telemetry;

// ── Policy structures ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Policy {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub name: String,
    pub trigger: Trigger,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default)]
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Trigger {
    pub event: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Condition {
    pub field: String,
    pub op: String,
    #[serde(default)]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Action {
    pub r#type: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

// ── Engine ────────────────────────────────────────────────────────────────────

pub struct EventEngine {
    db: Mutex<Connection>,
    pub policies_dir: PathBuf,
    policies: RwLock<Vec<Policy>>,
    telemetry: Arc<Telemetry>,
    pub bus: Arc<SseBus>,
    start_time: Instant,
    events_processed: Mutex<u64>,
}

impl EventEngine {
    pub fn new(
        _hex_dir: &Path,
        telemetry: Arc<Telemetry>,
        bus: Arc<SseBus>,
    ) -> Result<Arc<Self>, String> {
        let home = PathBuf::from(shellexpand::tilde("~").as_ref());
        let hex_events_dir = home.join(".hex-events");
        let _ = std::fs::create_dir_all(&hex_events_dir);

        let db_path = hex_events_dir.join("events.db");
        let policies_dir = hex_events_dir.join("policies");
        let _ = std::fs::create_dir_all(&policies_dir);

        let conn = Connection::open(&db_path)
            .map_err(|e| format!("events db open failed: {e}"))?;
        init_schema(&conn)?;

        let engine = Arc::new(Self {
            db: Mutex::new(conn),
            policies_dir,
            policies: RwLock::new(Vec::new()),
            telemetry,
            bus,
            start_time: Instant::now(),
            events_processed: Mutex::new(0),
        });

        engine.load_policies();
        Ok(engine)
    }

    pub fn load_policies(&self) {
        let pattern = self.policies_dir.join("*.yaml");
        let pattern_str = pattern.to_string_lossy();
        let mut loaded = Vec::new();

        if let Ok(paths) = glob::glob(&pattern_str) {
            for entry in paths.flatten() {
                match std::fs::read_to_string(&entry) {
                    Ok(content) => match serde_yaml::from_str::<Policy>(&content) {
                        Ok(p) => {
                            if p.enabled.unwrap_or(true) {
                                loaded.push(p);
                            }
                        }
                        Err(e) => eprintln!("events: failed to parse {:?}: {e}", entry),
                    },
                    Err(e) => eprintln!("events: failed to read {:?}: {e}", entry),
                }
            }
        }

        let count = loaded.len();
        *self.policies.write().unwrap() = loaded;
        eprintln!("events: loaded {count} policies from {:?}", self.policies_dir);
    }

    pub fn reload_policies(&self) {
        self.load_policies();
    }

    /// Ingest an event: write to DB, match policies, execute actions.
    /// Returns the new event's row ID, or -1 on error.
    pub fn ingest(&self, event_type: &str, payload: &Value, source: &str) -> i64 {
        let now = Utc::now().to_rfc3339();
        let payload_str = payload.to_string();

        let event_id = {
            let db = self.db.lock().unwrap();
            match db.execute(
                "INSERT INTO events (event_type, payload, source, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![event_type, payload_str, source, now],
            ) {
                Ok(_) => db.last_insert_rowid(),
                Err(e) => {
                    eprintln!("events: db insert failed: {e}");
                    return -1;
                }
            }
        };

        *self.events_processed.lock().unwrap() += 1;

        let policies = self.policies.read().unwrap().clone();
        for policy in &policies {
            for rule in &policy.rules {
                if !wildcard_matches(&rule.trigger.event, event_type) {
                    continue;
                }
                let all_pass = rule
                    .conditions
                    .iter()
                    .all(|c| self.evaluate_condition(c, payload));
                if !all_pass {
                    continue;
                }
                for action in &rule.actions {
                    self.execute_action(
                        action,
                        event_id,
                        &policy.name,
                        &rule.name,
                        event_type,
                        payload,
                    );
                }
            }
        }

        self.bus.publish("hex.events", event_type, payload);
        self.telemetry.emit(
            "hex.event.ingested",
            &serde_json::json!({ "event_id": event_id, "event_type": event_type }),
        );

        event_id
    }

    fn evaluate_condition(&self, condition: &Condition, payload: &Value) -> bool {
        match condition.op.as_str() {
            "exists" => resolve_field(&condition.field, payload).is_some(),
            "eq" => match (
                resolve_field(&condition.field, payload).as_ref(),
                condition.value.as_ref(),
            ) {
                (Some(a), Some(v)) => a == v,
                _ => false,
            },
            "ne" => match (
                resolve_field(&condition.field, payload).as_ref(),
                condition.value.as_ref(),
            ) {
                (Some(a), Some(v)) => a != v,
                _ => true,
            },
            "gt" => {
                let actual = resolve_field(&condition.field, payload);
                cmp_nums(&actual, &condition.value, true)
            }
            "lt" => {
                let actual = resolve_field(&condition.field, payload);
                cmp_nums(&actual, &condition.value, false)
            }
            "contains" => {
                match (
                    resolve_field(&condition.field, payload),
                    condition.value.as_ref(),
                ) {
                    (Some(a), Some(v)) => {
                        let needle = v
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| v.to_string());
                        value_to_str(&a).contains(&needle)
                    }
                    _ => false,
                }
            }
            "regex" => {
                match (
                    resolve_field(&condition.field, payload),
                    condition.value.as_ref(),
                ) {
                    (Some(a), Some(v)) => {
                        let pattern = v
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| v.to_string());
                        // Substring match (full regex support can be added with the regex crate)
                        value_to_str(&a).contains(&pattern)
                    }
                    _ => false,
                }
            }
            other => {
                eprintln!("events: unknown condition op '{other}'");
                false
            }
        }
    }

    fn execute_action(
        &self,
        action: &Action,
        event_id: i64,
        policy_name: &str,
        rule_name: &str,
        event_type: &str,
        payload: &Value,
    ) {
        let now = Utc::now().to_rfc3339();

        match action.r#type.as_str() {
            "shell" => {
                let (status, error) = if let Some(cmd_tpl) = &action.command {
                    let cmd = render_template(cmd_tpl, event_type, payload);
                    match std::process::Command::new("sh").arg("-c").arg(&cmd).output() {
                        Ok(out) => {
                            if out.status.success() {
                                ("ok".to_string(), String::new())
                            } else {
                                let err = String::from_utf8_lossy(&out.stderr).to_string();
                                ("error".to_string(), err[..err.len().min(500)].to_string())
                            }
                        }
                        Err(e) => ("error".to_string(), e.to_string()),
                    }
                } else {
                    ("error".to_string(), "no command specified".to_string())
                };
                let db = self.db.lock().unwrap();
                let _ = db.execute(
                    "INSERT INTO action_log \
                     (event_id, policy_name, rule_name, action_type, status, error, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![event_id, policy_name, rule_name, "shell", status, error, now],
                );
            }
            "emit" => {
                if let Some(emit_event) = &action.event {
                    let rendered = render_template(emit_event, event_type, payload);
                    self.ingest(&rendered, payload, "event_engine");
                }
            }
            "notify" => {
                eprintln!(
                    "events: [notify] policy={policy_name} rule={rule_name} event={event_type}"
                );
                let db = self.db.lock().unwrap();
                let _ = db.execute(
                    "INSERT INTO action_log \
                     (event_id, policy_name, rule_name, action_type, status, error, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![event_id, policy_name, rule_name, "notify", "ok", "", now],
                );
            }
            other => eprintln!("events: unknown action type '{other}'"),
        }
    }

    pub fn start_scheduler(engine: Arc<Self>) {
        std::thread::spawn(move || {
            let mut last_minutely = Instant::now();
            let mut last_hourly = Instant::now();
            let mut last_6h = Instant::now();
            let mut last_daily = Instant::now();

            loop {
                std::thread::sleep(Duration::from_secs(60));
                let now = Instant::now();

                if now.duration_since(last_minutely).as_secs() >= 60 {
                    engine.ingest("timer.tick.minutely", &serde_json::json!({}), "scheduler");
                    last_minutely = now;
                }
                if now.duration_since(last_hourly).as_secs() >= 3600 {
                    engine.ingest("timer.tick.hourly", &serde_json::json!({}), "scheduler");
                    last_hourly = now;
                }
                if now.duration_since(last_6h).as_secs() >= 6 * 3600 {
                    engine.ingest("timer.tick.6h", &serde_json::json!({}), "scheduler");
                    last_6h = now;
                }
                if now.duration_since(last_daily).as_secs() >= 24 * 3600 {
                    engine.ingest("timer.tick.daily", &serde_json::json!({}), "scheduler");
                    last_daily = now;
                }
            }
        });
    }

    // ── HTTP ──────────────────────────────────────────────────────────────────

    pub fn handle(&self, req: &Request) -> Response {
        let path = req.path.strip_prefix("/events").unwrap_or(&req.path);
        let method = req.method.as_str();

        match (method, path) {
            ("POST", "/ingest") => self.http_ingest(req),
            ("GET", "/recent") => self.http_recent(req),
            ("GET", "/status") => self.http_status(),
            ("GET", "/health") | ("GET", "/health/") => {
                json_ok(&serde_json::json!({ "status": "ok" }))
            }
            _ => json_error(404, "events endpoint not found"),
        }
    }

    fn http_ingest(&self, req: &Request) -> Response {
        #[derive(Deserialize)]
        struct Body {
            event_type: String,
            #[serde(default)]
            payload: Value,
            #[serde(default)]
            source: String,
        }

        let b: Body = match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(e) => return json_error(400, &format!("invalid JSON: {e}")),
        };

        let event_id = self.ingest(&b.event_type, &b.payload, &b.source);
        Response {
            status: 202,
            content_type: "application/json".to_string(),
            headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
            body: serde_json::to_vec(&serde_json::json!({ "event_id": event_id }))
                .unwrap_or_default(),
        }
    }

    fn http_recent(&self, req: &Request) -> Response {
        let limit: i64 = req
            .query
            .get("limit")
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);

        let db = self.db.lock().unwrap();
        let mut stmt = match db.prepare(
            "SELECT id, event_type, payload, source, created_at \
             FROM events ORDER BY id DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(e) => return json_error(500, &e.to_string()),
        };

        let mut rows: Vec<Value> = Vec::new();
        let mut query = match stmt.query(params![limit]) {
            Ok(q) => q,
            Err(e) => return json_error(500, &e.to_string()),
        };

        while let Ok(Some(row)) = query.next() {
            let id: i64 = row.get(0).unwrap_or(0);
            let event_type: String = row.get(1).unwrap_or_default();
            let payload_str: String = row.get(2).unwrap_or_else(|_| "null".to_string());
            let source: String = row.get(3).unwrap_or_default();
            let created_at: String = row.get(4).unwrap_or_default();
            let payload: Value =
                serde_json::from_str(&payload_str).unwrap_or(Value::Null);
            rows.push(serde_json::json!({
                "id": id,
                "event_type": event_type,
                "payload": payload,
                "source": source,
                "created_at": created_at,
            }));
        }

        json_ok(&rows)
    }

    fn http_status(&self) -> Response {
        let policy_count = self.policies.read().unwrap().len();
        let events_processed = *self.events_processed.lock().unwrap();
        let uptime_secs = self.start_time.elapsed().as_secs();

        json_ok(&serde_json::json!({
            "status": "running",
            "uptime_seconds": uptime_secs,
            "policies_loaded": policy_count,
            "events_processed": events_processed,
            "policies_dir": self.policies_dir.to_string_lossy(),
        }))
    }

    // ── CLI ───────────────────────────────────────────────────────────────────

    pub fn policy_count(&self) -> usize {
        self.policies.read().map(|p| p.len()).unwrap_or(0)
    }

    pub fn cli_status(&self) {
        let policy_count = self.policies.read().unwrap().len();
        let events_processed = *self.events_processed.lock().unwrap();
        let uptime_secs = self.start_time.elapsed().as_secs();

        println!("hex events status");
        println!("  uptime:           {uptime_secs}s");
        println!("  policies loaded:  {policy_count}");
        println!("  events processed: {events_processed}");
        println!("  policies dir:     {:?}", self.policies_dir);
        if policy_count == 0 {
            println!("  (server may not be running; showing local state)");
        }
    }

    pub fn cli_emit(&self, event_type: &str, payload_json: &str) {
        let payload: Value = match serde_json::from_str(payload_json) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Invalid payload JSON: {e}");
                std::process::exit(1);
            }
        };
        let event_id = self.ingest(event_type, &payload, "cli");
        println!("Emitted {event_type} (id={event_id})");
    }

    pub fn cli_trace(&self, event_id: i64) {
        let db = self.db.lock().unwrap();

        let ev = db.query_row(
            "SELECT event_type, payload, source, created_at FROM events WHERE id = ?1",
            params![event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2).unwrap_or_default(),
                    row.get::<_, String>(3)?,
                ))
            },
        );

        match ev {
            Ok((event_type, payload, source, created_at)) => {
                println!("Event #{event_id}: {event_type}");
                println!("  source:     {source}");
                println!("  created_at: {created_at}");
                println!("  payload:    {payload}");
            }
            Err(_) => {
                eprintln!("Event {event_id} not found");
                std::process::exit(1);
            }
        }

        let mut stmt = match db.prepare(
            "SELECT policy_name, rule_name, action_type, status, error, created_at \
             FROM action_log WHERE event_id = ?1 ORDER BY id",
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to query action log: {e}");
                return;
            }
        };

        println!("\nAction chain:");
        let mut query = match stmt.query(params![event_id]) {
            Ok(q) => q,
            Err(e) => {
                eprintln!("{e}");
                return;
            }
        };

        let mut count = 0usize;
        while let Ok(Some(row)) = query.next() {
            let policy: String = row.get(0).unwrap_or_default();
            let rule: String = row.get(1).unwrap_or_default();
            let action: String = row.get(2).unwrap_or_default();
            let status: String = row.get(3).unwrap_or_default();
            let error: String = row.get(4).unwrap_or_default();
            let ts: String = row.get(5).unwrap_or_default();
            println!("  [{ts}] policy={policy} rule={rule} action={action} status={status}");
            if !error.is_empty() {
                println!("    error: {error}");
            }
            count += 1;
        }
        if count == 0 {
            println!("  (no actions recorded)");
        }
    }

    pub fn cli_policies(&self) {
        let policies = self.policies.read().unwrap();
        if policies.is_empty() {
            println!("No policies loaded (dir: {:?})", self.policies_dir);
            return;
        }
        println!("{} policies:", policies.len());
        for p in policies.iter() {
            println!("  {} — {} rules — {}", p.name, p.rules.len(), p.description);
        }
    }

    pub fn cli_reload(&self) {
        self.reload_policies();
        let count = self.policies.read().unwrap().len();
        println!("Reloaded {count} policies");
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn init_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA busy_timeout=5000;
         CREATE TABLE IF NOT EXISTS events (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             event_type TEXT NOT NULL,
             payload TEXT,
             source TEXT DEFAULT '',
             created_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS action_log (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             event_id INTEGER REFERENCES events(id),
             policy_name TEXT,
             rule_name TEXT,
             action_type TEXT,
             status TEXT,
             error TEXT,
             created_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);
         CREATE INDEX IF NOT EXISTS idx_events_created ON events(created_at);",
    )
    .map_err(|e| format!("schema init failed: {e}"))
}

fn wildcard_matches(pattern: &str, event_type: &str) -> bool {
    if pattern == "*" || pattern == event_type {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return event_type.starts_with(&format!("{prefix}."));
    }
    false
}

fn resolve_field(field: &str, payload: &Value) -> Option<Value> {
    let path = field.strip_prefix("payload.").unwrap_or(field);
    let mut current = payload;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current.clone())
}

fn value_to_str(v: &Value) -> String {
    v.as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| v.to_string())
}

fn cmp_nums(actual: &Option<Value>, expected: &Option<Value>, gt: bool) -> bool {
    match (actual.as_ref().and_then(|v| v.as_f64()), expected.as_ref().and_then(|v| v.as_f64())) {
        (Some(a), Some(b)) => if gt { a > b } else { a < b },
        _ => false,
    }
}

/// Substitute {{event.type}} and {{event.FIELD}} in action templates.
fn render_template(template: &str, event_type: &str, payload: &Value) -> String {
    let mut s = template
        .replace("{{event.type}}", event_type)
        .replace("{{event_type}}", event_type);
    if let Value::Object(map) = payload {
        for (k, v) in map {
            s = s.replace(&format!("{{{{event.{k}}}}}"), &value_to_str(v));
        }
    }
    s
}

fn json_ok<T: Serialize>(val: &T) -> Response {
    Response {
        status: 200,
        content_type: "application/json".to_string(),
        headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
        body: serde_json::to_vec(val).unwrap_or_default(),
    }
}

fn json_error(status: u16, msg: &str) -> Response {
    Response {
        status,
        content_type: "application/json".to_string(),
        headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
        body: serde_json::to_vec(&serde_json::json!({ "error": msg })).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_engine(tmp: &TempDir) -> Arc<EventEngine> {
        let bus = SseBus::new();
        let telemetry = Arc::new(Telemetry::new(tmp.path()));
        // Override policies_dir to an empty temp dir so we load 0 policies
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        Arc::new(EventEngine {
            db: Mutex::new(conn),
            policies_dir: tmp.path().join("policies"),
            policies: RwLock::new(Vec::new()),
            telemetry,
            bus,
            start_time: Instant::now(),
            events_processed: Mutex::new(0),
        })
    }

    #[test]
    fn wildcard_exact() {
        assert!(wildcard_matches("boi.spec.completed", "boi.spec.completed"));
        assert!(!wildcard_matches("boi.spec.completed", "boi.spec.started"));
    }

    #[test]
    fn wildcard_star_suffix() {
        assert!(wildcard_matches("boi.spec.*", "boi.spec.completed"));
        assert!(wildcard_matches("boi.spec.*", "boi.spec.started"));
        assert!(!wildcard_matches("boi.spec.*", "boi.other.event"));
        assert!(!wildcard_matches("boi.spec.*", "boi.spec"));
    }

    #[test]
    fn wildcard_global() {
        assert!(wildcard_matches("*", "anything.at.all"));
    }

    #[test]
    fn ingest_writes_to_db() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let id = engine.ingest("test.event", &serde_json::json!({"x": 1}), "test");
        assert!(id > 0);

        let db = engine.db.lock().unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn condition_eq() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let payload = serde_json::json!({"status": "done"});
        let cond = Condition {
            field: "status".to_string(),
            op: "eq".to_string(),
            value: Some(Value::String("done".to_string())),
        };
        assert!(engine.evaluate_condition(&cond, &payload));
        let cond_no = Condition {
            value: Some(Value::String("pending".to_string())),
            ..cond
        };
        assert!(!engine.evaluate_condition(&cond_no, &payload));
    }

    #[test]
    fn condition_contains() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let payload = serde_json::json!({"msg": "hello world"});
        let cond = Condition {
            field: "msg".to_string(),
            op: "contains".to_string(),
            value: Some(Value::String("world".to_string())),
        };
        assert!(engine.evaluate_condition(&cond, &payload));
    }

    #[test]
    fn condition_exists() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let payload = serde_json::json!({"present": true});
        let present = Condition {
            field: "present".to_string(),
            op: "exists".to_string(),
            value: None,
        };
        let missing = Condition {
            field: "missing".to_string(),
            op: "exists".to_string(),
            value: None,
        };
        assert!(engine.evaluate_condition(&present, &payload));
        assert!(!engine.evaluate_condition(&missing, &payload));
    }

    #[test]
    fn render_template_substitution() {
        let payload = serde_json::json!({"spec_id": "q-911", "status": "done"});
        let result = render_template(
            "echo 'spec {{event.spec_id}} is {{event.status}}'",
            "boi.spec.completed",
            &payload,
        );
        assert_eq!(result, "echo 'spec q-911 is done'");
    }
}
