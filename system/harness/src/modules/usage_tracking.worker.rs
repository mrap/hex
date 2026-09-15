//! `hex-usage-tracking` — bounded local usage ledger collection.
//!
//! This is an existing-harness worker. It starts no daemon and sends no
//! alerts.
//!
//! Registers one named cron handler per source kind in
//! `hex::usage_kinds::ALL_SOURCE_KINDS`, each running
//! `hex usage collect --source-kind <kind> --max-records 1000`. One kind per
//! `ctx.run` call means a failure in one kind's collector never skips the
//! others, and the runtime records one telemetry row per handler invocation
//! (`hex-usage-tracking::<kind>`) — so each source's health is visible on its
//! own in `hex failures`/telemetry, not folded into a single opaque
//! ran-or-didn't row for all kinds. (decision
//! usage-ledger-provider-agnostic-2026-09-14)
use hex::usage_kinds::ALL_SOURCE_KINDS;
use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

pub const CRON_EVERY_5M: &str = "0 */5 * * * * *";
pub const MAX_RECORDS: &str = "1000";

/// Build the argv for a single source kind's collect run:
/// `["hex", "usage", "collect", "--source-kind", <kind>, "--max-records", "1000"]`.
pub fn argv_for_kind(kind: &str) -> Vec<String> {
    [
        "hex",
        "usage",
        "collect",
        "--source-kind",
        kind,
        "--max-records",
        MAX_RECORDS,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Build the cron handler for one source kind. Kept separate from
/// `argv_for_kind` so the argv shape is independently unit-testable.
fn handler_for_kind(kind: &'static str) -> impl Fn(Event, Ctx) -> Result<()> + Send + Sync + 'static {
    move |_event: Event, ctx: Ctx| ctx.run(&argv_for_kind(kind)).map(|_| ())
}

/// Build the `hex-usage-tracking` worker: one named cron handler per source
/// kind (name == the kind string), so the runtime records
/// `hex-usage-tracking::<kind>` telemetry per kind independently.
pub fn worker() -> Worker {
    ALL_SOURCE_KINDS
        .iter()
        .copied()
        .fold(Worker::new("hex-usage-tracking"), |w, kind| {
            w.on_cron_named(kind, CRON_EVERY_5M, handler_for_kind(kind))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex::worker::TriggerSpec;

    /// One named cron handler per `ALL_SOURCE_KINDS` entry, each named
    /// exactly the kind string, each on the 5-minute cron expression.
    #[test]
    fn registers_one_named_handler_per_source_kind() {
        let w = worker();
        assert_eq!(w.name, "hex-usage-tracking");
        assert_eq!(w.handlers.len(), ALL_SOURCE_KINDS.len());

        let names: Vec<Option<String>> = w.handlers.iter().map(|(n, _, _)| n.clone()).collect();
        for kind in ALL_SOURCE_KINDS {
            assert!(
                names.contains(&Some(kind.to_string())),
                "missing handler named {kind}"
            );
        }

        for (_, spec, _) in &w.handlers {
            assert_eq!(
                spec,
                &TriggerSpec::Cron {
                    expression: CRON_EVERY_5M.to_string()
                }
            );
        }
    }

    /// Argv for each kind is exactly `hex usage collect --source-kind <kind>
    /// --max-records 1000` — one kind per `ctx.run` call, so one collector's
    /// failure never skips the others.
    #[test]
    fn argv_for_each_kind_is_source_kind_scoped() {
        for kind in ALL_SOURCE_KINDS {
            assert_eq!(
                argv_for_kind(kind),
                vec![
                    "hex".to_string(),
                    "usage".to_string(),
                    "collect".to_string(),
                    "--source-kind".to_string(),
                    kind.to_string(),
                    "--max-records".to_string(),
                    "1000".to_string(),
                ]
            );
        }
    }
}
