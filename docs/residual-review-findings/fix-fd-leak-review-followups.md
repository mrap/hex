# Residual review findings: fix/fd-leak-review-followups

Source: ce-code-review run 20260917-115415-d355b21c (mode:agent, plan docs/plans/2026-09-17-1130-fix-fd-leak-review-followups-plan.md), base e068bcb6, reviewed head b52afb7b (rebuilt into d1915d44..4413c713 after apply).

## Residual Review Findings

All four actionable findings (#2 telemetry on teardown overrun, #3 test-before-fix commit order, #5 rlimit Drop guard, #6 stall test with an independent bound) were applied on the branch. No tracker tickets were needed (filed: none, failed: none, no_sink: none).

Report-only items kept for the record:

- [P1] system/harness/src/ops.rs:155 — Cascade: stalled handshake leaks SDK threads and sockets (cross-model, Codex). Settled conflict with KTD1 (fix stays in ops.rs; the pinned iii-sdk is not touched; rejected alternative: a handshake timeout inside the SDK's run_connection). Preference-grade: the bounded join keeps the caller responsive; a persistently stalled engine still leaks one detached thread pair plus socket per call, now loud in stderr and telemetry. Follow-up if it recurs: SDK-side cancellation-aware connect, as a separate release-scale change.
- [P1] system/harness/tests/fd_limits.rs:85 — Stall test closes the peer before verifying cleanup (cross-model, Codex; human-owned advisory). The test proves the caller bound only; it cannot assert fd stability under a persistent stall because that leak is the accepted residual above. The listener now stays open through the assertions.

Settled-decision conflicts from implementation (ce-work): none.
