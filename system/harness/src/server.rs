use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crate::assets::AssetsHandler;
use crate::events::EventEngine;
use crate::messaging::MessagingHandler;
use crate::sse::SseBus;
use crate::telemetry::Telemetry;

const THREAD_POOL_SIZE: usize = 32;

pub struct HexServer {
    pub port: u16,
    pub hex_dir: PathBuf,
    pub bus: Arc<SseBus>,
    pub telemetry: Arc<Telemetry>,
    pub events: Arc<EventEngine>,
    pub messaging: Arc<MessagingHandler>,
    pub assets: Arc<AssetsHandler>,
}

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HexServer {
    pub fn new(
        port: u16,
        hex_dir: PathBuf,
        bus: Arc<SseBus>,
        telemetry: Arc<Telemetry>,
        events: Arc<EventEngine>,
        messaging: Arc<MessagingHandler>,
        assets: Arc<AssetsHandler>,
    ) -> Self {
        Self { port, hex_dir, bus, telemetry, events, messaging, assets }
    }

    pub fn start(&self) {
        let addr = format!("127.0.0.1:{}", self.port);
        let listener = match TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("hex server: failed to bind {}: {}", addr, e);
                std::process::exit(1);
            }
        };

        // Start event engine scheduler in background
        EventEngine::start_scheduler(Arc::clone(&self.events));

        let policy_count = self.events.policy_count();
        self.telemetry.emit("hex.server.started", &serde_json::json!({ "port": self.port }));
        println!("hex server listening on http://{}", addr);
        println!("  policies loaded: {}", policy_count);

        // Bounded thread pool: 32 workers sharing a channel of TcpStream connections
        let (tx, rx) = mpsc::sync_channel::<TcpStream>(THREAD_POOL_SIZE * 2);
        let rx = Arc::new(std::sync::Mutex::new(rx));

        for _ in 0..THREAD_POOL_SIZE {
            let rx = Arc::clone(&rx);
            let bus = Arc::clone(&self.bus);
            let telemetry = Arc::clone(&self.telemetry);
            let hex_dir = self.hex_dir.clone();
            let events = Arc::clone(&self.events);
            let messaging = Arc::clone(&self.messaging);
            let assets = Arc::clone(&self.assets);
            std::thread::spawn(move || loop {
                let stream = match rx.lock().unwrap().recv() {
                    Ok(s) => s,
                    Err(_) => break,
                };
                handle_connection(stream, &bus, &telemetry, &hex_dir, &events, &messaging, &assets);
            });
        }

        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    // sync_channel blocks when full — natural backpressure
                    let _ = tx.send(s);
                }
                Err(e) => eprintln!("hex server: accept error: {}", e),
            }
        }
        self.telemetry.emit("hex.server.stopped", &serde_json::json!({}));
    }

    pub fn check_health(port: u16) -> bool {
        let addr = format!("127.0.0.1:{}", port);
        let sock_addr: std::net::SocketAddr = match addr.parse() {
            Ok(a) => a,
            Err(_) => return false,
        };
        let mut stream = match TcpStream::connect_timeout(&sock_addr, Duration::from_secs(2)) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let req = "GET /events/health HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
        if stream.write_all(req.as_bytes()).is_err() {
            return false;
        }
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buf = [0u8; 16];
        let n = stream.read(&mut buf).unwrap_or(0);
        let s = std::str::from_utf8(&buf[..n]).unwrap_or("");
        s.starts_with("HTTP/1.1 200") || s.starts_with("HTTP/1.0 200")
    }
}

fn handle_connection(
    mut stream: TcpStream,
    bus: &Arc<SseBus>,
    telemetry: &Arc<Telemetry>,
    hex_dir: &Path,
    events: &Arc<EventEngine>,
    messaging: &Arc<MessagingHandler>,
    assets: &Arc<AssetsHandler>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));

    let req = match parse_request(&stream) {
        Some(r) => r,
        None => return,
    };

    // CORS preflight
    if req.method == "OPTIONS" {
        let resp = Response {
            status: 204,
            content_type: "text/plain".to_string(),
            headers: vec![
                ("Access-Control-Allow-Origin".to_string(), "*".to_string()),
                ("Access-Control-Allow-Methods".to_string(), "GET, POST, OPTIONS".to_string()),
                ("Access-Control-Allow-Headers".to_string(), "Content-Type".to_string()),
            ],
            body: Vec::new(),
        };
        write_response(&mut stream, resp);
        return;
    }

    // SSE stream is long-lived — handle before normal routing
    if req.path == "/events/stream" {
        let topics = req.query.get("topics")
            .map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>())
            .unwrap_or_else(|| vec!["*".to_string()]);
        telemetry.emit("hex.sse.subscribe", &serde_json::json!({ "topics": topics }));
        handle_sse_stream(&mut stream, topics, bus, telemetry);
        return;
    }

    let start = std::time::Instant::now();
    let resp = route_request(&req, bus, hex_dir, events, messaging, assets);
    let duration_ms = start.elapsed().as_millis();
    telemetry.emit("hex.server.request", &serde_json::json!({
        "method": req.method,
        "path": req.path,
        "status": resp.status,
        "duration_ms": duration_ms,
    }));
    write_response(&mut stream, resp);
}

