use chrono;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCapability {
    pub id: String,
    pub kind: String,
    pub created_by: String,
    pub created_at: String,
    pub created_in_wake: u64,
    pub unprompted: bool,
    pub description: String,
    pub exec: String,
    pub input_schema: serde_json::Value,
    pub callable_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerCapability {
    pub id: String,
    pub kind: String,
    pub created_by: String,
    pub created_at: String,
    pub created_in_wake: u64,
    pub unprompted: bool,
    pub description: String,
    pub event: String,
    pub input_schema: serde_json::Value,
    pub callable_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub kind: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub created_by: String,
}

/// Atomically persist a function capability.
///
/// Write order: bin/<id> → chmod +x → functions/<id>.json (tmp+rename).
/// The JSON file is the commit barrier: a reader that sees it is guaranteed the
/// executable exists and is +x. Never call this if functions/<id>.json already
/// exists — that check belongs to capability_guard.rs.
pub fn add_function(
    registry_dir: &Path,
    cap: &FunctionCapability,
    script_body: &[u8],
) -> Result<(), String> {
    let bin_dir = registry_dir.join("bin");
    fs::create_dir_all(&bin_dir).map_err(|e| format!("create bin dir: {e}"))?;
    let bin_path = bin_dir.join(&cap.id);

    // Step 1: write script body
    fs::write(&bin_path, script_body).map_err(|e| format!("write bin/{}: {e}", cap.id))?;

    // Step 2: chmod +x
    let mut perms = fs::metadata(&bin_path)
        .map_err(|e| format!("stat bin/{}: {e}", cap.id))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin_path, perms).map_err(|e| format!("chmod bin/{}: {e}", cap.id))?;

    // Step 3: write JSON definition last (tmp + rename = atomic commit barrier)
    let fn_dir = registry_dir.join("functions");
    fs::create_dir_all(&fn_dir).map_err(|e| format!("create functions dir: {e}"))?;
    let fn_path = fn_dir.join(format!("{}.json", cap.id));
    let tmp_path = fn_dir.join(format!(".{}.json.tmp", cap.id));
    let json = serde_json::to_vec_pretty(cap).map_err(|e| format!("serialize capability: {e}"))?;
    fs::write(&tmp_path, &json).map_err(|e| format!("write tmp json: {e}"))?;
    fs::rename(&tmp_path, &fn_path)
        .map_err(|e| format!("rename to functions/{}.json: {e}", cap.id))?;

    Ok(())
}

/// Atomically persist a trigger capability (no executable — JSON only).
pub fn add_trigger(registry_dir: &Path, cap: &TriggerCapability) -> Result<(), String> {
    let tr_dir = registry_dir.join("triggers");
    fs::create_dir_all(&tr_dir).map_err(|e| format!("create triggers dir: {e}"))?;
    let tr_path = tr_dir.join(format!("{}.json", cap.id));
    let tmp_path = tr_dir.join(format!(".{}.json.tmp", cap.id));
    let json = serde_json::to_vec_pretty(cap).map_err(|e| format!("serialize trigger: {e}"))?;
    fs::write(&tmp_path, &json).map_err(|e| format!("write tmp trigger json: {e}"))?;
    fs::rename(&tmp_path, &tr_path)
        .map_err(|e| format!("rename to triggers/{}.json: {e}", cap.id))?;
    Ok(())
}

fn append_jsonl(path: &Path, record: &serde_json::Value) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let line = serde_json::to_string(record).map_err(|e| format!("serialize record: {e}"))?;
    writeln!(file, "{}", line).map_err(|e| format!("write to {}: {e}", path.display()))?;
    Ok(())
}

pub fn append_call(registry_dir: &Path, record: &serde_json::Value) -> Result<(), String> {
    append_jsonl(&registry_dir.join("calls.jsonl"), record)
}

pub fn append_audit(registry_dir: &Path, record: &serde_json::Value) -> Result<(), String> {
    append_jsonl(&registry_dir.join("audit.jsonl"), record)
}

/// Load the pilot-agent allowlist from <hex_dir>/.hex/registry/allowlist.json.
/// Returns an empty list if the file does not exist (no pilots configured).
pub fn load_allowlist(hex_dir: &Path) -> Result<Vec<String>, String> {
    let path = hex_dir.join(".hex/registry/allowlist.json");
    if !path.exists() {
        return Ok(vec![]);
    }
    let data = fs::read_to_string(&path).map_err(|e| format!("read allowlist: {e}"))?;
    serde_json::from_str::<Vec<String>>(&data).map_err(|e| format!("parse allowlist: {e}"))
}

/// Return true iff `agent_id` appears in the pilot allowlist.
pub fn is_allowed(hex_dir: &Path, agent_id: &str) -> bool {
    load_allowlist(hex_dir)
        .map(|list| list.iter().any(|a| a == agent_id))
        .unwrap_or(false)
}

