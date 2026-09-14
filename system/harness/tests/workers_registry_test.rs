//! Red test for task Tfr6deqwv: workers::registry().
//!
//! Asserts the Rust registry surfaces `hex-memory-maintenance` and `hex-backup`
//! as cron workers, mirroring the existing YAML configs in
//! `system/iii/workers/`. The two memory-maintenance jobs must match the
//! YAML exactly: `hex memory index` @ "0 */15 * * * * *" and
//! `hex memory consolidate full` @ "0 0 3 * * * *".

use hex::worker::TriggerSpec;
use hex::workers;

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
fn workers_registry_memory_maintenance_cron_matches_yaml() {
    let reg = workers::registry();
    let mm = reg
        .iter()
        .find(|w| w.name == "hex-memory-maintenance")
        .expect("hex-memory-maintenance worker must be registered");
    let exprs = cron_exprs(mm);
    assert!(
        exprs.iter().any(|e| e == "0 */15 * * * * *"),
        "expected `hex memory index` cron '0 */15 * * * * *' in {:?}",
        exprs
    );
    assert!(
        exprs.iter().any(|e| e == "0 0 3 * * * *"),
        "expected `hex memory consolidate full` cron '0 0 3 * * * *' in {:?}",
        exprs
    );
}

#[test]
fn workers_registry_memory_maintenance_has_weekly_maintain() {
    // `hex memory maintain --vacuum --backfill-facts` runs weekly — Sunday
    // 04:33Z, after the 04:00Z backup, offset off the :30 boundary so its
    // unlocked VACUUM doesn't collide with the 15-min index tick — so one-off
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
fn workers_registry_freshness_daily_0900() {
    // hex-freshness: daily ledger freshness alerting (agent-infra P0, E0 step 4).
    // 09:00 PT = 16:00 UTC — engine crons evaluate UTC (telemetry-consumption
    // proposal: the original "0 0 9" fired at 02:00 PT, while Mike slept).
    let reg = workers::registry();
    let fr = reg
        .iter()
        .find(|w| w.name == "hex-freshness")
        .expect("hex-freshness worker must be registered");
    let exprs = cron_exprs(fr);
    assert!(
        exprs.iter().any(|e| e == "0 0 16 * * * *"),
        "expected hex-freshness cron '0 0 16 * * * *' (09:00 PT / 16:00 UTC) in {:?}",
        exprs
    );
}

#[test]
fn workers_registry_backup_is_cron_worker() {
    let reg = workers::registry();
    let bk = reg
        .iter()
        .find(|w| w.name == "hex-backup")
        .expect("hex-backup worker must be registered");
    assert!(
        !cron_exprs(bk).is_empty(),
        "hex-backup must have at least one cron-triggered handler"
    );
}

#[test]
fn workers_registry_oss_releaser_release_requested_event_and_watch_cron() {
    // oss-releaser (oss-releaser spec, scope item 6): exactly two triggers —
    // the `release.requested` event (a State trigger scope="events",
    // key="release.requested", the `.on_event` convention — the manual
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
fn workers_registry_build_cache_guard_hourly_at_15() {
    // hex-build-cache-guard: hourly at :15, offset from the :00 memory-index
    // tick so the two don't contend for the same second.
    let reg = workers::registry();
    let w = reg
        .iter()
        .find(|w| w.name == "hex-build-cache-guard")
        .expect("hex-build-cache-guard worker must be registered");
    let exprs = cron_exprs(w);
    assert_eq!(
        exprs.len(),
        1,
        "hex-build-cache-guard must have exactly one cron trigger, got {:?}",
        exprs
    );
    assert_eq!(
        exprs[0],
        hex::workers::hex_modules::build_cache_guard::CRON_HOURLY,
        "hex-build-cache-guard's cron must equal CRON_HOURLY"
    );
}
