use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::server::{Request, Response};
use crate::sse::SseBus;
use crate::telemetry::Telemetry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub r#type: String,
    pub local_id: String,
    pub title: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub owner: String,
    pub registered_at: String,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct AssetsFile {
    version: u32,
    assets: Vec<Asset>,
}

pub struct AssetsHandler {
    data_path: PathBuf,
    bus: Arc<SseBus>,
    telemetry: Arc<Telemetry>,
}

impl AssetsHandler {
    pub fn new(hex_dir: &Path, bus: Arc<SseBus>, telemetry: Arc<Telemetry>) -> Arc<Self> {
        let data_path = hex_dir.join(".hex/data/assets.json");
        if let Some(parent) = data_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Arc::new(Self { data_path, bus, telemetry })
    }

    pub fn handle(&self, req: &Request) -> Response {
        let path = req.path.strip_prefix("/assets").unwrap_or(&req.path);
        let method = req.method.as_str();

        match (method, path) {
            ("GET", "/types") => self.types(),
            ("GET", "/list") => {
                let asset_type = req.query.get("type").map(|s| s.as_str());
                let owner = req.query.get("owner").map(|s| s.as_str());
                self.list(asset_type, owner)
            }
            ("GET", "/search") => {
                let q = req.query.get("q").map(|s| s.as_str()).unwrap_or("");
                self.search(q)
            }
            ("POST", "/register") => self.register(req),
            ("GET", p) if p.starts_with("/resolve/") => {
                let id = &p["/resolve/".len()..];
                self.resolve(id)
            }
            _ => json_error(404, "assets endpoint not found"),
        }
    }

