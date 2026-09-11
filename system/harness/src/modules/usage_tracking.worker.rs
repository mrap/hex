//! `hex-usage-tracking` — bounded local Codex ledger collection.
//!
//! This is an existing-harness worker. It starts no daemon and sends no alerts.
use hex::worker::{ctx::Ctx, event::Event, Result, Worker};
pub const CRON_EVERY_5M: &str = "0 */5 * * * * *";
pub const ARGV_COLLECT: &[&str] = &["hex", "usage", "collect", "--max-records", "1000"];
fn run_collect(_event: Event, ctx: Ctx) -> Result<()> { ctx.run(&ARGV_COLLECT.iter().map(|s| s.to_string()).collect::<Vec<_>>()).map(|_| ()) }
pub fn worker() -> Worker { Worker::new("hex-usage-tracking").on_cron_named("every-5m", CRON_EVERY_5M, run_collect) }