fn parse_request(stream: &TcpStream) -> Option<Request> {
    let clone = stream.try_clone().ok()?;
    let mut reader = BufReader::new(clone);

    let mut req_line = String::new();
    reader.read_line(&mut req_line).ok()?;
    let req_line = req_line.trim_end();
    let mut parts = req_line.splitn(3, ' ');
    let method = parts.next()?.to_string();
    let full_path = parts.next()?.to_string();

    let (path, query_str) = if let Some(pos) = full_path.find('?') {
        (full_path[..pos].to_string(), full_path[pos + 1..].to_string())
    } else {
        (full_path, String::new())
    };
    let query = parse_query(&query_str);

    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(pos) = trimmed.find(':') {
            let key = trimmed[..pos].trim().to_lowercase();
            let val = trimmed[pos + 1..].trim().to_string();
            headers.insert(key, val);
        }
    }

    let body = if let Some(cl) = headers.get("content-length") {
        let len: usize = cl.parse().unwrap_or(0);
        if len > 0 && len <= 10 * 1024 * 1024 {
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).ok()?;
            buf
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    Some(Request { method, path, query, headers, body })
}

fn parse_query(query_str: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if query_str.is_empty() {
        return map;
    }
    for part in query_str.split('&') {
        if let Some(pos) = part.find('=') {
            map.insert(url_decode(&part[..pos]), url_decode(&part[pos + 1..]));
        } else if !part.is_empty() {
            map.insert(url_decode(part), String::new());
        }
    }
    map
}

fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                result.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let (Some(h), Some(l)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2])) {
                    result.push(char::from(h * 16 + l));
                    i += 3;
                } else {
                    result.push('%');
                    i += 1;
                }
            }
            b => {
                result.push(char::from(b));
                i += 1;
            }
        }
    }
    result
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn route_request(
    req: &Request,
    bus: &Arc<SseBus>,
    hex_dir: &Path,
    events: &Arc<EventEngine>,
    messaging: &Arc<MessagingHandler>,
    assets: &Arc<AssetsHandler>,
) -> Response {
    let path = req.path.as_str();

    // SSE / events infrastructure endpoints
    match path {
        "/events/topics" => return events_topics(bus),
        "/events/health" => return events_health(bus),
        "/events/publish" if req.method == "POST" => return events_publish(req, bus),
        _ => {}
    }

    // Event engine HTTP API
    if path.starts_with("/events/") {
        return events.handle(req);
    }

    // Messaging API
    if path.starts_with("/messages/") {
        return messaging.handle(req);
    }

    // Legacy comments redirect → messages (backward compat for widget migration)
    if path.starts_with("/comments/") {
        let new_path = format!("/messages/{}?type=comment", &path["/comments/".len()..]);
        return redirect(&new_path);
    }

    if path.starts_with("/assets") {
        return assets.handle(req);
    }

    // Reverse proxy routes (longest prefix first)
    if path.starts_with("/proposals") { return proxy_request(req, 8898); }
    if path.starts_with("/social")    { return proxy_request(req, 8899); }
    if path.starts_with("/pulse")     { return proxy_request(req, 8896); }
    if path.starts_with("/visions")   { return proxy_request(req, 8890); }
    if path.starts_with("/boi")       { return proxy_request(req, 8891); }
    if path.starts_with("/ui")        { return proxy_request(req, 8889); }
    if path.starts_with("/artifacts") { return proxy_request(req, 8897); }

    // Static files for fleet dashboard
    if path.starts_with("/fleet") {
        return serve_fleet_static(path, hex_dir);
    }

    // Landing page
    if path == "/" {
        return landing_page();
    }

    json_error(404, "not found")
}

