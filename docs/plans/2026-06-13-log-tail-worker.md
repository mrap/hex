# Plan: Reusable event-driven log-tail worker (`hex log-tail`)

**Date:** 2026-06-13
**Branch:** `feature/log-tail-worker` (off `develop`, GitFlow)
**Spec written against:** hex-harness `0.43.0`, foundation `develop` @ `3659a064`

## Goal

Ship a **generic, event-driven log-tail capability** in hex-foundation whose first consumer is capturing headroom proxy overhead (`STAGE_TIMINGS.total_pre_upstream`) into hex's unified telemetry + event bus — headroom treated as an opaque, swappable upstream. The reusable core is a `LineObserver` trait; headroom is one compiled impl. New upstreams = new impl, never a config DSL.

Origin: `me/decisions/headroom-overhead-telemetry-architecture-2026-06-13.md` (mrap-hex) + `docs/ideation/2026-06-13-headroom-overhead-telemetry-ideation.md`.

## Key design decision (forced by SDK reality)

The Rust worker SDK (`Worker::on_cron/on_event`) runs handlers on `spawn_blocking` — **handlers must return; no daemon/continuous-loop primitive exists.** A continuous, event-driven `tail -F` therefore cannot be a `*.worker.rs` cron handler without degrading to a poll (the thing we explicitly rejected). The hex-idiomatic continuous mechanism is the **`iii-exec` supervised daemon** — the exact pattern that runs the headroom proxy and console (`engine-workers.yaml`, `restart.on_crash: true`).

**Therefore:** implement `hex log-tail` as a long-running CLI subcommand (event-driven via `notify`/FSEvents — blocks on kernel FS events, no timer), supervised by iii-exec. This honors no-poll + reuses hex's supervised-daemon worker pattern. The "reusable worker" is the iii-exec stanza running `hex log-tail --observer <name>`.

**Testability discipline:** the brain is pure and unit-tested; the notify loop is thin glue.
- `LineObserver` trait + impls — pure, unit-tested.
- Offset/rotation **tail reader** (given file + prior offset → new lines, new offset, rotation flag) — pure, unit-tested with temp files (deterministic, no FS-event flakiness).
- Episode **coalescer** (state machine: burst of breaches → one episode) — pure, unit-tested.
- notify follow-loop — thin glue; exercised by a bounded integration test, not relied on for FS-event timing in CI.

## Architecture / files

New module `system/harness/src/log_tail/`:
- `mod.rs` — module exports + the `run()` entrypoint (follow loop driver) + `LogTailConfig`.
- `observer.rs` — `LineObserver` trait, `Observation { value_ms, stage, severity, fields }`, `Severity { Normal, Degraded, Stall }`, and `observer_registry(name) -> Option<Box<dyn LineObserver>>`.
- `reader.rs` — `TailReader`: open-at-offset, read new complete lines, detect rotation/truncation (len < offset OR inode change → restart at 0), return `(Vec<String>, new_offset)`. Pure, no notify.
- `coalesce.rs` — `EpisodeCoalescer`: feed `(ts, Observation)`; emits a `StallEpisode { duration_ms, count, peak_ms, dominant_stage }` when a burst closes (gap > `quiet_window`). Pure state machine.
- `emit.rs` — durable-first sink: `telemetry::record(...)` FIRST (durable SQLite), then `ops::emit(event, data, producer)` best-effort (lossy bus). Never lose the row to a bus drop.
- `observers/headroom.rs` — `HeadroomStageTimings: LineObserver`. Matches lines containing `STAGE_TIMINGS`, parses the embedded JSON, extracts `stages.total_pre_upstream` (+ `read_request_json`, `deep_copy`), classifies severity by thresholds, returns `Observation`.

Wiring:
- `lib.rs` — add `pub mod log_tail;`.
- `main.rs` — add `Commands::LogTail { path, observer, event, source, threshold_ms, quiet_ms, from_start }` (flat args) + a dispatch arm calling `log_tail::run(cfg)`.
- `Cargo.toml` — add `notify = "8"` (already in Cargo.lock @ 8.2.0 transitively → no new download).
- `system/iii/engine-workers.example.yaml` — add a commented `log-tail-headroom` iii-exec stanza documenting how to enable (instance enables in its own `engine-workers.yaml`).

Fixture + contract:
- `system/harness/tests/fixtures/headroom-stage-timings.jsonl` — a golden real `STAGE_TIMINGS` line.
- Contract test (inline in `observers/headroom.rs` tests) asserts the parser extracts `total_pre_upstream` from the golden line — fails loud if headroom's format drifts (the "telemetry dies silently" guard, idea #6).

## Event + telemetry contract

- Telemetry row (durable-first): `source="headroom-proxy"` (or `--source`), `event="overhead"` / `"stall_episode"`, `status` ∈ {`ok`,`degraded`,`stall`}, `duration_ms = total_pre_upstream_ms` (peak for episodes), `detail = compact JSON { stage, count, ... }`.
- Bus event (best-effort): name from `--event` (default `headroom.overhead`), `data = { severity, value_ms, stage, count, peak_ms, source }`. Consumers register `on_event`.
- Producer label: `--source` value, so swapping/dual-running upstreams stays attributable (producer_id baked in).

## Severity thresholds (headroom)

Normal < 500ms ≤ Degraded < 2000ms ≤ Stall. Only Degraded/Stall emit per-event; Normal feeds episode stats only. Defaults overridable via `--threshold-ms` (degraded) — keep flags minimal; no config DSL.

## Testing strategy / CI gates

CI: `cargo test --manifest-path system/harness/Cargo.toml` (canonical, per AGENTS.md) + release build in Docker. Local gates before push: `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` (run all in the worktree).

Unit tests (pure, deterministic):
- `reader`: appends advance offset; rotation (truncate/shrink) restarts at 0; partial trailing line not emitted until newline.
- `coalesce`: single breach → episode of 1; burst within window → one episode with correct count/peak; gap closes episode.
- `observer/headroom`: parses golden line; classifies Normal/Degraded/Stall; ignores non-STAGE_TIMINGS lines; tolerates malformed JSON (no panic).
- `registry`: known name → Some, unknown → None.
- contract: golden fixture parses to expected total_pre_upstream.

## STOP / risk conditions

- If `notify` cannot be added cleanly (version conflict with the iii engine's transitive copy) → fall back to a `std` + short-interval read loop **inside the daemon** (still a continuous supervised process, latency bounded; document the deviation). Do NOT convert to a cron `*.worker.rs` poll.
- If `main.rs` wiring risks breaking the large central file → keep the variant + arm minimal; all logic in `log_tail/`.
- Code ≠ this plan's description of the SDK → re-verify against live code, never improvise.

## Out of scope (YAGNI — recorded, not built)

- Non-file sources (stream/pipe/socket) — trait stays source-agnostic; ship file only.
- `/metrics` or `/stats-history` taps (polling; rejected).
- SPC adaptive baseline + self-closing decision-file loop (idea #7) — strong follow-up, separate change once the base signal flows.
- Cost-correlation, back-pressure, generic "observe-any-upstream" config language.
