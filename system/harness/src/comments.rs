use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use crate::server::{Request, Response};
use crate::sse::SseBus;
use crate::telemetry::Telemetry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    #[serde(default)]
    pub asset: String,
    pub text: String,
    #[serde(default = "default_author")]
    pub author: String,
    pub status: String,
    pub created_at: String,
    #[serde(default)]
    pub action_log: Vec<ActionEntry>,
    #[serde(default)]
    pub routed_to: Vec<String>,
    #[serde(default)]
    pub related_assets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionEntry {
    pub ts: String,
    pub action: String,
}

fn default_author() -> String {
    "mike".to_string()
}

#[derive(Debug, Deserialize, Serialize)]
struct CommentsFile {
    comments: Vec<Comment>,
}

pub struct CommentsHandler {
    data_path: PathBuf,
    hex_dir: PathBuf,
    bus: Arc<SseBus>,
    telemetry: Arc<Telemetry>,
}

impl CommentsHandler {
    pub fn new(hex_dir: &Path, bus: Arc<SseBus>, telemetry: Arc<Telemetry>) -> Arc<Self> {
        let data_path = hex_dir.join(".hex/data/comments.json");
        if let Some(parent) = data_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Arc::new(Self {
            data_path,
            hex_dir: hex_dir.to_path_buf(),
            bus,
            telemetry,
        })
    }

    pub fn handle(&self, req: &Request) -> Response {
        let path = req.path.strip_prefix("/comments").unwrap_or(&req.path);
        let method = req.method.as_str();

        match (method, path) {
            ("GET", "/widget.js") => self.serve_widget_js(),
            ("GET", "/api/comments/all") => self.list_all(),
            ("GET", "/api/comments/pending") => self.list_pending(),
            ("GET", "/api/comments/summary") => self.summary(),
            ("GET", "/api/comments") => {
                let asset = req.query.get("asset").map(|s| s.as_str());
                self.list_by_asset(asset)
            }
            ("POST", "/api/comments") => self.create(req),
            ("POST", "/api/comments/update") => self.update(req),
            _ => json_error(404, "comments endpoint not found"),
        }
    }

    fn read_comments(&self) -> Result<Vec<Comment>, String> {
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
        let parsed: CommentsFile =
            serde_json::from_str(&content).map_err(|e| format!("parse failed: {e}"))?;
        Ok(parsed.comments)
    }

    fn write_comments(&self, comments: &[Comment]) -> Result<(), String> {
        let tmp_path = self.data_path.with_extension("json.tmp");
        let file = CommentsFile {
            comments: comments.to_vec(),
        };
        let json =
            serde_json::to_string_pretty(&file).map_err(|e| format!("serialize failed: {e}"))?;

        // Atomic write: write to tmp then rename under exclusive lock
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
        match self.read_comments() {
            Ok(comments) => json_ok(&comments),
            Err(e) => json_error(500, &e),
        }
    }

    fn list_pending(&self) -> Response {
        match self.read_comments() {
            Ok(comments) => {
                let pending: Vec<_> = comments
                    .into_iter()
                    .filter(|c| c.status == "new" || c.status == "seen")
                    .collect();
                json_ok(&pending)
            }
            Err(e) => json_error(500, &e),
        }
    }

    fn list_by_asset(&self, asset: Option<&str>) -> Response {
        match self.read_comments() {
            Ok(comments) => {
                let filtered: Vec<_> = match asset {
                    Some(a) => comments.into_iter().filter(|c| c.asset == a).collect(),
                    None => comments,
                };
                json_ok(&filtered)
            }
            Err(e) => json_error(500, &e),
        }
    }

    fn summary(&self) -> Response {
        match self.read_comments() {
            Ok(comments) => {
                let mut by_surface: HashMap<String, serde_json::Value> = HashMap::new();
                for c in &comments {
                    let entry = by_surface
                        .entry(c.asset.clone())
                        .or_insert_with(|| serde_json::json!({ "total": 0, "pending": 0 }));
                    let total = entry["total"].as_i64().unwrap_or(0);
                    let pending = entry["pending"].as_i64().unwrap_or(0);
                    let is_pending = c.status == "new" || c.status == "seen";
                    *entry = serde_json::json!({
                        "total": total + 1,
                        "pending": if is_pending { pending + 1 } else { pending },
                    });
                }
                json_ok(&by_surface)
            }
            Err(e) => json_error(500, &e),
        }
    }

    fn create(&self, req: &Request) -> Response {
        #[derive(Deserialize)]
        struct CreateBody {
            text: String,
            #[serde(default)]
            asset: String,
            #[serde(default = "default_author")]
            author: String,
        }

        let body: CreateBody = match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(e) => return json_error(400, &format!("invalid JSON: {e}")),
        };

        let id = format!("c-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let comment = Comment {
            id: id.clone(),
            asset: body.asset.clone(),
            text: body.text.clone(),
            author: body.author.clone(),
            status: "new".to_string(),
            created_at: Utc::now().to_rfc3339(),
            action_log: Vec::new(),
            routed_to: Vec::new(),
            related_assets: Vec::new(),
        };

