use crate::types::{AgentResponse, AssessmentResponse, ClaudeOutput};
use std::io::Write;
use std::process::{Command, Stdio};

pub fn parse_output(raw: &str) -> Result<ClaudeOutput, Box<dyn std::error::Error>> {
    let output: ClaudeOutput = serde_json::from_str(raw)?;
    Ok(output)
}

pub fn parse_agent_response(
    result_text: &str,
) -> Result<AgentResponse, Box<dyn std::error::Error>> {
    let cleaned = extract_json(result_text);
    let response: AgentResponse = serde_json::from_str(&cleaned)?;
    Ok(response)
}

pub fn parse_assessment_response(
    result_text: &str,
) -> Result<AssessmentResponse, Box<dyn std::error::Error>> {
    let cleaned = extract_json(result_text);
    let response: AssessmentResponse = serde_json::from_str(&cleaned)?;
    Ok(response)
}

fn extract_json(text: &str) -> String {
    let trimmed = text.trim();

    // Try direct parse first
    if trimmed.starts_with('{') {
        return trimmed.to_string();
    }

    // Strip markdown code fences: ```json ... ``` or ``` ... ```
    if let Some(start) = trimmed.find("```") {
        let after_fence = &trimmed[start + 3..];
        // Skip optional language tag (e.g., "json")
        let content_start = after_fence.find('\n').map(|i| i + 1).unwrap_or(0);
        let content = &after_fence[content_start..];
        if let Some(end) = content.find("```") {
            let inner = content[..end].trim();
            if inner.starts_with('{') {
                return inner.to_string();
            }
        }
    }

    // Find first { and last } as fallback
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start < end {
            return trimmed[start..=end].to_string();
        }
    }

    trimmed.to_string()
}

pub fn build_args(model: &str, allowed_tools: &[&str]) -> Vec<String> {
    let tools_str = allowed_tools.join(",");
    vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "json".to_string(),
        "--model".to_string(),
        model.to_string(),
        "--allowedTools".to_string(),
        tools_str,
        "--dangerously-skip-permissions".to_string(),
    ]
}

pub fn invoke(
    prompt: &str,
    model: &str,
    allowed_tools: &[&str],
) -> Result<ClaudeOutput, Box<dyn std::error::Error>> {
    let args = build_args(model, allowed_tools);
    let mut child = Command::new("claude")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn claude: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes())?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("claude exited with {}: {stderr}", output.status).into());
    }

    let stdout = String::from_utf8(output.stdout)?;
    parse_output(&stdout)
}
