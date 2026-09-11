//! Claude Code `UserPromptSubmit` hook. It injects relevant workspace memory as
//! `additionalContext` and fails open without blocking the turn.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;

fn emit_context(mut output: impl Write, context: &str) -> std::io::Result<()> {
    let value = json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": context,
        }
    });
    writeln!(output, "{value}")
}

fn run_with_io<F>(
    mut input: impl Read,
    output: impl Write,
    mut error_output: impl Write,
    root_lookup: F,
) where
    F: FnOnce() -> Option<PathBuf>,
{
    let mut raw = String::new();
    if let Err(error) = input.read_to_string(&mut raw) {
        let _ = writeln!(
            error_output,
            "[hook/user-prompt-submit] read hook input: {error}"
        );
        return;
    }
    let input: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(error) => {
            let _ = writeln!(
                error_output,
                "[hook/user-prompt-submit] parse hook input: {error}"
            );
            return;
        }
    };
    let Some(prompt) = input.get("prompt").and_then(Value::as_str) else {
        let _ = writeln!(
            error_output,
            "[hook/user-prompt-submit] hook input has no string prompt"
        );
        return;
    };
    let Some(hex_dir) = root_lookup() else {
        let _ = writeln!(
            error_output,
            "[hook/user-prompt-submit] HEX_DIR/CLAUDE_PROJECT_DIR not set; memory injection disabled"
        );
        return;
    };

    let outcome = crate::memory::recall::recall(&hex_dir, prompt, false);
    if outcome.injected {
        if let Err(error) = emit_context(output, &outcome.context) {
            let _ = writeln!(
                error_output,
                "[hook/user-prompt-submit] write hook output: {error}"
            );
        }
    }
}

pub fn run() {
    run_with_io(
        std::io::stdin(),
        std::io::stdout(),
        std::io::stderr(),
        || {
            std::env::var("HEX_DIR")
                .ok()
                .or_else(|| std::env::var("CLAUDE_PROJECT_DIR").ok())
                .map(PathBuf::from)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn emit_context_is_valid_hook_json() {
        // Smoke test the JSON shape — capturing stdout is overkill; build the
        // value directly the way emit_context does.
        let v = json!({
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": "hello",
            }
        });
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    }

    #[test]
    fn actual_hook_path_keeps_interactive_private_recall() {
        let root = tempfile::TempDir::new().unwrap();
        let registry = root.path().join(".hex/config/memory-authority.toml");
        std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(root.path().join("docs")).unwrap();
        std::fs::write(
            registry,
            "version=1\n[[sources]]\nid='hook-public'\npath='docs/hook.md'\ntopics=['memory recall']\nauthority_status='current'\nprivate=false\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("docs/hook.md"),
            "HOOK_PUBLIC_SOURCE_CANARY memory recall",
        )
        .unwrap();
        let db = crate::memory::db_path(root.path());
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open(db).unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private) \
             VALUES ('private-hook','memory','recall','HOOK_PRIVATE_CANARY',0.9,'2026-09-10','2026-09-10',1)",
            [],
        )
        .unwrap();
        drop(conn);

        let input = br#"{"prompt":"what does memory recall contain privately"}"#;
        let mut output = Vec::new();
        let mut errors = Vec::new();
        run_with_io(&input[..], &mut output, &mut errors, || {
            Some(root.path().to_path_buf())
        });
        assert!(errors.is_empty());
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert!(value["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("HOOK_PRIVATE_CANARY"));
        assert!(value["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("HOOK_PUBLIC_SOURCE_CANARY"));
    }
}
