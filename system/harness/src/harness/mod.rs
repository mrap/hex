//! Minimal harness contract: the first vertical slice of submit(Event) -> Result.
//! These types are intentionally thin; the full submit() loop will EXTEND them.
//!
//! NAMING (review 2026-06-05): a DIFFERENT `Event` already exists at
//! `crate::worker::event::Event` (the iii-worker JSON envelope). This `harness::Event`
//! is unrelated. They do not collide because they live in separate modules — BUT
//! never `use crate::worker::event::Event` in `harness/` or `messages/`, and always
//! refer to this one as `crate::harness::Event`.

pub mod id;
pub mod supervise;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Opt {
    pub id: String,
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Prompt {
    pub id: String,
    pub text: String,
    pub multi: bool,
    pub options: Vec<Opt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Answer {
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub source: String,
    pub kind: String, // v1 only "request"; present for forward-compat routing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultOut {
    pub event_id: String,
    pub status: String, // "done" | "failed"
    pub output: String, // the worker's text answer (empty when output is a prompt)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Prompt>,
}

use crate::messages::{self, StoredMessage};
use crate::worker::run::WorkerOutput;
use rusqlite::Connection;
use std::path::Path;

const BUDGET: usize = 0; // 0 => assemble's default MAX_CONTEXT_CHARS

/// The v1 vertical slice of submit(Event) -> Result. `worker` is injected for
/// testability; production passes `crate::worker::run::run_worker`.
pub fn submit<F>(conn: &Connection, e: &Event, worker: F) -> Result<ResultOut, String>
where
    F: Fn(&str) -> Result<WorkerOutput, String>,
{
    submit_inner(conn, e, worker, None)
}

/// Root-aware worker submission. This is the only worker entry point that may
/// resolve registered authority sources, and it always applies agent privacy.
pub fn submit_with_root<F>(
    conn: &Connection,
    hex_root: &Path,
    e: &Event,
    worker: F,
) -> Result<ResultOut, String>
where
    F: Fn(&str) -> Result<WorkerOutput, String>,
{
    submit_inner(conn, e, worker, Some(hex_root))
}

fn submit_inner<F>(
    conn: &Connection,
    e: &Event,
    worker: F,
    hex_root: Option<&Path>,
) -> Result<ResultOut, String>
where
    F: Fn(&str) -> Result<WorkerOutput, String>,
{
    // 1. derive the assemble() query + optional pinned block
    let (query, pin) = if let Some(reply_to) = &e.reply_to {
        match messages::lookup(conn, reply_to)
            .map_err(|x| format!("messages lookup failed: {x}"))?
        {
            // S6: a dangling reply_to is a referential-integrity violation — loud, not silent.
            None => {
                return Err(format!(
                    "reply_to target {reply_to:?} not found (referential integrity)"
                ))
            }
            Some(m) => match (&m.prompt, &e.answer) {
                (Some(q), Some(ans)) => {
                    let r = messages::resolve::resolve_answer(q, ans)?;
                    (r.query, Some(r.pin))
                }
                (None, _) => (
                    format!(
                        "(reply to non-question {reply_to}) {}",
                        e.body.clone().unwrap_or_default()
                    ),
                    Some(format!(
                        "Note: reply_to target {reply_to} is not a question.\n"
                    )),
                ),
                (Some(_), None) => {
                    return Err(format!("reply to question {reply_to} carried no answer"))
                }
            },
        }
    } else {
        (e.body.clone().unwrap_or_default(), None)
    };

    // 2. persist inbound event (must-commit) + best-effort telemetry mirror
    persist_message(
        conn,
        &StoredMessage {
            id: e.id.clone(),
            source: e.source.clone(),
            kind: e.kind.clone(),
            body: e.body.clone(),
            reply_to: e.reply_to.clone(),
            answer: e.answer.clone(),
            prompt: None,
            ts: e.ts.clone(),
        },
    )?;

    // 3. assemble context, then build the worker input = retrieved context +
    //    the user-facing text. For a reply that text is the resolved pin (carries
    //    the chosen option's description); for a normal message it's the body.
    // Hot path (`harness::submit`, called from the UserPromptSubmit hook / worker
    // dispatch): pass `None` for `query_vec` so `assemble` runs in FTS-only mode
    // and does not construct an `Embedder`. Per spec Tj0b203yv, the hook is a
    // fresh OS process per user message and cold-loading the 522 MB nomic model
    // blows the latency budget; the embedder policy is caller-decided, not env-
    // gated, so this call site is *structurally* incapable of loading the model.
    let rendered = match hex_root {
        Some(root) => crate::memory::recall::context_with_connection(root, conn, &query, true),
        None => {
            let ctx = crate::memory::assemble::assemble(conn, &query, false, BUDGET, None);
            crate::memory::assemble::render_candidates(&ctx)
        }
    };
    let user_text = match &pin {
        Some(p) => p.clone(),
        None => e.body.clone().unwrap_or_default(),
    };
    let worker_input = if rendered.trim().is_empty() {
        user_text
    } else {
        format!("{rendered}\n\n{user_text}")
    };

    // 4. run the worker; a question output is persisted as the asked question
    match worker(&worker_input)? {
        WorkerOutput::Answer(text) => Ok(ResultOut {
            event_id: e.id.clone(),
            status: "done".into(),
            output: text,
            prompt: None,
        }),
        WorkerOutput::Question(p) => {
            persist_message(
                conn,
                &StoredMessage::question(p.id.clone(), "hex".into(), p.clone(), e.ts.clone()),
            )?;
            Ok(ResultOut {
                event_id: e.id.clone(),
                status: "done".into(),
                output: String::new(),
                prompt: Some(p),
            })
        }
    }
}

/// Must-commit persist + best-effort telemetry mirror (full body + message id).
fn persist_message(conn: &Connection, m: &StoredMessage) -> Result<(), String> {
    messages::insert(conn, m).map_err(|e| format!("messages insert failed (fatal): {e}"))?;
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: m.source.clone(),
        event: "message".into(),
        status: "ok".into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(
            serde_json::json!({"id": m.id, "kind": m.kind, "body": m.body, "prompt": m.prompt, "answer": m.answer})
                .to_string(),
        ),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, body: &str) -> Event {
        Event {
            id: id.into(),
            source: "test".into(),
            kind: "request".into(),
            body: Some(body.into()),
            reply_to: None,
            answer: None,
            refs: None,
            scope: None,
            ts: "2026-09-10T00:00:00Z".into(),
        }
    }

    fn worker_db() -> Connection {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::schema::apply_messages_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn submit_with_root_uses_authority_builder_and_agent_privacy() {
        let root = tempfile::TempDir::new().unwrap();
        let registry = root.path().join(".hex/config/memory-authority.toml");
        std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(root.path().join("docs")).unwrap();
        std::fs::write(
            registry,
            "version=1\n[[sources]]\nid='public'\npath='docs/public.md'\ntopics=['memory recall']\nauthority_status='current'\nprivate=false\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("docs/public.md"),
            "WORKER_PUBLIC_SOURCE_CANARY memory recall procedure",
        )
        .unwrap();
        let conn = worker_db();
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private) \
             VALUES ('private','memory','recall','WORKER_PRIVATE_FACT_CANARY',0.9,'2026-09-10','2026-09-10',1)",
            [],
        )
        .unwrap();
        let echo = |input: &str| Ok(WorkerOutput::Answer(input.to_owned()));
        let matched = submit_with_root(
            &conn,
            root.path(),
            &request("worker-matched", "how does memory recall work"),
            echo,
        )
        .unwrap();
        assert!(matched.output.contains("WORKER_PUBLIC_SOURCE_CANARY"));
        assert!(!matched.output.contains("WORKER_PRIVATE_FACT_CANARY"));

        let unmatched = submit_with_root(
            &conn,
            root.path(),
            &request(
                "worker-unmatched",
                "what private material does memory contain",
            ),
            echo,
        )
        .unwrap();
        assert!(!unmatched.output.contains("WORKER_PRIVATE_FACT_CANARY"));
    }

