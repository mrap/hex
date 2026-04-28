use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub struct SseBus {
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
    manifests: Arc<Mutex<HashMap<String, TopicManifest>>>,
}

struct Subscriber {
    id: String,
    topics: Vec<String>,
    sender: std::sync::mpsc::Sender<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TopicManifest {
    pub topic: String,
    pub description: String,
    #[serde(default)]
    pub bridge: Vec<String>,
    #[serde(default)]
    pub events: Vec<EventSchema>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EventSchema {
    pub r#type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub payload: HashMap<String, serde_json::Value>,
}

impl SseBus {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
            manifests: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn load_manifests(&self, topics_dir: &Path) {
        let pattern = topics_dir.join("*.yaml");
        let pattern_str = pattern.to_string_lossy();
        let paths = match glob::glob(&pattern_str) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("SSE: failed to glob topics dir: {e}");
                return;
            }
        };
        let mut manifests = self.manifests.lock().unwrap();
        for entry in paths.flatten() {
            let content = match std::fs::read_to_string(&entry) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("SSE: failed to read {:?}: {e}", entry);
                    continue;
                }
            };
            match serde_yaml::from_str::<TopicManifest>(&content) {
                Ok(m) => {
                    manifests.insert(m.topic.clone(), m);
                }
                Err(e) => {
                    eprintln!("SSE: failed to parse {:?}: {e}", entry);
                }
            }
        }
    }

    pub fn subscribe(&self, topics: Vec<String>) -> (String, std::sync::mpsc::Receiver<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let id = Uuid::new_v4().to_string();
        let sub = Subscriber {
            id: id.clone(),
            topics,
            sender: tx,
        };
        self.subscribers.lock().unwrap().push(sub);
        (id, rx)
    }

    pub fn unsubscribe(&self, id: &str) {
        self.subscribers.lock().unwrap().retain(|s| s.id != id);
    }

    pub fn publish(&self, topic: &str, event_type: &str, payload: &serde_json::Value) {
        let msg = serde_json::json!({
            "topic": topic,
            "type": event_type,
            "payload": payload,
        });
        let msg_str = match serde_json::to_string(&msg) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("SSE: serialize failed: {e}");
                return;
            }
        };
        let subs = self.subscribers.lock().unwrap();
        for sub in subs.iter() {
            if sub.topics.iter().any(|t| topic_matches(t, topic)) {
                let _ = sub.sender.send(msg_str.clone());
            }
        }
    }

    pub fn get_manifests(&self) -> HashMap<String, TopicManifest> {
        self.manifests.lock().unwrap().clone()
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().unwrap().len()
    }
}

// Returns true if filter matches the given topic.
// Exact: "content.comments" matches "content.comments"
// Wildcard: "content.*" matches anything starting with "content."
// Global: "*" matches everything
fn topic_matches(filter: &str, topic: &str) -> bool {
    if filter == "*" {
        return true;
    }
    if filter == topic {
        return true;
    }
    if let Some(prefix) = filter.strip_suffix(".*") {
        return topic.starts_with(&format!("{prefix}."));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(topic_matches("content.comments", "content.comments"));
        assert!(!topic_matches("content.comments", "content.assets"));
    }

    #[test]
    fn wildcard_match() {
        assert!(topic_matches("content.*", "content.comments"));
        assert!(topic_matches("content.*", "content.assets"));
        assert!(!topic_matches("content.*", "system.agents"));
        assert!(!topic_matches("content.*", "content"));
    }

    #[test]
    fn global_match() {
        assert!(topic_matches("*", "content.comments"));
        assert!(topic_matches("*", "system.boi"));
        assert!(topic_matches("*", "anything"));
    }
}
