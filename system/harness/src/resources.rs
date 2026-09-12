//! `hex-resources` — disk/resource sampling (tier 0) + deterministic pressure
//! rules (tier 1). Detection + emission only: NEVER cleans anything up
//! (proposal: telemetry-consumption-layer v2, C2).

use std::collections::BTreeMap;

/// Watched directories — hardcoded const, seeded from the 2026-06-11 cruft
/// survey offenders. Promote to a config file only after edits prove churn
/// (review: a config file means format+parser+validation for a ~quarterly
/// list). `~` is expanded against $HOME at runtime.
pub const WATCH_LIST: &[&str] = &[
    "~/github.com/mrap/boi/target",
    "~/github.com/mrap/hex-foundation/target",
    "~/hex/target",
    "~/hex/.hex/harness/target",
    "~/hex/raw",
    "~/.boi/v2",
    "~/worktrees",
    "~/Library/pnpm",
    "~/.npm",
    "~/.iii/cache",
    "~/.claude",
];

/// Floor: alert + pressure when root free space drops below this.
pub const FLOOR_FREE_GB: i64 = 150;
/// Trend: alert + pressure when a watched dir grows more than this across
/// the trend window.
pub const TREND_GROWTH_GB: i64 = 20;
pub const TREND_WINDOW_HOURS: i64 = 72;
/// Re-run the (13s) du pass when this much free space vanished since the
/// last du sample, else every DU_INTERVAL_HOURS.
pub const DU_DELTA_GB: i64 = 30;
pub const DU_INTERVAL_HOURS: i64 = 6;

pub fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => p.to_string(),
    }
}

/// `WATCH_LIST`, expanded to the absolute-path form that both `sample_tick`
/// (when it builds `dirs` for a du pass) and `evaluate_rules`'s trend loop
/// (R3/KTD3, to filter out non-watched keys) need to agree on. One helper so
/// the two sides can never drift apart.
fn watch_list_expanded() -> Vec<String> {
    WATCH_LIST.iter().map(|d| expand_home(d)).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct DfSample {
    pub free_gb: i64,
    pub used_gb: i64,
}

/// Parse `df -k /` output (KiB blocks → GB, truncating).
pub fn parse_df(out: &str) -> Option<DfSample> {
    let line = out.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    let used_kb: i64 = cols.get(2)?.parse().ok()?;
    let free_kb: i64 = cols.get(3)?.parse().ok()?;
    Some(DfSample {
        free_gb: free_kb / 1_048_576,
        used_gb: used_kb / 1_048_576,
    })
}

pub fn sample_df() -> Option<DfSample> {
    let out = std::process::Command::new("df")
        .args(["-k", "/"])
        .output()
        .ok()?;
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// Exclusion masks applied to `du` during the on-pressure discovery pass only
/// (KTD6/R7). Discovery walks `$HOME`'s direct children (see `sample_tick`),
/// which puts `~/Library` in scope; without an exclusion, `du` recurses into
/// `~/Library/CloudStorage` (the iCloud virtual mount) and reports
/// phantom/inflated sizes that don't reflect real local disk use. `WATCH_LIST`
/// never needs this (none of its entries sit under `~/Library`), so the
/// watch-list `du_sizes` path below passes no masks and its command/output
/// stay byte-for-byte identical to before this constant existed.
pub const DU_EXCLUDE_MASKS: &[&str] = &["CloudStorage"];

/// Build the `du` argv for one dir: `-sk`, then BSD `du -I <mask>` once per
/// mask (a mask match is ignored anywhere in the recursive walk), then the
/// dir itself.
fn du_args<'a>(dir: &'a str, masks: &[&'a str]) -> Vec<&'a str> {
    let mut args: Vec<&str> = vec!["-sk"];
    for m in masks {
        args.push("-I");
        args.push(m);
    }
    args.push(dir);
    args
}

/// Raw `du -sk` KiB result for one dir (before GB truncation), honoring
/// `masks`. `None` for a missing dir or a `du` invocation that fails or
/// doesn't parse.
fn du_kb(dir: &str, masks: &[&str]) -> Option<i64> {
    if !std::path::Path::new(dir).exists() {
        return None;
    }
    let o = std::process::Command::new("du")
        .args(du_args(dir, masks))
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .next()?
        .parse::<i64>()
        .ok()
}

fn du_sizes_with_masks(dirs: &[String], masks: &[&str]) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    for d in dirs {
        if let Some(kb) = du_kb(d, masks) {
            out.insert(d.clone(), kb / 1_048_576);
        }
    }
    out
}

/// `du -sk` per watched dir (expanded), GB truncating. Missing/unreadable
/// dirs are skipped silently — a watched dir that was cleaned up is normal.
/// No exclusion masks — command/behavior are unchanged from before KTD6.
pub fn du_sizes(dirs: &[String]) -> BTreeMap<String, i64> {
    du_sizes_with_masks(dirs, &[])
}

/// Discovery-only `du`: same as [`du_sizes`] but honors `DU_EXCLUDE_MASKS`
/// (KTD6/R7) so a virtual mount like iCloud's CloudStorage never inflates a
/// discovery-pass size.
pub fn du_sizes_discovery(dirs: &[String]) -> BTreeMap<String, i64> {
    du_sizes_with_masks(dirs, DU_EXCLUDE_MASKS)
}

/// Persist samples as telemetry rows (durable trend history).
/// df row every call: event=sample::df, detail {"free_gb":N,"used_gb":N}.
/// du row when taken:  event=sample::du, detail {"<dir>":gb,...}.
pub fn record_df(d: &DfSample) {
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "hex-resources".into(),
        event: "sample::df".into(),
        status: "ok".into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(serde_json::json!({ "free_gb": d.free_gb, "used_gb": d.used_gb }).to_string()),
    });
}

