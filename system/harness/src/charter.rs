use crate::types::Charter;
use std::path::Path;

#[derive(Debug)]
pub struct CharterError(pub String);

impl std::fmt::Display for CharterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Charter error: {}", self.0)
    }
}

impl std::error::Error for CharterError {}

pub fn load_from_str(yaml: &str) -> Result<Charter, Box<dyn std::error::Error>> {
    let charter: Charter =
        serde_yaml::from_str(yaml).map_err(|e| CharterError(format!("YAML parse error: {e}")))?;
    validate(&charter)?;
    Ok(charter)
}

pub fn load(path: &Path) -> Result<Charter, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| CharterError(format!("cannot read {}: {e}", path.display())))?;
    let charter: Charter = serde_yaml::from_str(&contents)
        .map_err(|e| CharterError(format!("YAML parse error in {}: {e}", path.display())))?;
    validate(&charter)?;
    Ok(charter)
}

fn validate(charter: &Charter) -> Result<(), CharterError> {
    if charter.id.is_empty() {
        return Err(CharterError("id is required and cannot be empty".into()));
    }
    if !charter
        .id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(CharterError(format!(
            "id '{}' contains unsafe characters — only [a-zA-Z0-9_-] allowed",
            charter.id
        )));
    }
    if charter.budget.usd_per_day < 0.0 {
        return Err(CharterError("budget.usd_per_day cannot be negative".into()));
    }
    if charter.budget.usd_per_shift < 0.0 {
        return Err(CharterError(
            "budget.usd_per_shift cannot be negative".into(),
        ));
    }
    if charter.kill_switch.is_empty() {
        return Err(CharterError("kill_switch path is required".into()));
    }
    Ok(())
}
