use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

pub fn append(audit_dir: &Path, agent_id: &str, action: &str, detail: &serde_json::Value) {
    let path = audit_dir.join("actions.jsonl");
    if let Err(e) = fs::create_dir_all(audit_dir) {
        eprintln!(
            "AUDIT WRITE FAILED: cannot create {}: {e}",
            audit_dir.display()
        );
        return;
    }
    let entry = serde_json::json!({
        "ts": Utc::now().to_rfc3339(),
        "agent": agent_id,
        "action": action,
        "detail": detail,
    });
    let line = match serde_json::to_string(&entry) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("AUDIT SERIALIZE FAILED: {e}");
            return;
        }
    };
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{}", line) {
                eprintln!(
                    "AUDIT WRITE FAILED: cannot write to {}: {e}",
                    path.display()
                );
            }
        }
        Err(e) => {
            eprintln!("AUDIT WRITE FAILED: cannot open {}: {e}", path.display());
        }
    }
}