pub fn record_du(sizes: &BTreeMap<String, i64>) {
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "hex-resources".into(),
        event: "sample::du".into(),
        status: "ok".into(),
        duration_ms: None,
        exit_code: None,
        detail: serde_json::to_string(sizes).ok(),
    });
}

/// Persist an on-pressure discovery pass under a DISTINCT event name
/// (KTD5/R6) so `evaluate_rules`'s trend loop — which filters on
/// `event='sample::du'` — never compares discovery-sourced sizes across
/// ticks. Same detail shape as [`record_du`].
pub fn record_du_discovery(sizes: &BTreeMap<String, i64>) {
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "hex-resources".into(),
        event: "sample::du-discovery".into(),
        status: "ok".into(),
        duration_ms: None,
        exit_code: None,
        detail: serde_json::to_string(sizes).ok(),
    });
}

#[derive(Debug, Clone, PartialEq)]
pub enum Breach {
    Floor {
        free_gb: i64,
    },
    Trend {
        dir: String,
        growth_gb: i64,
        window_hours: i64,
    },
}

/// Compose the `Breach::Floor` alert message: the base floor line, plus (R5)
/// the three largest entries of the already-loaded `last_du` map, sorted
/// descending by size, using whatever path form the map already holds
/// (watch-list entries are `expand_home`-expanded by the time they're
/// recorded). An empty map (no du sample yet) yields the base line only.
fn floor_message(free_gb: i64, last_du: &BTreeMap<String, i64>) -> String {
    let base = format!("root free space {free_gb}G < {FLOOR_FREE_GB}G floor");
    let mut entries: Vec<(&String, &i64)> = last_du.iter().collect();
    entries.sort_by(|a, b| b.1.cmp(a.1));
    let top: Vec<String> = entries
        .into_iter()
        .take(3)
        .map(|(d, g)| format!("{d} {g}G"))
        .collect();
    if top.is_empty() {
        base
    } else {
        format!("{base}; top: {}", top.join(", "))
    }
}

