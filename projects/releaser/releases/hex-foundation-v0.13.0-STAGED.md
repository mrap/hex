# hex-foundation v0.13.0 — RELEASED

**Released:** 2026-05-06
**SHA range:** bcdcd8f..fda88cb (origin/main..HEAD)
**Commits:** 10
**Security gate:** SA-026 PASS (Sentinel, 2026-05-06)

## What's in this release

### Fleet self-driving mechanisms
- **Fleet pulse watchdog** (`system/scripts/health/check-fleet-pulse.sh`): emits `hex.agent.needs-attention` for dormant agents. Composite liveness score, WARN/ERROR tiers, budget-lockout suppression.
- **Stalled initiative monitor** (`system/scripts/health/check-stalled-initiatives.sh`): detects initiatives with no progress signal in 48h (commit, act trail, KR update). Sends drive-or-close directives. 24h anti-spam guard.
- **Mike-pending board monitor** (`system/scripts/health/check-mike-pending.sh`): tier-labeled (quiet/digest/direct-ping) coalesced alerts. DM fallback if Slack user ID not configured.
- **Budget period auto-reset** (`system/scripts/health/budget-period-reset.py`): rolls cost.current_period.start forward on expiry. 5x runaway safety gate emits ERROR alert instead of resetting out-of-control agent.
- **Backlog auto-promotion** (`system/harness/src/wake.rs`): three safety constraints — `proactive_initiatives` gate, 80% daily budget ceiling, 2 items/wake limit.

### Agent performance review
- **Per-agent scorecard** (`system/scripts/health/agent-performance-review.py`): quality/velocity/autonomy from critic reviews, BOI DB, audit trail, Mike-pushback signals. Geometric mean (0.0–1.0). Cold-start handling (<5 wakes → confidence=low).
- **Fleet aggregate** (`system/scripts/health/fleet-scorecard-aggregate.py`): top/bottom 5 performers, biggest movers, Mike-pushback heatmap. Single coalesced digest (no per-agent pings per ergonomics rule).

### Policy templates (5 new)
- `adapter/policy-templates/fleet-pulse.yaml` — wires `timer.tick.1h` → check-fleet-pulse.sh
- `adapter/policy-templates/stalled-initiative-monitor.yaml` — wires `timer.tick.6h` → check-stalled-initiatives.sh
- `adapter/policy-templates/mike-pending-escalator.yaml` — wires `timer.tick.2h` → check-mike-pending.sh
- `adapter/policy-templates/budget-period-reset.yaml` — wires `timer.tick.daily` → budget-period-reset.py
- `adapter/policy-templates/agent-performance-review-weekly.yaml` — wires `timer.tick.daily` (Sunday gate) → fleet-scorecard-aggregate.py

### Misc
- `system/events/hex_eventd.py`: re-apply duration field to action_result (sc2b5 fix)

## Files changed from origin/main
- CHANGELOG.md
- README.md
- adapter/policy-templates/agent-performance-review-weekly.yaml (new)
- adapter/policy-templates/budget-period-reset.yaml (new)
- adapter/policy-templates/fleet-pulse.yaml (new)
- adapter/policy-templates/mike-pending-escalator.yaml (new)
- adapter/policy-templates/stalled-initiative-monitor.yaml (new)
- system/events/hex_eventd.py (modified, 4 lines)
- system/harness/src/wake.rs (modified, +249/-15)
- system/scripts/health/agent-performance-review.py (new, 911 lines)
- system/scripts/health/backlog-promote.py (new, 289 lines)
- system/scripts/health/budget-period-reset.py (new, 319 lines)
- system/scripts/health/check-fleet-pulse.sh (new, 337 lines)
- system/scripts/health/check-mike-pending.sh (new, 312 lines)
- system/scripts/health/check-stalled-initiatives.sh (new, 402 lines)
- system/scripts/health/fleet-scorecard-aggregate.py (new, 463 lines)

## Security-relevant changes
- `system/harness/src/wake.rs`: +249/-15 lines. New backlog auto-promotion logic with budget ceiling enforcement. New safety gates (proactive_initiatives check, per-wake item ceiling).
- 7 new shell scripts and Python scripts in system/scripts/health/ — run as agent actions, invoke `hex agent message`, `boi` CLI.
- Policy templates in adapter/policy-templates/ wire timer events to health scripts.
