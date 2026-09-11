use std::path::{Path, PathBuf};
use std::time::SystemTime;

// ── path derivation ───────────────────────────────────────────────────────────

/// Convert a CLAUDE_PROJECT_DIR value to the slug used by Claude Code.
///
/// Claude Code turns the project dir path into a storage slug by replacing
/// every '/' with '-' (no leading dash; the path always starts with '/').
fn dir_to_slug(project_dir: &str) -> String {
    project_dir.replace('/', "-")
}

/// Fast-path source resolution using env vars (O(1)).
pub fn fast_path_source(projects_dir: &Path, project_dir: &str, session_id: &str) -> PathBuf {
    let slug = dir_to_slug(project_dir);
    projects_dir.join(&slug).join(format!("{session_id}.jsonl"))
}

/// Fallback: walk `~/.claude/projects/` up to depth 2, return the
/// most-recently-modified `.jsonl` file.
pub fn find_latest_jsonl(projects_dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;

    let top = match std::fs::read_dir(projects_dir) {
        Ok(d) => d,
        Err(_) => return None,
    };

    for project_entry in top.filter_map(|e| e.ok()) {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let inner = match std::fs::read_dir(&project_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for file_entry in inner.filter_map(|e| e.ok()) {
            let path = file_entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if !name.ends_with(".jsonl") {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(mtime) = meta.modified() {
                    match &best {
                        None => best = Some((mtime, path)),
                        Some((prev_time, _)) if mtime > *prev_time => {
                            best = Some((mtime, path));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    best.map(|(_, p)| p)
}

// ── entry point ───────────────────────────────────────────────────────────────

/// Stop-hook stdin payload → transcript path, validated to exist.
pub fn source_from_stdin(raw: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let p = v.get("transcript_path").and_then(|t| t.as_str())?;
    let path = PathBuf::from(p);
    path.is_file().then_some(path)
}

pub fn run() {
    let mut raw = String::new();
    use std::io::Read;
    let _ = std::io::stdin().read_to_string(&mut raw);
    let hex_dir = std::env::var("HEX_DIR")
        .ok()
        .or_else(|| std::env::var("CLAUDE_PROJECT_DIR").ok())
        .map(PathBuf::from);
    let Some(hex_dir) = hex_dir else {
        fail("HEX_DIR and CLAUDE_PROJECT_DIR both unset");
        return;
    };
    run_inner(&raw, &hex_dir);
}

fn run_inner(raw: &str, hex_dir: &Path) {
    let Some(home) = std::env::var("HOME").ok().map(PathBuf::from) else {
        fail("HOME unset");
        return;
    };
    let projects_dir = home.join(".claude/projects");
    let backup_dir = hex_dir.join("raw/transcripts");

    // Priority: stdin payload (authoritative) → env fast path → newest scan.
    let source = source_from_stdin(raw)
        .or_else(|| {
            let sid = std::env::var("CLAUDE_SESSION_ID").unwrap_or_default();
            let pd = std::env::var("CLAUDE_PROJECT_DIR").unwrap_or_default();
            (!sid.is_empty() && !pd.is_empty())
                .then(|| fast_path_source(&projects_dir, &pd, &sid))
                .filter(|p| p.is_file())
        })
        .or_else(|| {
            eprintln!("hex hook capture: no transcript_path on stdin — falling back to newest-jsonl scan (race-prone)");
            find_latest_jsonl(&projects_dir)
        });

    let Some(source) = source else {
        fail("no transcript source found (stdin, env, and scan all empty)");
        return;
    };
    let Some(basename) = source.file_name().map(|n| n.to_os_string()) else {
        fail(&format!("source has no basename: {}", source.display()));
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&backup_dir) {
        fail(&format!("create {}: {e}", backup_dir.display()));
        return;
    }
    let dest = backup_dir.join(&basename);
    match std::fs::copy(&source, &dest) {
        Ok(bytes) => {
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "hook::capture".into(),
                event: "capture".into(),
                status: "ok".into(),
                duration_ms: None,
                exit_code: Some(0),
                detail: Some(format!("{} ({bytes} bytes)", dest.display())),
            });
        }
        Err(e) => fail(&format!(
            "copy {} -> {}: {e}",
            source.display(),
            dest.display()
        )),
    }
}

/// Loud but never blocking: a failed backup must not disrupt the session, so
/// the hook process always exits 0 — loudness lives in stderr + telemetry.
fn fail(msg: &str) {
    eprintln!("hex hook capture: {msg}");
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "hook::capture".into(),
        event: "capture".into(),
        status: "error".into(),
        duration_ms: None,
        exit_code: Some(0),
        detail: Some(msg.to_string()),
    });
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn slug_replaces_slashes() {
        assert_eq!(dir_to_slug("/Users/test/hex"), "-Users-test-hex");
    }

    #[test]
    fn fast_path_derives_correct_path() {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path();
        let project_dir = "/Users/test/hex";
        let session_id = "abc123";

        let got = fast_path_source(projects_dir, project_dir, session_id);
        let expected = projects_dir.join("-Users-test-hex").join("abc123.jsonl");
        assert_eq!(got, expected);
    }

    #[test]
    fn find_latest_jsonl_returns_most_recent() {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path();

        // Create two project subdirs with .jsonl files.
        let proj_a = projects_dir.join("proj-a");
        let proj_b = projects_dir.join("proj-b");
        std::fs::create_dir_all(&proj_a).unwrap();
        std::fs::create_dir_all(&proj_b).unwrap();

        std::fs::write(proj_a.join("old.jsonl"), b"old").unwrap();
        // Small sleep to ensure different mtime.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(proj_b.join("new.jsonl"), b"new").unwrap();

        let result = find_latest_jsonl(projects_dir).expect("should find a file");
        assert_eq!(result.file_name().unwrap(), "new.jsonl");
    }

    #[test]
    fn find_latest_jsonl_empty_dir_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(find_latest_jsonl(tmp.path()).is_none());
    }

    #[test]
    fn find_latest_jsonl_ignores_non_jsonl() {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path();
        let proj = projects_dir.join("proj-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("session.log"), b"not jsonl").unwrap();

        assert!(find_latest_jsonl(projects_dir).is_none());
    }

    #[test]
    fn stdin_transcript_path_wins() {
        let tmp = TempDir::new().unwrap();
        let t = tmp.path().join("abc.jsonl");
        std::fs::write(&t, b"{}").unwrap();
        let raw = format!(
            r#"{{"session_id":"abc","transcript_path":"{}","hook_event_name":"Stop"}}"#,
            t.display()
        );
        assert_eq!(source_from_stdin(&raw), Some(t));
    }

    #[test]
    fn run_inner_copies_stdin_transcript() {
        // run_inner's success path records telemetry, which resolves
        // events.db from the process-global $HEX_DIR — isolate it to a temp
        // dir (review-fix 2026-06-11: without this, the test wrote fake
        // `hook::capture ok` rows into the PRODUCTION events.db, fabricating
        // the exact signal the post-deploy smoke test queries).
        let _environment = crate::test_env::isolate_hex_dir();
        let tmp = TempDir::new().unwrap();
        let hex = tmp.path().join("hex");
        std::fs::create_dir_all(&hex).unwrap();
        let t = tmp.path().join("sess.jsonl");
        std::fs::write(&t, b"line1").unwrap();
        let raw = format!(r#"{{"transcript_path":"{}"}}"#, t.display());
        run_inner(&raw, &hex);
        assert!(hex.join("raw/transcripts/sess.jsonl").is_file());

        // The capture telemetry row must land in the ISOLATED store.
        let conn = crate::telemetry::open_ro().unwrap();
        let expected_detail = format!(
            "{} (5 bytes)",
            hex.join("raw/transcripts/sess.jsonl").display()
        );
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE source = 'hook::capture' AND status = 'ok' AND detail = ?1",
            [expected_detail],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(
            count, 1,
            "capture success row must be recorded in the isolated HEX_DIR \
             telemetry store"
        );
    }

    #[test]
    fn stdin_rejects_missing_file_and_garbage() {
        assert_eq!(
            source_from_stdin(r#"{"transcript_path":"/nonexistent/x.jsonl"}"#),
            None
        );
        assert_eq!(source_from_stdin("not json"), None);
        assert_eq!(source_from_stdin(r#"{"session_id":"abc"}"#), None);
    }
}