// NOTE: `emit_trigger_policy` was removed in the fleet teardown. It existed solely
// to write a `.hex/registry/policies/registry-<id>.yaml` whose only action shelled
// out to `hex agent wake <agent_id>`. The agent fleet and the `hex agent` CLI no
// longer exist, so that action could never succeed. The registry still manages
// function/trigger capabilities and reconciles policy files via `remove_capability`.

/// Remove a capability by id: removes functions/<id>.json or triggers/<id>.json,
/// the corresponding bin/<id> (if present), and the policies/registry-<id>.yaml (if present).
pub fn remove_capability(registry_dir: &Path, cap_id: &str) -> Result<(), String> {
    let fn_path = registry_dir
        .join("functions")
        .join(format!("{cap_id}.json"));
    if fn_path.exists() {
        fs::remove_file(&fn_path).map_err(|e| format!("remove functions/{cap_id}.json: {e}"))?;
    }

    let tr_path = registry_dir.join("triggers").join(format!("{cap_id}.json"));
    if tr_path.exists() {
        fs::remove_file(&tr_path).map_err(|e| format!("remove triggers/{cap_id}.json: {e}"))?;
    }

    let bin_path = registry_dir.join("bin").join(cap_id);
    if bin_path.exists() {
        fs::remove_file(&bin_path).map_err(|e| format!("remove bin/{cap_id}: {e}"))?;
    }

    // Lifecycle reconcile: remove the policy file if present.
    let policy_path = registry_dir
        .join("policies")
        .join(format!("registry-{cap_id}.yaml"));
    if policy_path.exists() {
        fs::remove_file(&policy_path)
            .map_err(|e| format!("remove policy registry-{cap_id}.yaml: {e}"))?;
    }

    Ok(())
}

/// Re-entrancy guard: returns Err if `agent_id` has any trigger capability created in
/// `current_wake`, which would mean a policy it created could wake it in the same wake.
pub fn check_reentrancy(
    registry_dir: &Path,
    agent_id: &str,
    current_wake: u64,
) -> Result<(), String> {
    let tr_dir = registry_dir.join("triggers");
    if !tr_dir.is_dir() {
        return Ok(());
    }

    let read_dir = fs::read_dir(&tr_dir).map_err(|e| format!("read triggers dir: {e}"))?;
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let data =
            fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let val: serde_json::Value =
            serde_json::from_str(&data).map_err(|e| format!("parse {}: {e}", path.display()))?;

        let created_by = val["created_by"].as_str().unwrap_or("");
        let created_in_wake = val["created_in_wake"].as_u64().unwrap_or(0);

        if created_by == agent_id && created_in_wake == current_wake {
            return Err(format!(
                "re-entrancy guard: agent '{}' cannot be woken by policy '{}' it created in wake {}",
                agent_id,
                path.file_stem().unwrap_or_default().to_string_lossy(),
                current_wake
            ));
        }
    }

    Ok(())
}

/// Emit a `registry.capability.added` event to `.hex/registry/events.jsonl`.
///
/// Wake-ordering contract: this MUST be called only AFTER the capability is fully
/// persisted (bin + JSON committed). The event is the ordering signal for sibling
/// pilot agents — their wakes are triggered by this event rather than an unordered
/// `timer.tick.daily` fan-out, so `build_catalog` on event receipt always observes
/// a consistent registry state that includes the new capability.
pub fn emit_capability_added(
    registry_dir: &Path,
    cap_id: &str,
    created_by: &str,
) -> Result<(), String> {
    let record = serde_json::json!({
        "type": "registry.capability.added",
        "id": cap_id,
        "created_by": created_by,
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    append_jsonl(&registry_dir.join("events.jsonl"), &record)
}

/// Build the derived capability catalog by reading functions/*.json and triggers/*.json.
/// Each entry is stripped to {id, kind, description, input_schema, created_by}.
pub fn build_catalog(registry_dir: &Path) -> Result<Vec<CatalogEntry>, String> {
    let mut entries = Vec::new();

    for subdir in &["functions", "triggers"] {
        let dir = registry_dir.join(subdir);
        if !dir.is_dir() {
            continue;
        }
        let read_dir = fs::read_dir(&dir).map_err(|e| format!("read {subdir} dir: {e}"))?;
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let data =
                fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
            let val: serde_json::Value = serde_json::from_str(&data)
                .map_err(|e| format!("parse {}: {e}", path.display()))?;

            let id = val["id"].as_str().unwrap_or("").to_string();
            if id.is_empty() {
                continue;
            }
            entries.push(CatalogEntry {
                id,
                kind: val["kind"].as_str().unwrap_or("").to_string(),
                description: val["description"].as_str().unwrap_or("").to_string(),
                input_schema: val["input_schema"].clone(),
                created_by: val["created_by"].as_str().unwrap_or("").to_string(),
            });
        }
    }

    Ok(entries)
}