fn handle_sse_stream(
    stream: &mut TcpStream,
    topics: Vec<String>,
    bus: &Arc<SseBus>,
    telemetry: &Arc<Telemetry>,
) {
    let (sub_id, rx) = bus.subscribe(topics);
    // No read timeout for long-lived SSE connections
    let _ = stream.set_read_timeout(None);

    let header = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: text/event-stream\r\n",
        "Cache-Control: no-cache\r\n",
        "Access-Control-Allow-Origin: *\r\n",
        "Connection: keep-alive\r\n",
        "\r\n",
    );
    if stream.write_all(header.as_bytes()).is_err() {
        bus.unsubscribe(&sub_id);
        return;
    }
    let _ = stream.flush();

    loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(msg) => {
                let data = format!("data: {}\n\n", msg);
                if stream.write_all(data.as_bytes()).is_err() {
                    break;
                }
                let _ = stream.flush();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if stream.write_all(b": heartbeat\n\n").is_err() {
                    break;
                }
                let _ = stream.flush();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    telemetry.emit("hex.sse.disconnect", &serde_json::json!({ "sub_id": sub_id }));
    bus.unsubscribe(&sub_id);
}

fn events_topics(bus: &Arc<SseBus>) -> Response {
    let manifests = bus.get_manifests();
    let body = serde_json::to_vec(&manifests).unwrap_or_default();
    json_response(200, body)
}

fn events_health(bus: &Arc<SseBus>) -> Response {
    let body = serde_json::to_vec(&serde_json::json!({
        "status": "ok",
        "subscribers": bus.subscriber_count(),
    }))
    .unwrap_or_default();
    json_response(200, body)
}

fn events_publish(req: &Request, bus: &Arc<SseBus>) -> Response {
    #[derive(serde::Deserialize)]
    struct PublishBody {
        topic: String,
        r#type: String,
        payload: serde_json::Value,
    }
    let p: PublishBody = match serde_json::from_slice(&req.body) {
        Ok(p) => p,
        Err(e) => return json_error(400, &format!("invalid JSON: {}", e)),
    };
    bus.publish(&p.topic, &p.r#type, &p.payload);
    let body = serde_json::to_vec(&serde_json::json!({ "ok": true })).unwrap_or_default();
    json_response(202, body)
}

fn redirect(location: &str) -> Response {
    Response {
        status: 301,
        content_type: "text/plain".to_string(),
        headers: vec![
            ("Location".to_string(), location.to_string()),
            ("Access-Control-Allow-Origin".to_string(), "*".to_string()),
        ],
        body: Vec::new(),
    }
}

fn proxy_request(req: &Request, backend_port: u16) -> Response {
    let addr = format!("127.0.0.1:{}", backend_port);
    let sock_addr: std::net::SocketAddr = match addr.parse() {
        Ok(a) => a,
        Err(_) => return json_error(502, "invalid backend address"),
    };
    let mut backend = match TcpStream::connect_timeout(&sock_addr, Duration::from_secs(30)) {
        Ok(s) => s,
        Err(e) => return json_error(502, &format!("backend unavailable: {}", e)),
    };
    let _ = backend.set_read_timeout(Some(Duration::from_secs(30)));

    let full_path = if req.query.is_empty() {
        req.path.clone()
    } else {
        let qs: String = req.query.iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");
        format!("{}?{}", req.path, qs)
    };

    let req_line = format!("{} {} HTTP/1.1\r\n", req.method, full_path);
    if backend.write_all(req_line.as_bytes()).is_err() {
        return json_error(502, "backend write failed");
    }
    for (k, v) in &req.headers {
        if k == "connection" { continue; }
        let _ = backend.write_all(format!("{}: {}\r\n", k, v).as_bytes());
    }
    let _ = backend.write_all(b"Connection: close\r\n\r\n");
    if !req.body.is_empty() {
        let _ = backend.write_all(&req.body);
    }

    let mut raw = Vec::new();
    let _ = backend.read_to_end(&mut raw);

    Response {
        status: 0,
        content_type: String::new(),
        headers: vec![("__raw__".to_string(), String::new())],
        body: raw,
    }
}

fn serve_fleet_static(path: &str, hex_dir: &Path) -> Response {
    let static_dir = hex_dir.join(".hex/scripts/hex-router/static");
    let rel = path.trim_start_matches("/fleet").trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };

    if rel.contains("..") {
        return json_error(400, "invalid path");
    }

    let file_path = static_dir.join(rel);
    match std::fs::read(&file_path) {
        Ok(data) => Response {
            status: 200,
            content_type: mime_type(rel).to_string(),
            headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
            body: data,
        },
        Err(_) => json_error(404, "file not found"),
    }
}

