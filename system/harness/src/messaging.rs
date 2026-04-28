use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use crate::server::{Request, Response};
use crate::sse::SseBus;
use crate::telemetry::Telemetry;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub msg_type: MessageType,
    pub from: String,
    pub to: Vec<String>,
    pub content: String,
    #[serde(default)]
    pub anchor: Option<String>,
    pub status: String,
    pub created_at: String,
    #[serde(default)]
    pub action_log: Vec<ActionEntry>,
    #[serde(default)]
    pub routed_to: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct MessagesFile {
    version: u32,
    messages: Vec<Message>,
}

pub struct MessagingHandler {
    data_path: PathBuf,
    hex_dir: PathBuf,
    bus: Arc<SseBus>,
    telemetry: Arc<Telemetry>,
}

impl MessagingHandler {
    pub fn new(hex_dir: &Path, bus: Arc<SseBus>, telemetry: Arc<Telemetry>) -> Arc<Self> {
        let data_path = hex_dir.join(".hex/data/messages.json");
        if let Some(parent) = data_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let handler = Arc::new(Self {
            data_path,
            hex_dir: hex_dir.to_path_buf(),
            bus,
            telemetry,
        });
        handler.migrate_if_needed();
        handler
    }

    fn migrate_if_needed(&self) {
        if self.data_path.exists() {
            return;
        }

        let mut messages: Vec<Message> = Vec::new();

        // Migrate comments.json → messages
        let comments_path = self.hex_dir.join(".hex/data/comments.json");
        if comments_path.exists() {
            #[derive(Deserialize)]
            struct OldActionEntry {
                ts: String,
                action: String,
            }
            #[derive(Deserialize)]
            struct OldComment {
                id: String,
                #[serde(default)]
                asset: String,
                text: String,
                #[serde(default = "default_mike")]
                author: String,
                status: String,
                created_at: String,
                #[serde(default)]
                action_log: Vec<OldActionEntry>,
                #[serde(default)]
                routed_to: Vec<String>,
            }
            #[derive(Deserialize)]
            struct OldCommentsFile {
                comments: Vec<OldComment>,
            }
            fn default_mike() -> String {
                "mike".to_string()
            }

            if let Ok(content) = fs::read_to_string(&comments_path) {
                if let Ok(parsed) = serde_json::from_str::<OldCommentsFile>(&content) {
                    let count = parsed.comments.len();
                    for c in parsed.comments {
                        let anchor = if c.asset.is_empty() { None } else { Some(c.asset) };
                        messages.push(Message {
                            id: c.id,
                            msg_type: MessageType::Comment,
                            from: c.author,
                            to: c.routed_to.clone(),
                            content: c.text,
                            anchor,
                            status: c.status,
                            created_at: c.created_at,
                            action_log: c.action_log.into_iter().map(|e| ActionEntry {
                                ts: e.ts,
                                action: e.action,
                                related_assets: Vec::new(),
                            }).collect(),
                            routed_to: c.routed_to,
                        });
                    }
                    eprintln!("messaging: migrated {} comments from comments.json", count);
                }
            }
        }

        // Migrate agent JSONL inboxes → messages
        let inbox_dir = self.hex_dir.join(".hex/messages");
        if inbox_dir.is_dir() {
            let mut inbox_count = 0usize;
            if let Ok(entries) = fs::read_dir(&inbox_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                        let agent_id = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        if let Ok(file) = fs::File::open(&path) {
                            for line in std::io::BufReader::new(file).lines() {
                                if let Ok(text) = line {
                                    let text = text.trim().to_string();
                                    if text.is_empty() {
                                        continue;
                                    }
                                    if let Ok(old) = serde_json::from_str::<crate::types::Message>(&text) {
                                        let raw_id = old.id.clone();
                                        let short = &raw_id[..raw_id.len().min(6)];
                                        messages.push(Message {
                                            id: format!("M{}", short),
                                            msg_type: MessageType::Agent,
                                            from: old.from,
                                            to: vec![agent_id.clone()],
                                            content: format!("{}: {}", old.subject, old.body),
                                            anchor: None,
                                            status: "new".to_string(),
                                            created_at: old.sent_at.to_rfc3339(),
                                            action_log: Vec::new(),
                                            routed_to: Vec::new(),
                                        });
                                        inbox_count += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if inbox_count > 0 {
                eprintln!("messaging: migrated {} agent inbox messages", inbox_count);
            }
        }

        if let Err(e) = self.write_messages(&messages) {
            eprintln!("messaging: migration write failed: {e}");
        } else {
            eprintln!("messaging: initialized messages.json ({} messages)", messages.len());
        }
    }

    pub fn handle(&self, req: &Request) -> Response {
        let path = req.path.strip_prefix("/messages").unwrap_or(&req.path);
        let method = req.method.as_str();

        match (method, path) {
            ("GET", "/widget.js") => self.serve_widget_js(),
            ("GET", "/api/messages/all") => self.list_all(),
            ("GET", "/api/messages/pending") => self.list_pending(),
            ("GET", "/api/messages/summary") => self.summary(),
            ("GET", "/api/messages") => {
                let msg_type = req.query.get("type").map(|s| s.as_str());
                let anchor = req.query.get("anchor").map(|s| s.as_str());
                let status = req.query.get("status").map(|s| s.as_str());
                self.list_filtered(msg_type, anchor, status)
            }
            ("POST", "/api/messages") => self.create(req),
            ("POST", "/api/messages/update") => self.update(req),
            _ => json_error(404, "messages endpoint not found"),
        }
    }

    fn read_messages(&self) -> Result<Vec<Message>, String> {
        if !self.data_path.exists() {
            return Ok(Vec::new());
        }
        let mut file = fs::File::open(&self.data_path)
            .map_err(|e| format!("open failed: {e}"))?;
        file.lock_shared().map_err(|e| format!("lock failed: {e}"))?;
        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|e| format!("read failed: {e}"))?;
        file.unlock().map_err(|e| format!("unlock failed: {e}"))?;
        let parsed: MessagesFile =
            serde_json::from_str(&content).map_err(|e| format!("parse failed: {e}"))?;
        Ok(parsed.messages)
    }

    fn write_messages(&self, messages: &[Message]) -> Result<(), String> {
        let tmp_path = self.data_path.with_extension("json.tmp");
        let file = MessagesFile {
            version: 1,
            messages: messages.to_vec(),
        };
        let json =
            serde_json::to_string_pretty(&file).map_err(|e| format!("serialize failed: {e}"))?;

        {
            let mut f = fs::File::create(&tmp_path)
                .map_err(|e| format!("create tmp failed: {e}"))?;
            f.write_all(json.as_bytes())
                .map_err(|e| format!("write tmp failed: {e}"))?;
        }

        let lock_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&self.data_path)
            .map_err(|e| format!("lock open failed: {e}"))?;
        lock_file
            .lock_exclusive()
            .map_err(|e| format!("exclusive lock failed: {e}"))?;
        fs::rename(&tmp_path, &self.data_path)
            .map_err(|e| format!("rename failed: {e}"))?;
        lock_file
            .unlock()
            .map_err(|e| format!("unlock failed: {e}"))?;
        Ok(())
    }

    fn list_all(&self) -> Response {
        match self.read_messages() {
            Ok(messages) => json_ok(&messages),
            Err(e) => json_error(500, &e),
        }
    }

    fn list_pending(&self) -> Response {
        match self.read_messages() {
            Ok(messages) => {
                let pending: Vec<_> = messages
                    .into_iter()
                    .filter(|m| m.status == "new" || m.status == "seen")
                    .collect();
                json_ok(&pending)
            }
            Err(e) => json_error(500, &e),
        }
    }

    fn list_filtered(
        &self,
        msg_type: Option<&str>,
        anchor: Option<&str>,
        status: Option<&str>,
    ) -> Response {
        match self.read_messages() {
            Ok(messages) => {
                let filtered: Vec<_> = messages
                    .into_iter()
                    .filter(|m| {
                        if let Some(t) = msg_type {
                            if m.msg_type.to_string() != t {
                                return false;
                            }
                        }
                        if let Some(a) = anchor {
                            if m.anchor.as_deref() != Some(a) {
                                return false;
                            }
                        }
                        if let Some(s) = status {
                            if m.status != s {
                                return false;
                            }
                        }
                        true
                    })
                    .collect();
                json_ok(&filtered)
            }
            Err(e) => json_error(500, &e),
        }
    }

    fn summary(&self) -> Response {
        match self.read_messages() {
            Ok(messages) => {
                let mut by_type: HashMap<String, serde_json::Value> = HashMap::new();
                for m in &messages {
                    let key = m.msg_type.to_string();
                    let entry = by_type
                        .entry(key)
                        .or_insert_with(|| serde_json::json!({ "total": 0, "pending": 0 }));
                    let total = entry["total"].as_i64().unwrap_or(0);
                    let pending = entry["pending"].as_i64().unwrap_or(0);
                    let is_pending = m.status == "new" || m.status == "seen";
                    *entry = serde_json::json!({
                        "total": total + 1,
                        "pending": if is_pending { pending + 1 } else { pending },
                    });
                }
                json_ok(&by_type)
            }
            Err(e) => json_error(500, &e),
        }
    }

    fn create(&self, req: &Request) -> Response {
        #[derive(Deserialize)]
        struct CreateBody {
            #[serde(default)]
            msg_type: Option<String>,
            from: String,
            #[serde(default)]
            to: Vec<String>,
            content: String,
            #[serde(default)]
            anchor: Option<String>,
        }

        let body: CreateBody = match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(e) => return json_error(400, &format!("invalid JSON: {e}")),
        };

        let mt: MessageType = match body.msg_type.as_deref().unwrap_or("agent").parse() {
            Ok(t) => t,
            Err(e) => return json_error(400, &e),
        };

        let id = format!("M{}", &Uuid::new_v4().simple().to_string()[..6]);
        let message = Message {
            id: id.clone(),
            msg_type: mt.clone(),
            from: body.from.clone(),
            to: body.to.clone(),
            content: body.content.clone(),
            anchor: body.anchor.clone(),
            status: "new".to_string(),
            created_at: Utc::now().to_rfc3339(),
            action_log: Vec::new(),
            routed_to: Vec::new(),
        };

        let mut messages = match self.read_messages() {
            Ok(m) => m,
            Err(e) => return json_error(500, &format!("read failed: {e}")),
        };
        messages.push(message.clone());

        if let Err(e) = self.write_messages(&messages) {
            return json_error(500, &format!("write failed: {e}"));
        }

        self.bus.publish(
            "content.messages",
            "created",
            &serde_json::json!({
                "id": message.id,
                "msg_type": message.msg_type.to_string(),
                "from": message.from,
                "to": message.to,
                "content": message.content,
                "anchor": message.anchor,
            }),
        );
        self.telemetry.emit(
            "hex.message.created",
            &serde_json::json!({ "id": id, "msg_type": mt.to_string() }),
        );

        if mt == MessageType::Comment {
            let script = self.hex_dir.join(".hex/scripts/route-comment.py");
            if script.exists() {
                let _ = std::process::Command::new("python3").arg(&script).arg(&id).spawn();
            }
        }

        json_created(&message)
    }

    fn update(&self, req: &Request) -> Response {
        #[derive(Deserialize)]
        struct UpdateBody {
            id: String,
            #[serde(default)]
            status: Option<String>,
            #[serde(default)]
            action: Option<String>,
            #[serde(default)]
            routed_to: Option<Vec<String>>,
            #[serde(default)]
            related_assets: Option<Vec<String>>,
        }

        let body: UpdateBody = match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(e) => return json_error(400, &format!("invalid JSON: {e}")),
        };

        let mut messages = match self.read_messages() {
            Ok(m) => m,
            Err(e) => return json_error(500, &format!("read failed: {e}")),
        };

        let message = match messages.iter_mut().find(|m| m.id == body.id) {
            Some(m) => m,
            None => return json_error(404, "message not found"),
        };

        let mut changed_status = None;

        if let Some(status) = body.status {
            message.status = status.clone();
            changed_status = Some(status);
        }
        if let Some(action) = body.action {
            message.action_log.push(ActionEntry {
                ts: Utc::now().to_rfc3339(),
                action,
                related_assets: body.related_assets.clone().unwrap_or_default(),
            });
        }
        if let Some(routed_to) = body.routed_to {
            message.routed_to = routed_to;
        }

        let updated = message.clone();

        if let Err(e) = self.write_messages(&messages) {
            return json_error(500, &format!("write failed: {e}"));
        }

        self.bus.publish(
            "content.messages",
            "status_changed",
            &serde_json::json!({
                "id": updated.id,
                "status": changed_status,
                "related_assets": updated.action_log.last().map(|e| e.related_assets.clone()).unwrap_or_default(),
            }),
        );
        self.telemetry
            .emit("hex.message.updated", &serde_json::json!({ "id": updated.id }));

        json_ok(&updated)
    }

    fn serve_widget_js(&self) -> Response {
        let widget_path = self.hex_dir.join(".hex/scripts/comments-service/widget.js");
        match fs::read(&widget_path) {
            Ok(data) => Response {
                status: 200,
                content_type: "application/javascript".to_string(),
                headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
                body: data,
            },
            Err(_) => json_error(404, "widget.js not found"),
        }
    }

    // ── CLI helpers ───────────────────────────────────────────────────────────

    pub fn cli_send(
        &self,
        from: &str,
        to: Vec<String>,
        content: &str,
        msg_type: &str,
        anchor: Option<&str>,
    ) {
        let mt: MessageType = match msg_type.parse() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Invalid msg_type: {e}");
                std::process::exit(1);
            }
        };

