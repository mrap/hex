use crate::types::TrailEntry;

struct GateSchema {
    required_fields: &'static [&'static str],
}

fn schema_for(entry_type: &str) -> Option<GateSchema> {
    match entry_type {
        "observe" => Some(GateSchema {
            required_fields: &["what", "noted"],
        }),
        "find" => Some(GateSchema {
            required_fields: &["finding", "evidence"],
        }),
        "decide" => Some(GateSchema {
            required_fields: &["decision", "alternatives", "reasoning"],
        }),
        "act" => Some(GateSchema {
            required_fields: &["action", "result"],
        }),
        "verify" => Some(GateSchema {
            required_fields: &["check", "evidence", "status"],
        }),
        "delegate" => Some(GateSchema {
            required_fields: &["initiative_id", "to", "context"],
        }),
        "park" => Some(GateSchema {
            required_fields: &["item_id", "reason", "resume_condition"],
        }),
        "reframe" => Some(GateSchema {
            required_fields: &["abandoned", "reason", "new_framing"],
        }),
        "message_sent" => Some(GateSchema {
            required_fields: &["to", "subject", "body"],
        }),
        "sync_started" => Some(GateSchema {
            required_fields: &["with", "context"],
        }),
        "assess" => Some(GateSchema {
            required_fields: &["area", "finding", "adjustment"],
        }),
        _ => None,
    }
}

pub fn validate(entry: &TrailEntry) -> Result<(), String> {
    let schema = schema_for(&entry.entry_type)
        .ok_or_else(|| format!("unknown action type: '{}'", entry.entry_type))?;
    let detail = entry
        .detail
        .as_object()
        .ok_or_else(|| "detail must be a JSON object".to_string())?;
    for field in schema.required_fields {
        match detail.get(*field) {
            None => {
                return Err(format!(
                    "missing required field '{}' for type '{}'",
                    field, entry.entry_type
                ))
            }
            Some(val) => {
                if let Some(s) = val.as_str() {
                    if s.is_empty() {
                        return Err(format!(
                            "required field '{}' cannot be empty for type '{}'",
                            field, entry.entry_type
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}