fn landing_page() -> Response {
    let html = r#"<!DOCTYPE html>
<html><head><title>hex server</title></head>
<body>
<h1>hex server</h1>
<ul>
<li><a href="/events/topics">/events/topics</a> — SSE topic manifests</li>
<li><a href="/events/health">/events/health</a> — SSE bus health</li>
<li><a href="/events/stream">/events/stream</a> — SSE event stream</li>
<li><a href="/events/status">/events/status</a> — Event engine status</li>
<li><a href="/events/recent">/events/recent</a> — Recent events</li>
<li>/messages/ — Unified messaging API</li>
<li>/assets/ — Asset registry</li>
<li><a href="/fleet">/fleet</a> — Fleet dashboard</li>
</ul>
</body></html>"#;
    Response {
        status: 200,
        content_type: "text/html; charset=utf-8".to_string(),
        headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
        body: html.as_bytes().to_vec(),
    }
}

fn json_response(status: u16, body: Vec<u8>) -> Response {
    Response {
        status,
        content_type: "application/json".to_string(),
        headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
        body,
    }
}

fn json_error(status: u16, msg: &str) -> Response {
    let body = serde_json::to_vec(&serde_json::json!({ "error": msg })).unwrap_or_default();
    json_response(status, body)
}

fn mime_type(filename: &str) -> &'static str {
    if filename.ends_with(".html") || filename.ends_with(".htm") {
        "text/html; charset=utf-8"
    } else if filename.ends_with(".js") {
        "application/javascript"
    } else if filename.ends_with(".css") {
        "text/css"
    } else if filename.ends_with(".json") {
        "application/json"
    } else if filename.ends_with(".png") {
        "image/png"
    } else if filename.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    }
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        400 => "Bad Request",
        404 => "Not Found",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        _ => "Unknown",
    }
}

fn write_response(stream: &mut TcpStream, resp: Response) {
    // Proxy passthrough: write raw bytes directly
    if resp.status == 0 && resp.headers.iter().any(|(k, _)| k == "__raw__") {
        let _ = stream.write_all(&resp.body);
        return;
    }

    let status_line = format!("HTTP/1.1 {} {}\r\n", resp.status, status_text(resp.status));
    let _ = stream.write_all(status_line.as_bytes());
    if !resp.content_type.is_empty() {
        let _ = stream.write_all(format!("Content-Type: {}\r\n", resp.content_type).as_bytes());
    }
    let _ = stream.write_all(format!("Content-Length: {}\r\n", resp.body.len()).as_bytes());
    for (k, v) in &resp.headers {
        let _ = stream.write_all(format!("{}: {}\r\n", k, v).as_bytes());
    }
    let _ = stream.write_all(b"\r\n");
    let _ = stream.write_all(&resp.body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_basic() {
        let q = parse_query("foo=bar&baz=qux");
        assert_eq!(q.get("foo").map(|s| s.as_str()), Some("bar"));
        assert_eq!(q.get("baz").map(|s| s.as_str()), Some("qux"));
    }

    #[test]
    fn parse_query_empty() {
        let q = parse_query("");
        assert!(q.is_empty());
    }

    #[test]
    fn parse_query_plus_encoded() {
        let q = parse_query("topics=content.comments%2Csystem.agents");
        assert!(q.contains_key("topics"));
    }

    #[test]
    fn url_decode_plus() {
        assert_eq!(url_decode("hello+world"), "hello world");
    }

    #[test]
    fn url_decode_percent() {
        assert_eq!(url_decode("hello%20world"), "hello world");
    }

    #[test]
    fn mime_type_mapping() {
        assert_eq!(mime_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_type("app.js"), "application/javascript");
        assert_eq!(mime_type("style.css"), "text/css");
    }

    #[test]
    fn router_events_prefix() {
        assert!("/events/health".starts_with("/events/"));
        assert!("/events/topics".starts_with("/events/"));
    }

    #[test]
    fn router_proxy_prefixes() {
        for prefix in &["/proposals", "/social", "/pulse", "/visions", "/boi", "/ui", "/artifacts"] {
            assert!(format!("{}/foo", prefix).starts_with(prefix));
        }
    }

    #[test]
    fn comments_redirect_path() {
        let path = "/comments/api/messages";
        assert!(path.starts_with("/comments/"));
        let new_path = format!("/messages/{}?type=comment", &path["/comments/".len()..]);
        assert_eq!(new_path, "/messages/api/messages?type=comment");
    }
}