        let mut comments = match self.read_comments() {
            Ok(c) => c,
            Err(e) => return json_error(500, &format!("read failed: {e}")),
        };
        comments.push(comment.clone());

        if let Err(e) = self.write_comments(&comments) {
            return json_error(500, &format!("write failed: {e}"));
        }

        self.bus.publish(
            "content.comments",
            "created",
            &serde_json::json!({
                "id": comment.id,
                "asset": comment.asset,
                "text": comment.text,
                "author": comment.author,
            }),
        );
        self.telemetry
            .emit("hex.comment.created", &serde_json::json!({ "id": id }));

        // Spawn route-comment.py in background (best-effort)
        let script = self.hex_dir.join(".hex/scripts/route-comment.py");
        if script.exists() {
            let _ = std::process::Command::new("python3")
                .arg(&script)
                .arg(&id)
                .spawn();
        }

        json_created(&comment)
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

        let mut comments = match self.read_comments() {
            Ok(c) => c,
            Err(e) => return json_error(500, &format!("read failed: {e}")),
        };

        let comment = match comments.iter_mut().find(|c| c.id == body.id) {
            Some(c) => c,
            None => return json_error(404, "comment not found"),
        };

        let mut changed_status = None;

        if let Some(status) = body.status {
            comment.status = status.clone();
            changed_status = Some(status);
        }
        if let Some(action) = body.action {
            comment.action_log.push(ActionEntry {
                ts: Utc::now().to_rfc3339(),
                action,
            });
        }
        if let Some(routed_to) = body.routed_to {
            comment.routed_to = routed_to;
        }
        if let Some(related_assets) = body.related_assets {
            comment.related_assets = related_assets;
        }

        let updated = comment.clone();

        if let Err(e) = self.write_comments(&comments) {
            return json_error(500, &format!("write failed: {e}"));
        }

        self.bus.publish(
            "content.comments",
            "status_changed",
            &serde_json::json!({
                "id": updated.id,
                "status": changed_status,
                "related_assets": updated.related_assets,
            }),
        );
        self.telemetry.emit(
            "hex.comment.updated",
            &serde_json::json!({ "id": updated.id }),
        );

        json_ok(&updated)
    }

    fn serve_widget_js(&self) -> Response {
        let widget_path = self
            .hex_dir
            .join(".hex/scripts/comments-service/widget.js");
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

    fn make_handler(dir: &Path) -> Arc<CommentsHandler> {
        let data_dir = dir.join(".hex/data");
        fs::create_dir_all(&data_dir).unwrap();
        let bus = SseBus::new();
        let telemetry = Arc::new(Telemetry::new(dir));
        CommentsHandler::new(dir, bus, telemetry)
    }

    fn make_comment(id: &str, asset: &str, status: &str) -> Comment {
        Comment {
            id: id.to_string(),
            asset: asset.to_string(),
            text: "test text".to_string(),
            author: "mike".to_string(),
            status: status.to_string(),
            created_at: Utc::now().to_rfc3339(),
            action_log: Vec::new(),
            routed_to: Vec::new(),
            related_assets: Vec::new(),
        }
    }

    #[test]
    fn read_write_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        let comments = vec![make_comment("c-001", "post:P-001", "new")];
        h.write_comments(&comments).unwrap();
        let read = h.read_comments().unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, "c-001");
    }

    #[test]
    fn list_pending_filters_status() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        h.write_comments(&[
            make_comment("c-001", "a", "new"),
            make_comment("c-002", "b", "seen"),
            make_comment("c-003", "c", "acknowledged"),
        ])
        .unwrap();
        let req = Request {
            method: "GET".to_string(),
            path: "/comments/api/comments/pending".to_string(),
            query: Default::default(),
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = h.handle(&req);
        assert_eq!(resp.status, 200);
        let val: Vec<Comment> = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(val.len(), 2);
    }

    #[test]
    fn list_by_asset_filters() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        h.write_comments(&[
            make_comment("c-001", "post:P-001", "new"),
            make_comment("c-002", "social", "new"),
        ])
        .unwrap();
        let mut query = HashMap::new();
        query.insert("asset".to_string(), "post:P-001".to_string());
        let req = Request {
            method: "GET".to_string(),
            path: "/comments/api/comments".to_string(),
            query,
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = h.handle(&req);
        assert_eq!(resp.status, 200);
        let val: Vec<Comment> = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(val.len(), 1);
        assert_eq!(val[0].asset, "post:P-001");
    }

    #[test]
    fn summary_groups_by_surface() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        h.write_comments(&[
            make_comment("c-001", "post:P-001", "new"),
            make_comment("c-002", "post:P-001", "acknowledged"),
            make_comment("c-003", "social", "seen"),
        ])
        .unwrap();
        let req = Request {
            method: "GET".to_string(),
            path: "/comments/api/comments/summary".to_string(),
            query: Default::default(),
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = h.handle(&req);
        assert_eq!(resp.status, 200);
        let val: HashMap<String, serde_json::Value> = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(val["post:P-001"]["total"], 2);
        assert_eq!(val["post:P-001"]["pending"], 1);
        assert_eq!(val["social"]["pending"], 1);
    }
}
