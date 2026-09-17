# Residual review findings: fix/fd-leak-review-followups

Source: ce-code-review run 20260917-115415-d355b21c (mode:agent, plan docs/plans/2026-09-17-1130-fix-fd-leak-review-followups-plan.md), base e068bcb6, reviewed head b52afb7b (rebuilt into d1915d44..4413c713 after apply).

## Residual Review Findings

All four actionable findings (#2 telemetry on teardown overrun, #3 test-before-fix commit order, #5 rlimit Drop guard, #6 stall test with an independent bound) were applied on the branch. No tracker tickets were needed (filed: none, failed: none, no_sink: none).

Report-only items kept for the record:

- [P1] system/harness/src/ops.rs:155 — Cascade: stalled handshake leaks SDK threads and sockets (cross-model, Codex). Settled conflict with KTD1 (fix stays in ops.rs; the pinned iii-sdk is not touched; rejected alternative: a handshake timeout inside the SDK's run_connection). Preference-grade: the bounded join keeps the caller responsive; a persistently stalled engine still leaks one detached thread pair plus socket per call, now loud in stderr and telemetry. Follow-up if it recurs: SDK-side cancellation-aware connect, as a separate release-scale change.
- [P1] system/harness/tests/fd_limits.rs:85 — Stall test closes the peer before verifying cleanup (cross-model, Codex; human-owned advisory). The test proves the caller bound only; it cannot assert fd stability under a persistent stall because that leak is the accepted residual above. The listener now stays open through the assertions.

Settled-decision conflicts from implementation (ce-work): none.

## 2026-09-17 update: serve-side residual closed (U6, KTD8)

`docs/plans/2026-09-17-1412-fix-known-open-harness-failures-plan.md` unit U6 closes the [P1] `ops.rs:155` finding above for the `hex harness serve` path specifically: `serve` now installs its one long-lived `iii_sdk::III` client into a process-wide `OnceLock` (`ops::install_shared_client`, called from `worker::runtime::connect_engine_client`) and `call_builtin_with_timeout_and_budget` reuses it — no `register_worker`, no per-call `shutdown()` — when present. That removes the wasted-connection-per-call cost inside `serve`, including the stalled-handshake shape: a call against a stalled shared client fails on its own trigger timeout without ever running a per-call `shutdown_within` that could overrun its budget.

Two parts of the original finding stay open, as scoped by KTD8:

- The CLI subprocess path (`hex` invoked as a one-shot process, no `serve` running) still opens one client per call and relies on the OS reclaiming it at process exit rather than an in-process `shutdown()` loop — unchanged by this unit, and not a leak in that shape since the process is short-lived.
- The underlying SDK handshake-timeout gap (`iii_sdk::register_worker`'s `connect_async` has no timeout, so a per-call client against a truly stalled handshake still depends on the bounded-join workaround rather than the SDK failing fast) remains a fork-level follow-up, rejected for this PR by KTD8 (fork change plus lockstep bump, deferred).
