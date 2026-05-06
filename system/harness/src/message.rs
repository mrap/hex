use crate::types::Message;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::Path;

fn agent_inbox_path(
    store_dir: &Path,
    agent_id: &str,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    if agent_id.is_empty()
        || agent_id.contains('/')
        || agent_id.contains('\\')
        || agent_id.contains("..")
    {
        return Err(format!("unsafe agent_id for inbox path: '{}'", agent_id).into());
    }
    Ok(store_dir.join(format!("{}.jsonl", agent_id)))
}

pub fn send(store_dir: &Path, msg: &Message) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(store_dir)?;
    for recipient in &msg.to {
        let path = agent_inbox_path(store_dir, recipient)?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{}", serde_json::to_string(msg)?)?;
    }
    Ok(())
}

pub fn receive(store_dir: &Path, agent_id: &str) -> Vec<Message> {
    let path = match agent_inbox_path(store_dir, agent_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("INBOX PATH ERROR: {e}");
            return vec![];
        }
    };
    if !path.exists() {
        return vec![];
    }
    let mut messages = vec![];
    if let Ok(file) = fs::File::open(&path) {
        for (line_num, line) in std::io::BufReader::new(file).lines().enumerate() {
            match line {
                Ok(text) => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Message>(&text) {
                        Ok(msg) => messages.push(msg),
                        Err(e) => {
                            eprintln!(
                                "INBOX PARSE ERROR: agent '{}' line {}: {e}",
                                agent_id,
                                line_num + 1
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "INBOX READ ERROR: agent '{}' line {}: {e}",
                        agent_id,
                        line_num + 1
                    );
                }
            }
        }
    }
    messages
}

pub fn clear_inbox(store_dir: &Path, agent_id: &str) {
    let path = match agent_inbox_path(store_dir, agent_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("INBOX CLEAR ERROR: {e}");
            return;
        }
    };
    if path.exists() {
        if let Err(e) = fs::remove_file(&path) {
            eprintln!("INBOX CLEAR FAILED: cannot remove {}: {e}", path.display());
        }
    }
}