/// Deterministic tier-1 rules over the current df sample + du history rows.
/// LEVEL-TRIGGERED by design: callers re-evaluate every sample tick and
/// re-emit while in breach (at-most-once event delivery means a single edge
/// emit can vanish; alert::notify's 6h dedupe caps human-facing noise).
pub fn evaluate_rules(
    df: &DfSample,
    now: chrono::DateTime<chrono::Utc>,
) -> rusqlite::Result<Vec<Breach>> {
    let mut out = Vec::new();
    if df.free_gb < FLOOR_FREE_GB {
        out.push(Breach::Floor {
            free_gb: df.free_gb,
        });
    }
    // No telemetry store yet → no du history → floor rule only. (open_ro on a
    // missing file is an open error, not empty history — never create the db
    // from a read-only consumer.)
    if !crate::telemetry::db_exists() {
        return Ok(out);
    }
    // Trend: compare oldest du sample inside the window to the newest.
    let conn = crate::telemetry::open_ro()?;
    let since = (now - chrono::Duration::hours(TREND_WINDOW_HOURS)).to_rfc3339();
    let mut stmt = conn.prepare(
        "SELECT detail FROM events
         WHERE source='hex-resources' AND event='sample::du' AND ts >= ?1 AND detail IS NOT NULL
         ORDER BY ts",
    )?;
    let details: Vec<String> = stmt
        .query_map([&since], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    if details.len() >= 2 {
        let parse =
            |s: &str| -> BTreeMap<String, i64> { serde_json::from_str(s).unwrap_or_default() };
        let oldest = parse(&details[0]);
        let newest = parse(details.last().unwrap());
        // R3/KTD3: a key not in WATCH_LIST (e.g. a stale on-pressure
        // discovery sample recorded before the discovery event rename) never
        // produces a trend breach, however much it grew. Exact-key match
        // against the same expanded form sample_tick writes — not a prefix
        // check, so a watched dir's parent (e.g. `~/Library` next to the
        // watched `~/Library/pnpm`) stays excluded too.
        let watched = watch_list_expanded();
        for (dir, new_gb) in &newest {
            if !watched.contains(dir) {
                continue;
            }
            if let Some(old_gb) = oldest.get(dir) {
                let growth = new_gb - old_gb;
                if growth > TREND_GROWTH_GB {
                    out.push(Breach::Trend {
                        dir: dir.clone(),
                        growth_gb: growth,
                        window_hours: TREND_WINDOW_HOURS,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// One sampler tick. Policy:
/// - df every tick (4ms).
/// - du when none in the last DU_INTERVAL_HOURS OR free fell ≥ DU_DELTA_GB
///   since the last du tick (attribution data for the trend rule).
/// - On breach: alert (deduped) + emit resource.pressure (LEVEL-triggered:
///   re-emitted every tick while in breach) + on-pressure-only discovery
///   pass and docker probe (gated on OrbStack actually running — du
///   under-reports docker ~300x and `docker system df` can wake the VM).
pub fn sample_tick(now: chrono::DateTime<chrono::Utc>) -> Result<Vec<Breach>, String> {
    let df = sample_df().ok_or("df sample failed")?;
    record_df(&df);

    let conn = crate::telemetry::open_ro().map_err(|e| e.to_string())?;
    let last_du: Option<(String, String)> = conn
        .query_row(
            "SELECT ts, COALESCE(detail,'') FROM events
             WHERE source='hex-resources' AND event='sample::du'
             ORDER BY ts DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    // Parse the already-loaded last_du row's detail for the floor message
    // (R5) — no extra query, just the JSON already sitting in `last_du`.
    // Mutable: when this tick itself refreshes du (below, `du_due`), the
    // freshly computed sizes are merged in so the floor message reflects
    // this tick's own du pass rather than the stale pre-tick DB row.
    let mut last_du_sizes: BTreeMap<String, i64> = last_du
        .as_ref()
        .and_then(|(_, detail)| serde_json::from_str(detail).ok())
        .unwrap_or_default();
    let last_df_free: Option<i64> = last_du.as_ref().and_then(|(ts, _)| {
        conn.query_row(
            "SELECT detail FROM events
             WHERE source='hex-resources' AND event='sample::df' AND ts <= ?1
             ORDER BY ts DESC LIMIT 1",
            [ts],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|d| serde_json::from_str::<serde_json::Value>(&d).ok())
        .and_then(|v| v["free_gb"].as_i64())
    });
    let du_due = match &last_du {
        None => true,
        Some((ts, _)) => {
            chrono::DateTime::parse_from_rfc3339(ts)
                .map(|t| (now - t.with_timezone(&chrono::Utc)).num_hours() >= DU_INTERVAL_HOURS)
                .unwrap_or(true)
                || last_df_free.is_some_and(|prev| prev - df.free_gb >= DU_DELTA_GB)
        }
    };
    if du_due {
        let dirs: Vec<String> = watch_list_expanded();
        let fresh_du_sizes = du_sizes(&dirs);
        record_du(&fresh_du_sizes);
        // Union fresh over stale: this tick's own du pass wins per-dir, but a
        // dir that this pass skipped (missing/unreadable — du_sizes drops it
        // silently) still falls back to whatever the last successful sample
        // saw, instead of vanishing from the floor message entirely.
        last_du_sizes.extend(fresh_du_sizes);
    }

    let breaches = evaluate_rules(&df, now).map_err(|e| e.to_string())?;
    for b in &breaches {
        let (key_ident, msg, data) = match b {
            Breach::Floor { free_gb } => (
                "floor".to_string(),
                floor_message(*free_gb, &last_du_sizes),
                serde_json::json!({ "category": "floor", "free_gb": free_gb }),
            ),
            Breach::Trend {
                dir,
                growth_gb,
                window_hours,
            } => (
                format!("trend-{dir}"),
                format!("{dir} grew {growth_gb}G in {window_hours}h"),
                serde_json::json!({ "category": "trend", "path": dir,
                    "growth_gb": growth_gb, "window_hours": window_hours }),
            ),
        };
        crate::alert::notify(
            &crate::failures::alert_key("resource", &key_ident),
            "resource pressure",
            &msg,
        );
        // Level-triggered emission. ops::emit signature on this branch is
        // emit(event, data, producer: Option<&str>) — adapted from the plan's
        // sketch per its VERIFY note.
        if let Err(e) = crate::ops::emit("resource.pressure", data.clone(), Some("hex-resources")) {
            eprintln!("resources: pressure emit failed (engine down?): {e}");
        }
    }
    if !breaches.is_empty() {
        // On-pressure attribution: discovery pass over $HOME top-level (find
        // NEW offenders — survey lesson: they shift) + docker logical sizes,
        // only if OrbStack is already running (never wake the VM).
        if let Ok(home) = std::env::var("HOME") {
            let tops: Vec<String> = std::fs::read_dir(&home)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().is_dir())
                        .map(|e| e.path().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            // Distinct event name (KTD5/R6) so evaluate_rules's watch-list
            // trend loop never sees discovery rows, and CloudStorage-excluded
            // du (KTD6/R7) so ~/Library's discovery size isn't inflated by
            // the iCloud virtual mount.
            record_du_discovery(&du_sizes_discovery(&tops));
        }
        let orb_running = std::process::Command::new("pgrep")
            .args(["-x", "OrbStack"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if orb_running {
            if let Ok(o) = std::process::Command::new("docker")
                .args(["system", "df"])
                .output()
            {
                crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                    source: "hex-resources".into(),
                    event: "sample::docker".into(),
                    status: "ok".into(),
                    duration_ms: None,
                    exit_code: None,
                    detail: Some(
                        String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .collect::<Vec<_>>()
                            .join(" | "),
                    ),
                });
            }
        }
    }
    Ok(breaches)
}

#[cfg(test)]
mod rule_tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    fn seed_row(event: &str, ts: chrono::DateTime<Utc>, detail: &str) {
        // schema first
        crate::telemetry::record(&crate::telemetry::TelemetryEvent {
            source: "seed".into(),
            event: "seed".into(),
            status: "ok".into(),
            duration_ms: None,
            exit_code: None,
            detail: None,
        })
        .unwrap();
        let conn = rusqlite::Connection::open(
            std::path::PathBuf::from(std::env::var("HEX_DIR").unwrap())
                .join(".hex/telemetry/events.db"),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (ts, source, event, status, detail) VALUES (?1,'hex-resources',?2,'ok',?3)",
            rusqlite::params![ts.to_rfc3339(), event, detail]).unwrap();
    }

    #[test]
    fn floor_breach_detected() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        let breaches = evaluate_rules(
            &DfSample {
                free_gb: 100,
                used_gb: 900,
            },
            now,
        )
        .unwrap();
        assert!(breaches
            .iter()
            .any(|b| matches!(b, Breach::Floor { free_gb: 100 })));
    }

    #[test]
    fn trend_breach_from_history() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        // R3/KTD3: the trend loop now only reports breaches for keys in
        // WATCH_LIST, so the seeded dir must be one — use its expanded form,
        // the same one sample_tick writes.
        let watched = expand_home("~/hex/target");
        seed_row(
            "sample::du",
            now - Duration::hours(70),
            &format!(r#"{{"{watched}":5}}"#),
        );
        seed_row(
            "sample::du",
            now - Duration::hours(1),
            &format!(r#"{{"{watched}":40}}"#),
        );
        let breaches = evaluate_rules(
            &DfSample {
                free_gb: 999,
                used_gb: 1,
            },
            now,
        )
        .unwrap();
        match breaches.iter().find(|b| matches!(b, Breach::Trend { .. })) {
            Some(Breach::Trend { dir, growth_gb, .. }) => {
                assert_eq!(dir, &watched);
                assert_eq!(*growth_gb, 35);
            }
            _ => panic!("expected trend breach: {breaches:?}"),
        }
    }

    #[test]
    fn no_breach_when_healthy() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        // Watched key, small growth — no breach because growth stays under
        // TREND_GROWTH_GB (not because the key gets filtered out).
        let watched = expand_home("~/hex/target");
        seed_row(
            "sample::du",
            now - Duration::hours(70),
            &format!(r#"{{"{watched}":5}}"#),
        );
        seed_row(
            "sample::du",
            now - Duration::hours(1),
            &format!(r#"{{"{watched}":6}}"#),
        );
        let breaches = evaluate_rules(
            &DfSample {
                free_gb: 999,
                used_gb: 1,
            },
            now,
        )
        .unwrap();
        assert!(breaches.is_empty(), "{breaches:?}");
    }

    /// R3/KTD3: a `sample::du` row for a directory that isn't in `WATCH_LIST`
    /// (e.g. an old on-pressure discovery sample recorded before the
    /// discovery event rename) must never produce a trend breach, no matter
    /// how much it grew. `<home>/Library` is deliberately the *parent* of the
    /// watched `~/Library/pnpm` entry — proves the check is exact-key
    /// membership, not a prefix match.
    #[test]
    fn trend_ignores_non_watch_list_dir() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        let non_watched = expand_home("~/Library");
        seed_row(
            "sample::du",
            now - Duration::hours(70),
            &format!(r#"{{"{non_watched}":5}}"#),
        );
        seed_row(
            "sample::du",
            now - Duration::hours(1),
            &format!(r#"{{"{non_watched}":505}}"#),
        );
        let breaches = evaluate_rules(
            &DfSample {
                free_gb: 999,
                used_gb: 1,
            },
            now,
        )
        .unwrap();
        assert!(breaches.is_empty(), "{breaches:?}");
    }

    /// R6/KTD5: discovery samples land under a distinct event name, so a huge
    /// growth between two `sample::du-discovery` rows never registers as a
    /// trend breach — evaluate_rules's trend loop only ever reads
    /// `event='sample::du'`.
    #[test]
    fn discovery_samples_excluded_from_trend() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        seed_row(
            "sample::du-discovery",
            now - Duration::hours(70),
            r#"{"/x/Library":5}"#,
        );
        seed_row(
            "sample::du-discovery",
            now - Duration::hours(1),
            r#"{"/x/Library":500}"#,
        );
        let breaches = evaluate_rules(
            &DfSample {
                free_gb: 999,
                used_gb: 1,
            },
            now,
        )
        .unwrap();
        assert!(breaches.is_empty(), "{breaches:?}");
    }

    /// R6/KTD5: the production discovery-recording function itself writes
    /// under `sample::du-discovery`, not `sample::du` — proves the wiring in
    /// `sample_tick`'s on-pressure block, not just the query filter.
    #[test]
    fn record_du_discovery_uses_distinct_event_name() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        let mut sizes = BTreeMap::new();
        sizes.insert("/x/Library".to_string(), 42);
        record_du_discovery(&sizes);
        let conn = crate::telemetry::open_ro().unwrap();
        let (event, detail): (String, String) = conn
            .query_row(
                "SELECT event, COALESCE(detail,'') FROM events
                 WHERE source='hex-resources' ORDER BY ts DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(event, "sample::du-discovery");
        assert!(detail.contains("/x/Library"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// df -k output fixture (macOS shape) → free/used GB.
    #[test]
    fn parses_df_output() {
        let fixture = "Filesystem   1024-blocks       Used  Available Capacity iused ifree %iused  Mounted on\n/dev/disk3s1s1  1942700360  248000000 1536000000    14%  500000 4294467295    0%   /\n";
        let d = parse_df(fixture).unwrap();
        assert_eq!(d.free_gb, 1464); // 1536000000 KiB / 1048576 ≈ 1464.8 → trunc(1464)
        assert_eq!(d.used_gb, 236);
    }

    /// du over a tempdir returns a size; missing dirs are skipped (None), not errors.
    #[test]
    fn du_sizes_tolerate_missing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("f"), vec![0u8; 1024 * 100]).unwrap();
        let sizes = du_sizes(&[
            tmp.path().to_string_lossy().into_owned(),
            "/nonexistent/definitely/missing".to_string(),
        ]);
        assert!(sizes.contains_key(tmp.path().to_string_lossy().as_ref()));
        assert!(!sizes.contains_key("/nonexistent/definitely/missing"));
    }

    /// R5: three-or-more `last_du` entries → message ends with the three
    /// largest, descending by size.
    #[test]
    fn floor_message_lists_three_largest_descending() {
        let mut m = BTreeMap::new();
        m.insert("~/.boi".to_string(), 151);
        m.insert("~/worktrees".to_string(), 154);
        m.insert("~/hex/target".to_string(), 12);
        m.insert("~/.npm".to_string(), 3);
        let msg = floor_message(100, &m);
        assert!(
            msg.ends_with("top: ~/worktrees 154G, ~/.boi 151G, ~/hex/target 12G"),
            "{msg}"
        );
    }

    /// R5: a single `last_du` entry → message lists exactly one.
    #[test]
    fn floor_message_single_entry() {
        let mut m = BTreeMap::new();
        m.insert("~/only".to_string(), 42);
        let msg = floor_message(10, &m);
        assert_eq!(msg, "root free space 10G < 150G floor; top: ~/only 42G");
    }

    /// R5: no `last_du` entries yet → base line only, no dangling "top:".
    #[test]
    fn floor_message_empty_map() {
        let m = BTreeMap::new();
        let msg = floor_message(5, &m);
        assert_eq!(msg, "root free space 5G < 150G floor");
    }

    /// R7/KTD6: discovery `du` over a fixture home whose `Library` contains
    /// both a CloudStorage subtree and an ordinary one — the built command
    /// includes `-I CloudStorage`, and running the real `du` on the fixture
    /// with that mask reports a smaller `Library` size than without it.
    #[test]
    fn discovery_du_excludes_cloudstorage_mask() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("Library");
        std::fs::create_dir_all(lib.join("CloudStorage")).unwrap();
        std::fs::create_dir_all(lib.join("other")).unwrap();
        std::fs::write(lib.join("CloudStorage").join("big"), vec![0u8; 400 * 1024]).unwrap();
        std::fs::write(lib.join("other").join("small"), vec![0u8; 10 * 1024]).unwrap();
        let lib_str = lib.to_string_lossy().into_owned();

        let args = du_args(&lib_str, DU_EXCLUDE_MASKS);
        assert!(
            args.windows(2).any(|w| w == ["-I", "CloudStorage"]),
            "{args:?}"
        );

        let with_excl = du_kb(&lib_str, DU_EXCLUDE_MASKS).expect("du with mask");
        let without_excl = du_kb(&lib_str, &[]).expect("du without mask");
        assert!(
            with_excl < without_excl,
            "expected CloudStorage-excluded size to be smaller: {with_excl} >= {without_excl}"
        );

        // du_sizes_discovery wires the mask through end-to-end.
        let sizes = du_sizes_discovery(&[lib_str.clone()]);
        assert!(sizes.contains_key(&lib_str));
    }
}
