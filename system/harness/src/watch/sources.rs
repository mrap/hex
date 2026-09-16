//! Source adapters: turn `(source, match, since)` into hits.
//!
//! Contract (spec `docs/hex-watch.md`, section 2): an adapter narrows at the
//! source where it can (gmail injects `after:<epoch>`), raises on transport
//! errors, and never decides "old". The tick applies the since guard.
//!
//! - `gmail`: runs the configured shell command (default: the instance's
//!   `gmail-search --json`), parses one JSON object per line.
//! - `event`: reads the newest envelope at iii state `events/<name>` through
//!   the tick's [`Substrate`](super::tick::Substrate). Last-write-wins.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::store::parse_iso;

/// One matching event from a poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Stable dedupe id: Gmail message id, `event@ts`.
    pub key: String,
    /// When it happened, epoch ms. `0` = unknown (trusted, logged).
    pub at_ms: i64,
    /// Becomes `WATCH_<FIELD>` in the action's env.
    pub fields: BTreeMap<String, String>,
}

/// Quote for `sh -c`: single quotes, with embedded single quotes escaped.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build the gmail command line from the configured template.
pub fn gmail_command(template: &str, query: &str, account: &str) -> String {
    template
        .replace("{query}", &shell_quote(query))
        .replace("{account}", &shell_quote(account))
}

/// Gmail query with the since guard pushed server-side.
pub fn gmail_query(
    match_: &BTreeMap<String, String>,
    since: Option<DateTime<Utc>>,
) -> Result<String, String> {
    let q = match_
        .get("query")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "gmail watch has no match.query".to_string())?;
    Ok(match since {
        Some(t) => format!("{q} after:{}", t.timestamp()),
        None => q.to_string(),
    })
}

pub fn gmail_account(match_: &BTreeMap<String, String>) -> String {
    match_
        .get("account")
        .cloned()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "primary".to_string())
}

/// Parse `gmail-search --json` output: one object per line, other lines
/// ignored. A line that starts with `{` but does not parse is an error
/// (S6: never a silent "0 results").
pub fn parse_gmail_output(stdout: &str, account: &str) -> Result<Vec<Hit>, String> {
    let mut hits = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .map_err(|e| format!("gmail-search line unparseable: {e}: {line}"))?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let ms = v.get("internal_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        let mut fields = BTreeMap::new();
        fields.insert("msg_id".into(), s("id"));
        fields.insert("subject".into(), s("subject"));
        fields.insert("from".into(), s("from"));
        fields.insert("date".into(), s("date"));
        fields.insert(
            "internal_ms".into(),
            if ms > 0 {
                ms.to_string()
            } else {
                String::new()
            },
        );
        fields.insert(
            "account".into(),
            if s("account").is_empty() {
                account.to_string()
            } else {
                s("account")
            },
        );
        hits.push(Hit {
            key: s("id"),
            at_ms: ms,
            fields,
        });
    }
    Ok(hits)
}

/// Gmail-only env aliases the rafting-era actions rely on.
pub fn gmail_aliases(hit: &Hit) -> Vec<(String, String)> {
    let f = |k: &str| hit.fields.get(k).cloned().unwrap_or_default();
    vec![
        ("MAIL_MSG_ID".into(), f("msg_id")),
        ("MAIL_SUBJECT".into(), f("subject")),
        ("MAIL_FROM".into(), f("from")),
        ("MAIL_DATE".into(), f("date")),
        ("MAIL_INTERNAL_MS".into(), f("internal_ms")),
        ("MAIL_ACCOUNT".into(), f("account")),
    ]
}

pub fn event_name(match_: &BTreeMap<String, String>) -> Result<String, String> {
    match_
        .get("event")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "event watch has no match.event".to_string())
}

/// Turn an `events/<name>` envelope (`{event, producer, ts, data}`) into a hit.
pub fn hit_from_envelope(name: &str, env: &Value) -> Hit {
    let s = |k: &str| {
        env.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    let ts = s("ts");
    let at_ms = parse_iso(&ts).map(|t| t.timestamp_millis()).unwrap_or(0);
    let mut fields = BTreeMap::new();
    fields.insert(
        "event".into(),
        if s("event").is_empty() {
            name.to_string()
        } else {
            s("event")
        },
    );
    fields.insert("producer".into(), s("producer"));
    fields.insert("ts".into(), ts.clone());
    let data = env
        .get("data")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    if let Some(obj) = data.as_object() {
        for (k, v) in obj {
            let sv = match v {
                Value::String(x) => x.clone(),
                other => other.to_string(),
            };
            fields.insert(format!("data_{k}"), sv);
        }
    }
    fields.insert("data".into(), data.to_string());
    Hit {
        key: format!("{name}@{ts}"),
        at_ms,
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_command_quotes_placeholders_for_sh() {
        let c = gmail_command(
            "gs --account {account} --json {query} 5",
            "from:a b's",
            "legacy",
        );
        assert_eq!(c, "gs --account 'legacy' --json 'from:a b'\\''s' 5");
    }

    #[test]
    fn gmail_query_injects_after_epoch_only_with_since() {
        let mut m = BTreeMap::new();
        m.insert("query".to_string(), "from:x".to_string());
        let t: DateTime<Utc> = "2026-09-16T12:00:00Z".parse().unwrap();
        assert_eq!(
            gmail_query(&m, Some(t)).unwrap(),
            format!("from:x after:{}", t.timestamp())
        );
        assert_eq!(gmail_query(&m, None).unwrap(), "from:x");
        m.clear();
        assert!(gmail_query(&m, None)
            .unwrap_err()
            .contains("no match.query"));
    }

    #[test]
    fn gmail_output_parses_json_lines_and_is_loud_on_broken_json() {
        let out = "# noise\n{\"account\":\"a@b\",\"id\":\"m1\",\"internal_ms\":1789574229000,\"date\":\"D\",\"from\":\"F\",\"subject\":\"S\"}\n";
        let hits = parse_gmail_output(out, "primary").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "m1");
        assert_eq!(hits[0].at_ms, 1789574229000);
        assert_eq!(hits[0].fields["subject"], "S");
        assert_eq!(hits[0].fields["account"], "a@b");
        assert!(parse_gmail_output("{broken", "primary").is_err());
        assert!(parse_gmail_output("", "primary").unwrap().is_empty());
    }

    #[test]
    fn envelope_becomes_hit_with_data_fields_and_ts_key() {
        let env = serde_json::json!({"event":"deploy.done","producer":"ci","ts":"2026-09-16T17:41:59.275060+00:00","data":{"id":"d42","n":2}});
        let h = hit_from_envelope("deploy.done", &env);
        assert_eq!(h.key, "deploy.done@2026-09-16T17:41:59.275060+00:00");
        assert_eq!(h.at_ms, 1789580519275);
        assert_eq!(h.fields["data_id"], "d42");
        assert_eq!(h.fields["data_n"], "2");
        assert_eq!(h.fields["producer"], "ci");
        assert!(h.fields["data"].contains("d42"));
    }
}