    #[test]
    fn submit_reply_pins_choice_and_persists() {
        use rusqlite::Connection;
        crate::memory::vector::register_sqlite_vec();
        let c = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&c).unwrap();
        crate::memory::schema::apply_plan2(&c).unwrap();
        crate::memory::schema::apply_messages_schema(&c).unwrap();
        let p = Prompt {
            id: "Q".into(),
            text: "Rebuy?".into(),
            multi: false,
            options: vec![Opt {
                id: "b".into(),
                label: "tilt".into(),
                description: "sell ETH on rebuy".into(),
            }],
        };
        crate::messages::insert(
            &c,
            &crate::messages::StoredMessage::question("Q".into(), "hex".into(), p, "t0".into()),
        )
        .unwrap();
        let e = Event {
            id: "E".into(),
            source: "mike-cli".into(),
            kind: "request".into(),
            body: None,
            reply_to: Some("Q".into()),
            answer: Some(Answer {
                selected: vec!["b".into()],
                free_text: None,
            }),
            refs: None,
            scope: None,
            ts: "t1".into(),
        };
        // fake worker echoes its input so we can assert the pin (with b's description) reached it
        let worker = |input: &str| {
            Ok::<_, String>(crate::worker::run::WorkerOutput::Answer(format!(
                "SAW::{input}"
            )))
        };
        let r = submit(&c, &e, worker).unwrap();
        assert_eq!(r.status, "done");
        assert!(
            r.output.contains("sell ETH on rebuy"),
            "pin reached worker: {}",
            r.output
        );
        assert!(crate::messages::lookup(&c, "E").unwrap().is_some());
    }

    #[test]
    fn event_and_prompt_roundtrip_json() {
        let p = Prompt {
            id: "Q1".into(),
            text: "pick".into(),
            multi: false,
            options: vec![Opt {
                id: "a".into(),
                label: "A".into(),
                description: "the a".into(),
            }],
        };
        let j = serde_json::to_string(&p).unwrap();
        let back: Prompt = serde_json::from_str(&j).unwrap();
        assert_eq!(back.options[0].label, "A");

        let e = Event {
            id: "E1".into(),
            source: "mike-cli".into(),
            kind: "request".into(),
            body: None,
            reply_to: Some("Q1".into()),
            answer: Some(Answer {
                selected: vec!["a".into()],
                free_text: None,
            }),
            refs: None,
            scope: None,
            ts: "2026-06-05T00:00:00Z".into(),
        };
        let j = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&j).unwrap();
        assert_eq!(back.answer.unwrap().selected, vec!["a".to_string()]);
    }
}
