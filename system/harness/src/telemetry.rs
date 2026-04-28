use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Telemetry {
    log_path: PathBuf,
}

impl Telemetry {
    pub fn new(hex_dir: &Path) -> Self {
        let dir = hex_dir.join(".hex/telemetry");
        let _ = fs::create_dir_all(&dir);
        Self { log_path: dir.join("server.jsonl") }
    }

    pub fn emit(&self, event_type: &str, detail: &serde_json::Value) {
        let entry = serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "event": event_type,
            "detail": detail,
        });
        let line = match serde_json::to_string(&entry) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("TELEMETRY SERIALIZE FAILED: {e}");
                return;
            }
        };
        match OpenOptions::new().create(true).append(true).open(&self.log_path) {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{}", line) {
                    eprintln!("TELEMETRY WRITE FAILED: {e}");
                }
            }
            Err(e) => {
                eprintln!("TELEMETRY OPEN FAILED: {e}");
            }
        }
    }
}
