//! Registry tests for `hex::workers::registry()`.
//!
//! Asserts the Rust registry surfaces `hex-memory-maintenance` and `hex-backup`
//! as cron workers, mirroring the earlier YAML configs in
//! `system/iii/workers/`. Two tests below encode real collision constraints
//! learned from incidents (the weekly maintain job's offset from the daily
//! backup, and the quick-consolidate offset from the full run); those stay
//! as named cron-literal assertions on purpose. Every other registered
//! worker's cron is checked generically: it must parse and it must fire
//! within the next 7 days, so a typo or a dead expression cannot sit
//! unnoticed the way four near-duplicate snapshot tests once let it.

use hex::worker::TriggerSpec;
use hex::workers;
use std::str::FromStr;

fn cron_exprs(w: &hex::worker::Worker) -> Vec<String> {
    w.handlers
        .iter()
        .filter_map(|(_name, spec, _)| match spec {
            TriggerSpec::Cron { expression } => Some(expression.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn workers_registry_contains_memory_maintenance_and_backup() {
    let reg: Vec<hex::worker::Worker> = workers::registry();
    let names: Vec<&str> = reg.iter().map(|w| w.name.as_str()).collect();
    assert!(
        names.contains(&"hex-memory-maintenance"),
        "expected hex-memory-maintenance in registry, got {:?}",
        names
    );
    assert!(
        names.contains(&"hex-backup"),
        "expected hex-backup in registry, got {:?}",
        names
    );
}

#[test]
fn workers_registry_memory_maintenance_has_weekly_maintain() {
    // `hex memory maintain --vacuum --backfill-facts` runs weekly - Sunday
    // 04:33Z, after the 04:00Z backup, offset off the :30 boundary so its
    // unlocked VACUUM doesn't collide with the 15-min index tick - so one-off
    // memory.db corruption (orphan vectors, FTS bloat, foreign transcript_files
    // rows) self-heals.
    let reg = workers::registry();
    let mm = reg
        .iter()
        .find(|w| w.name == "hex-memory-maintenance")
        .expect("hex-memory-maintenance worker must be registered");
    let exprs = cron_exprs(mm);
    assert!(
        exprs.iter().any(|e| e == "0 33 4 * * SUN *"),
        "expected weekly `hex memory maintain` cron '0 33 4 * * SUN *' in {:?}",
        exprs
    );
    assert_eq!(
        hex::workers::hex_modules::memory_maintenance::ARGV_MAINTAIN,
        &["hex", "memory", "maintain", "--vacuum", "--backfill-facts"],
    );
}

#[test]
fn workers_registry_quick_consolidate_offset_from_full_run() {
    // 2026-06-10: the 03:00:00Z full consolidation was lock-skipped behind a
    // quick tick that fired the same second. The quick cron must stay offset
    // from the :00 boundary (4x/hour at :05/:20/:35/:50).
    let reg = workers::registry();
    let mm = reg
        .iter()
        .find(|w| w.name == "hex-memory-maintenance")
        .expect("hex-memory-maintenance worker must be registered");
    let exprs = cron_exprs(mm);
    assert!(
        exprs.iter().any(|e| e == "0 5,20,35,50 * * * * *"),
        "expected quick-consolidate cron '0 5,20,35,50 * * * * *' in {:?}",
        exprs
    );
}

#[test]
fn workers_registry_oss_releaser_release_requested_event_and_watch_cron() {
    // oss-releaser (oss-releaser spec, scope item 6): exactly two triggers -
    // the `release.requested` event (a State trigger scope="events",
    // key="release.requested", the `.on_event` convention - the manual
    // escape hatch) and the every-5-minutes branch-watch cron.
    let reg = workers::registry();
    let w = reg
        .iter()
        .find(|w| w.name == "oss-releaser")
        .expect("oss-releaser worker must be registered");
    assert_eq!(
        w.handlers.len(),
        2,
        "oss-releaser must register exactly two handlers (event + watch cron)"
    );
    let specs: Vec<&TriggerSpec> = w.handlers.iter().map(|(_name, s, _h)| s).collect();
    assert!(
        specs.contains(&&TriggerSpec::State {
            scope: "events".to_string(),
            key: "release.requested".to_string(),
        }),
        "oss-releaser must trigger on events/release.requested"
    );
    assert!(
        specs.contains(&&TriggerSpec::Cron {
            expression: "0 */5 * * * * *".to_string(),
        }),
        "oss-releaser must poll watched repos every 5 minutes"
    );
}

#[test]
fn every_registered_cron_parses_and_fires_within_seven_days() {
    let reg = workers::registry();
    let now = chrono::Utc::now();
    for w in &reg {
        for expr in cron_exprs(w) {
            let schedule = cron::Schedule::from_str(&expr).unwrap_or_else(|e| {
                panic!(
                    "worker '{}': cron expression '{}' does not parse: {e}",
                    w.name, expr
                )
            });
            let next = schedule.after(&now).next();
            match next {
                Some(t) => {
                    let until = t - now;
                    assert!(
                        until < chrono::Duration::days(7),
                        "worker '{}': cron '{}' next fires at {} ({} from now), \
                         which is not within the next 7 days",
                        w.name,
                        expr,
                        t,
                        until
                    );
                }
                None => panic!(
                    "worker '{}': cron expression '{}' never fires again after {}",
                    w.name, expr, now
                ),
            }
        }
    }
}

#[test]
fn nightly_tests_worker_is_registered_with_one_nightly_cron() {
    let reg = workers::registry();
    let w = reg
        .iter()
        .find(|w| w.name == "hex-nightly-tests")
        .expect("hex-nightly-tests worker must be registered");
    assert_eq!(
        w.handlers.len(),
        1,
        "hex-nightly-tests must register exactly one handler, got {}",
        w.handlers.len()
    );
    let (_name, spec, _handler) = &w.handlers[0];
    assert_eq!(
        spec,
        &TriggerSpec::Cron {
            expression: hex::workers::hex_modules::nightly_tests::CRON_NIGHTLY.to_string(),
        },
        "hex-nightly-tests's one trigger must be a Cron trigger equal to CRON_NIGHTLY"
    );
}
