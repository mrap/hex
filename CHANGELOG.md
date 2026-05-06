# Changelog

All notable changes to hex-foundation will be documented in this file.

## [2026-05-06] — doctor reliability + skip_llm WakeConfig (v0.13.1)

### Fixed
- `system/harness/src/main.rs`: Doctor command switches from `cmd.output()` (buffered) to `cmd.spawn()` with `Stdio::inherit()` — output streams live instead of appearing all-at-once after completion.
- `system/scripts/run-startup-checks.sh`, `run-memory-checks.sh`, `run-landings-workspace-checks.sh`: Stale `CLAUDE_DIR=$HEX_DIR/.claude` path changed to `$HEX_DIR/.hex`. Was causing 5 spurious ERRORs on install paths that follow the `.hex` layout.
- `system/scripts/hex-doctor`: Replace buffered `$()` capture with `tee | tail -n +5` streaming. All PIPESTATUS slots captured so mid-pipeline failures surface explicitly. Combined two EXIT traps into one.
- BOI daemon check in hex-doctor rewritten for LaunchAgent-aware detection (was `pgrep`-based, missed managed processes).

### Added
- `system/harness/src/types.rs`: `WakeConfig.skip_llm` field (`#[serde(default)]` for backwards compat). Allows health-probe agents to exercise wake plumbing without paying for an LLM call.
- `system/harness/src/wake.rs`: When `charter.wake.skip_llm=true`, bypass shift loop and self-assessment phase. Inbox loads, wake-start audit fires, `mark_delivered` runs. Inbox-sourced active queue items drained to prevent `state.json` unbounded growth.
- `system/scripts/health/check-message-roundtrip.sh`: end-to-end validation of skip_llm health-probe wake — sends a message, wakes health-probe agent, verifies mark_delivered, state save, and audit emit.
- `system/scripts/health/check-career-pipeline.sh`: career email pipeline health check — validates draft existence, policy load, and optional dry-run send. Sanitize-clean (env-var paths, example addresses).
- `system/scripts/doctor-checks/boi.sh`: BOI daemon doctor check with LaunchAgent-aware detection.
- `system/scripts/hex-watcher`: minimal tmux BOI status pane (one-shot or `--watch` loop).

## [2026-05-05] — agent performance review + calibration

### Added
- `system/scripts/health/agent-performance-review.py`: per-agent quality/velocity/autonomy scorecard — extracts signals from critic reviews, BOI DB, audit trail, and Mike-pushback messages; composite geometric mean (0.0–1.0); cold-start handling (confidence=low for agents with <5 wakes); outputs markdown scorecard with top/bottom artifacts.
- `system/scripts/health/fleet-scorecard-aggregate.py`: fleet-wide aggregate scorecard — runs agent-performance-review.py for all agents, produces top/bottom 5 performers, biggest movers, Mike-pushback heatmap; sends single coalesced Slack digest to configured Slack channel (no per-agent pings per ergonomics-critic rule).
- `adapter/policy-templates/agent-performance-review-weekly.yaml`: policy template wiring `timer.tick.daily` (Sunday 09:00 ET gate) → `fleet-scorecard-aggregate.py` with 6d rate limit.

## [2026-05-05]

### Added
- `system/scripts/health/check-fleet-pulse.sh`: fleet-pulse watchdog — emits `hex.agent.needs-attention` events for dormant agents; composite liveness score with WARN/ERROR escalation; suppresses when budget-lockout active.
- `system/scripts/health/check-stalled-initiatives.sh`: stalled initiative monitor — detects initiatives with no progress signal in 48h (commit, act trail, KR update), sends drive-or-close directive to owner; anti-spam guard prevents re-fire within 24h.
- `system/scripts/health/check-mike-pending.sh`: Mike-pending board monitor — tier:quiet/digest/direct-ping labels, coalesced per-run alerts, DM fallback to channel if Slack user ID not configured.
- `system/scripts/health/budget-period-reset.py`: budget period auto-reset — rolls cost.current_period.start forward when period expires; 5x runaway safety gate blocks reset and emits ERROR alert instead of silently clearing an out-of-control agent.
- `system/harness/src/wake.rs`: backlog auto-promotion with three safety constraints — proactive_initiatives gate (reactive-only agents never self-assign), per-agent daily wake-budget ceiling at 80% of `charter.budget.usd_per_day`, and a per-wake ceiling of 2 backlog items.
- `adapter/policy-templates/fleet-pulse.yaml`: policy template wiring `timer.tick.1h` → `check-fleet-pulse.sh`.
- `adapter/policy-templates/stalled-initiative-monitor.yaml`: policy template wiring `timer.tick.6h` → `check-stalled-initiatives.sh` with per-initiative rate limiting.
- `adapter/policy-templates/mike-pending-escalator.yaml`: policy template wiring `timer.tick.2h` → `check-mike-pending.sh`.
- `adapter/policy-templates/budget-period-reset.yaml`: policy template wiring `timer.tick.daily` → `budget-period-reset.py`.

## [2026-05-04]

### Changed
- AGENTS.md: Added "Related repos" cross-link section in Quick Start pointing to boi and the local hex workspace, so agents navigating hex-foundation can find the delegation engine and production workspace
- templates/CLAUDE.md: Added Quick Start section with "Related repos" placeholder before the system-managed block