        let id = format!("M{}", &Uuid::new_v4().simple().to_string()[..6]);
        let message = Message {
            id: id.clone(),
            msg_type: mt.clone(),
            from: from.to_string(),
            to: to.clone(),
            content: content.to_string(),
            anchor: anchor.map(|s| s.to_string()),
            status: "new".to_string(),
            created_at: Utc::now().to_rfc3339(),
            action_log: Vec::new(),
            routed_to: Vec::new(),
        };

        let mut messages = match self.read_messages() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("read failed: {e}");
                std::process::exit(1);
            }
        };
        messages.push(message.clone());

        if let Err(e) = self.write_messages(&messages) {
            eprintln!("write failed: {e}");
            std::process::exit(1);
        }

        self.bus.publish(
            "content.messages",
            "created",
            &serde_json::json!({
                "id": message.id,
                "msg_type": mt.to_string(),
                "from": from,
                "to": to,
                "content": content,
            }),
        );
        self.telemetry
            .emit("hex.message.created", &serde_json::json!({ "id": id }));

        println!(
            "Sent message {} ({}) from {} to {}",
            id,
            mt,
            from,
            to.join(",")
        );
    }

    pub fn cli_list(
        &self,
        msg_type: Option<&str>,
        status: Option<&str>,
        anchor: Option<&str>,
    ) {
        let messages = match self.read_messages() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Error reading messages: {e}");
                std::process::exit(1);
            }
        };

        let filtered: Vec<_> = messages
            .iter()
            .filter(|m| {
                if let Some(t) = msg_type {
                    if m.msg_type.to_string() != t {
                        return false;
                    }
                }
                if let Some(s) = status {
                    if m.status != s {
                        return false;
                    }
                }
                if let Some(a) = anchor {
                    if m.anchor.as_deref() != Some(a) {
                        return false;
                    }
                }
                true
            })
            .collect();

        if filtered.is_empty() {
            println!("No messages found");
            return;
        }

        for m in &filtered {
            println!(
                "{} [{}] from={} to={} status={} anchor={} created={}",
                m.id,
                m.msg_type,
                m.from,
                m.to.join(","),
                m.status,
                m.anchor.as_deref().unwrap_or("-"),
                m.created_at,
            );
            println!("  {}", m.content);
        }
        println!("\n{} messages", filtered.len());
    }

    pub fn cli_respond(
        &self,
        id: &str,
        status: &str,
        action: Option<&str>,
        assets: Vec<String>,
    ) {
        let mut messages = match self.read_messages() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("read failed: {e}");
                std::process::exit(1);
            }
        };

        let message = match messages.iter_mut().find(|m| m.id == id) {
            Some(m) => m,
            None => {
                eprintln!("message not found: {id}");
                std::process::exit(1);
            }
        };

        message.status = status.to_string();
        if let Some(a) = action {
            message.action_log.push(ActionEntry {
                ts: Utc::now().to_rfc3339(),
                action: a.to_string(),
                related_assets: assets,
            });
        }
        let updated = message.clone();

        if let Err(e) = self.write_messages(&messages) {
            eprintln!("write failed: {e}");
            std::process::exit(1);
        }

        self.telemetry
            .emit("hex.message.updated", &serde_json::json!({ "id": id }));
        println!("Updated message {} status={}", updated.id, updated.status);
    }
}