    fn read_assets(&self) -> Result<Vec<Asset>, String> {
        if !self.data_path.exists() {
            return Ok(Vec::new());
        }
        let mut file =
            fs::File::open(&self.data_path).map_err(|e| format!("open failed: {e}"))?;
        file.lock_shared().map_err(|e| format!("lock failed: {e}"))?;
        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|e| format!("read failed: {e}"))?;
        file.unlock().map_err(|e| format!("unlock failed: {e}"))?;
        let parsed: AssetsFile =
            serde_json::from_str(&content).map_err(|e| format!("parse failed: {e}"))?;
        Ok(parsed.assets)
    }

    fn write_assets(&self, assets: &[Asset]) -> Result<(), String> {
        let tmp_path = self.data_path.with_extension("json.tmp");
        let file = AssetsFile { version: 1, assets: assets.to_vec() };
        let json =
            serde_json::to_string_pretty(&file).map_err(|e| format!("serialize failed: {e}"))?;

        {
            let mut f = fs::File::create(&tmp_path)
                .map_err(|e| format!("create tmp failed: {e}"))?;
            f.write_all(json.as_bytes())
                .map_err(|e| format!("write failed: {e}"))?;
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
        lock_file.unlock().map_err(|e| format!("unlock failed: {e}"))?;
        Ok(())
    }

    fn resolve(&self, id: &str) -> Response {
        match self.read_assets() {
            Ok(assets) => match assets.into_iter().find(|a| a.id == id) {
                Some(a) => json_ok(&a),
                None => json_error(404, &format!("asset not found: {}", id)),
            },
            Err(e) => json_error(500, &e),
        }
    }

    fn list(&self, asset_type: Option<&str>, owner: Option<&str>) -> Response {
        match self.read_assets() {
            Ok(assets) => {
                let filtered: Vec<_> = assets
                    .into_iter()
                    .filter(|a| asset_type.map_or(true, |t| a.r#type == t))
                    .filter(|a| owner.map_or(true, |o| a.owner == o))
                    .collect();
                json_ok(&filtered)
            }
            Err(e) => json_error(500, &e),
        }
    }

    fn types(&self) -> Response {
        match self.read_assets() {
            Ok(assets) => {
                let mut counts: HashMap<String, usize> = HashMap::new();
                for a in &assets {
                    *counts.entry(a.r#type.clone()).or_insert(0) += 1;
                }
                let mut sorted: Vec<_> = counts.into_iter().collect();
                sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                let result: Vec<_> = sorted
                    .into_iter()
                    .map(|(t, n)| serde_json::json!({ "type": t, "count": n }))
                    .collect();
                json_ok(&result)
            }
            Err(e) => json_error(500, &e),
        }
    }

    fn search(&self, query: &str) -> Response {
        if query.is_empty() {
            return json_error(400, "query parameter 'q' is required");
        }
        let q = query.to_lowercase();
        match self.read_assets() {
            Ok(assets) => {
                let matched: Vec<_> = assets
                    .into_iter()
                    .filter(|a| {
                        a.title.to_lowercase().contains(&q)
                            || a.id.to_lowercase().contains(&q)
                            || a.owner.to_lowercase().contains(&q)
                    })
                    .collect();
                json_ok(&matched)
            }
            Err(e) => json_error(500, &e),
        }
    }

    fn register(&self, req: &Request) -> Response {
        #[derive(Deserialize)]
        struct RegisterBody {
            r#type: String,
            local_id: String,
            title: String,
            #[serde(default)]
            path: Option<String>,
            #[serde(default)]
            url: Option<String>,
            #[serde(default)]
            owner: String,
            #[serde(default)]
            metadata: HashMap<String, Value>,
        }

        let body: RegisterBody = match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(e) => return json_error(400, &format!("invalid JSON: {e}")),
        };

        let id = format!("{}:{}", body.r#type, body.local_id);
        let mut assets = match self.read_assets() {
            Ok(a) => a,
            Err(e) => return json_error(500, &format!("read failed: {e}")),
        };

        let asset = Asset {
            id: id.clone(),
            r#type: body.r#type,
            local_id: body.local_id,
            title: body.title,
            path: body.path,
            url: body.url,
            owner: body.owner,
            registered_at: Utc::now().to_rfc3339(),
            metadata: body.metadata,
        };

        // Upsert: replace if exists, else append
        if let Some(pos) = assets.iter().position(|a| a.id == id) {
            assets[pos] = asset.clone();
        } else {
            assets.push(asset.clone());
        }

        if let Err(e) = self.write_assets(&assets) {
            return json_error(500, &format!("write failed: {e}"));
        }

        self.bus.publish(
            "content.assets",
            "registered",
            &serde_json::json!({
                "id": asset.id,
                "title": asset.title,
                "asset_type": asset.r#type,
            }),
        );
        self.telemetry
            .emit("hex.asset.registered", &serde_json::json!({ "id": id }));

        json_ok(&asset)
    }

    // CLI helpers — read JSON directly, no server required

    pub fn cli_resolve(&self, id: &str) {
        match self.read_assets() {
            Ok(assets) => match assets.into_iter().find(|a| a.id == id) {
                Some(a) => println!("{}", serde_json::to_string_pretty(&a).unwrap_or_default()),
                None => {
                    eprintln!("asset not found: {}", id);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("ERROR: {e}");
                std::process::exit(1);
            }
        }
    }

    pub fn cli_list(&self, asset_type: Option<&str>) {
        match self.read_assets() {
            Ok(assets) => {
                let filtered: Vec<_> = assets
                    .into_iter()
                    .filter(|a| asset_type.map_or(true, |t| a.r#type == t))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&filtered).unwrap_or_default());
            }
            Err(e) => {
                eprintln!("ERROR: {e}");
                std::process::exit(1);
            }
        }
    }

    pub fn cli_search(&self, query: &str) {
        let q = query.to_lowercase();
        match self.read_assets() {
            Ok(assets) => {
                let matched: Vec<_> = assets
                    .into_iter()
                    .filter(|a| {
                        a.title.to_lowercase().contains(&q)
                            || a.id.to_lowercase().contains(&q)
                            || a.owner.to_lowercase().contains(&q)
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&matched).unwrap_or_default());
            }
            Err(e) => {
                eprintln!("ERROR: {e}");
                std::process::exit(1);
            }
        }
    }

    pub fn cli_types(&self) {
        match self.read_assets() {
            Ok(assets) => {
                let mut counts: HashMap<String, usize> = HashMap::new();
                for a in &assets {
                    *counts.entry(a.r#type.clone()).or_insert(0) += 1;
                }
                let mut sorted: Vec<_> = counts.into_iter().collect();
                sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                for (t, n) in sorted {
                    println!("{}: {}", t, n);
                }
            }
            Err(e) => {
                eprintln!("ERROR: {e}");
                std::process::exit(1);
            }
        }
    }

    pub fn cli_register(&self, asset_type: &str, local_id: &str, title: &str, path: Option<&str>) {
        let id = format!("{}:{}", asset_type, local_id);
        let mut assets = match self.read_assets() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("ERROR reading assets: {e}");
                std::process::exit(1);
            }
        };

        let asset = Asset {
            id: id.clone(),
            r#type: asset_type.to_string(),
            local_id: local_id.to_string(),
            title: title.to_string(),
            path: path.map(|s| s.to_string()),
            url: None,
            owner: String::new(),
            registered_at: Utc::now().to_rfc3339(),
            metadata: HashMap::new(),
        };

        if let Some(pos) = assets.iter().position(|a| a.id == id) {
            assets[pos] = asset.clone();
        } else {
            assets.push(asset.clone());
        }

        if let Err(e) = self.write_assets(&assets) {
            eprintln!("ERROR writing assets: {e}");
            std::process::exit(1);
        }

        println!("{}", serde_json::to_string_pretty(&asset).unwrap_or_default());
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

    fn make_handler(dir: &Path) -> Arc<AssetsHandler> {
        let data_dir = dir.join(".hex/data");
        fs::create_dir_all(&data_dir).unwrap();
        let bus = SseBus::new();
        let telemetry = Arc::new(Telemetry::new(dir));
        AssetsHandler::new(dir, bus, telemetry)
    }

    fn make_asset(id: &str, asset_type: &str, title: &str, owner: &str) -> Asset {
        Asset {
            id: id.to_string(),
            r#type: asset_type.to_string(),
            local_id: id.split(':').nth(1).unwrap_or(id).to_string(),
            title: title.to_string(),
            path: None,
            url: None,
            owner: owner.to_string(),
            registered_at: Utc::now().to_rfc3339(),
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn read_write_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        let assets = vec![make_asset("post:P-001", "post", "Test Post", "brand")];
        h.write_assets(&assets).unwrap();
        let read = h.read_assets().unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, "post:P-001");
    }

    #[test]
    fn resolve_existing_asset() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        h.write_assets(&[make_asset("post:P-001", "post", "Test Post", "brand")])
            .unwrap();
        let req = Request {
            method: "GET".to_string(),
            path: "/assets/resolve/post:P-001".to_string(),
            query: Default::default(),
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = h.handle(&req);
        assert_eq!(resp.status, 200);
        let val: Asset = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(val.id, "post:P-001");
    }

    #[test]
    fn resolve_missing_asset_returns_404() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        h.write_assets(&[]).unwrap();
        let req = Request {
            method: "GET".to_string(),
            path: "/assets/resolve/post:P-999".to_string(),
            query: Default::default(),
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = h.handle(&req);
        assert_eq!(resp.status, 404);
    }

    #[test]
    fn list_filters_by_type() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        h.write_assets(&[
            make_asset("post:P-001", "post", "Post 1", "brand"),
            make_asset("image:I-001", "image", "Image 1", "brand"),
            make_asset("post:P-002", "post", "Post 2", "brand"),
        ])
        .unwrap();
        let mut query = HashMap::new();
        query.insert("type".to_string(), "post".to_string());
        let req = Request {
            method: "GET".to_string(),
            path: "/assets/list".to_string(),
            query,
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = h.handle(&req);
        assert_eq!(resp.status, 200);
        let val: Vec<Asset> = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(val.len(), 2);
        assert!(val.iter().all(|a| a.r#type == "post"));
    }

    #[test]
    fn types_returns_counts() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        h.write_assets(&[
            make_asset("post:P-001", "post", "Post 1", "brand"),
            make_asset("post:P-002", "post", "Post 2", "brand"),
            make_asset("image:I-001", "image", "Image 1", "brand"),
        ])
        .unwrap();
        let req = Request {
            method: "GET".to_string(),
            path: "/assets/types".to_string(),
            query: Default::default(),
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = h.handle(&req);
        assert_eq!(resp.status, 200);
        let val: Vec<serde_json::Value> = serde_json::from_slice(&resp.body).unwrap();
        let post_entry = val.iter().find(|v| v["type"] == "post").unwrap();
        assert_eq!(post_entry["count"], 2);
    }

    #[test]
    fn search_matches_title() {
        let tmp = TempDir::new().unwrap();
        let h = make_handler(tmp.path());
        h.write_assets(&[
            make_asset("post:P-001", "post", "Hello World", "brand"),
            make_asset("post:P-002", "post", "Goodbye World", "brand"),
        ])
        .unwrap();
        let mut query = HashMap::new();
        query.insert("q".to_string(), "hello".to_string());
        let req = Request {
            method: "GET".to_string(),
            path: "/assets/search".to_string(),
            query,
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = h.handle(&req);
        assert_eq!(resp.status, 200);
        let val: Vec<Asset> = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(val.len(), 1);
        assert_eq!(val[0].id, "post:P-001");
    }
}