fn json_ok<T: Serialize>(val: &T) -> Response {
    let body = serde_json::to_vec(val).unwrap_or_default();
    Response {
        status: 200,
        content_type: "application/json".to_string(),
        headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
        body,
    }
}

fn json_created<T: Serialize>(val: &T) -> Response {
    let body = serde_json::to_vec(val).unwrap_or_default();
    Response {
        status: 201,
        content_type: "application/json".to_string(),
        headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
        body,
    }
}

fn json_error(status: u16, msg: &str) -> Response {
    let body = serde_json::to_vec(&serde_json::json!({ "error": msg })).unwrap_or_default();
    Response {
        status,
        content_type: "application/json".to_string(),
        headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_handler(dir: &Path) -> Arc<MessagingHandler> {
        let data_dir = dir.join(".hex/data");
        fs::create_dir_all(&data_dir).unwrap();
        let bus = SseBus::new();
        let telemetry = Arc::new(Telemetry::new(dir));
        MessagingHandler::new(dir, bus, telemetry)
    }

    fn make_message(id: &str, msg_type: MessageType, from: &str, status: &str) -> Message {
        Message {
            id: id.to_string(),
            msg_type,
            from: from.to_string(),
            to: vec!["brand".to_string()],
            content: "test content".to_string(),
            anchor: None,
            status: status.to_string(),
            created_at: Utc::now().to_rfc3339(),
            action_log: Vec::new(),
            routed_to: Vec::new(),
        }
    }

    #[test]
    fn read_write_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        let messages = vec![make_message("M001", MessageType::Agent, "mike", "new")];
        h.write_messages(&messages).unwrap();
        let read = h.read_messages().unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, "M001");
        assert_eq!(read[0].msg_type, MessageType::Agent);
    }

    #[test]
    fn list_pending_filters_status() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        h.write_messages(&[
            make_message("M001", MessageType::Agent, "mike", "new"),
            make_message("M002", MessageType::Comment, "mike", "seen"),
            make_message("M003", MessageType::Agent, "brand", "done"),
        ])
        .unwrap();
        let req = Request {
            method: "GET".to_string(),
            path: "/messages/api/messages/pending".to_string(),
            query: Default::default(),
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = h.handle(&req);
        assert_eq!(resp.status, 200);
        let val: Vec<Message> = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(val.len(), 2);
    }

    #[test]
    fn list_filtered_by_type() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        h.write_messages(&[
            make_message("M001", MessageType::Agent, "mike", "new"),
            make_message("M002", MessageType::Comment, "mike", "new"),
            make_message("M003", MessageType::Notification, "system", "new"),
        ])
        .unwrap();
        let mut query = HashMap::new();
        query.insert("type".to_string(), "comment".to_string());
        let req = Request {
            method: "GET".to_string(),
            path: "/messages/api/messages".to_string(),
            query,
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = h.handle(&req);
        assert_eq!(resp.status, 200);
        let val: Vec<Message> = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(val.len(), 1);
        assert_eq!(val[0].msg_type, MessageType::Comment);
    }

    #[test]
    fn summary_groups_by_type() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        h.write_messages(&[
            make_message("M001", MessageType::Agent, "mike", "new"),
            make_message("M002", MessageType::Agent, "mike", "done"),
            make_message("M003", MessageType::Comment, "mike", "new"),
        ])
        .unwrap();
        let req = Request {
            method: "GET".to_string(),
            path: "/messages/api/messages/summary".to_string(),
            query: Default::default(),
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = h.handle(&req);
        assert_eq!(resp.status, 200);
        let val: HashMap<String, serde_json::Value> = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(val["agent"]["total"], 2);
        assert_eq!(val["agent"]["pending"], 1);
        assert_eq!(val["comment"]["total"], 1);
        assert_eq!(val["comment"]["pending"], 1);
    }

    #[test]
    fn message_type_serde_roundtrip() {
        let mt = MessageType::Comment;
        let json = serde_json::to_string(&mt).unwrap();
        assert_eq!(json, "\"comment\"");
        let parsed: MessageType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, MessageType::Comment);
    }

    #[test]
    fn migrate_from_comments_json() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join(".hex/data");
        fs::create_dir_all(&data_dir).unwrap();

        // Write old comments.json
        let old_comments = serde_json::json!({
            "comments": [
                {
                    "id": "c-001",
                    "asset": "post:P-001",
                    "text": "Hello world",
                    "author": "mike",
                    "status": "new",
                    "created_at": "2026-01-01T00:00:00Z",
                    "action_log": [],
                    "routed_to": [],
                    "related_assets": []
                }
            ]
        });
        fs::write(
            data_dir.join("comments.json"),
            serde_json::to_string_pretty(&old_comments).unwrap(),
        )
        .unwrap();

        // Handler creation triggers migration
        let bus = SseBus::new();
        let telemetry = Arc::new(Telemetry::new(tmp.path()));
        let h = MessagingHandler::new(tmp.path(), bus, telemetry);

        let messages = h.read_messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "c-001");
        assert_eq!(messages[0].msg_type, MessageType::Comment);
        assert_eq!(messages[0].content, "Hello world");
        assert_eq!(messages[0].anchor, Some("post:P-001".to_string()));
    }
}
