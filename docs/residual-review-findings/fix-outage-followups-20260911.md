# Residual review findings: fix/outage-followups-20260911

Source: ce-code-review run `20260911-175930-e15a1b2f` (5 reviewers: correctness, project-standards, testing, reliability, adversarial; all on Sonnet; no cross-model peer). Plan: `docs/plans/2026-09-11-1612-fix-harness-outage-followups-plan.md` (mrap-hex instance repo).

Applied in commit `fix(review): ...` on this branch: storm window filter (#2), storm alert key hash suffix (#3), installer journal cleared after a clean rollback (#6), self-check exec-failure tests (#5), fresh du sizes in floor alert (#11), fresh mtime in index loop (#10).

Accepted, not applied:

- #4 P1 `system/harness/src/resources.rs` `du_kb`: `du` runs with no subprocess timeout, so a hung `du` can stall the hourly resource sample. Pre-existing shape carried into the new helper. Follow-up: spawn with a deadline and kill on timeout, returning `None`.
- #7 P1 `system/scripts/macos-app-install.py:1296`: reviewer proposes running the self-check against the staged candidate before the atomic swap. Pushed back: the 2026-09-10 outage was a broken published symlink, which a pre-swap check cannot see. The post-publish check plus rollback stays (KTD1). A pre-swap smoke test could be added in addition.
- #8 P2 `system/harness/src/main.rs:3151`: `hex resources status` does not show the new top-3 reclaimable-directory hint that the alert path shows. Follow-up: share `floor_message` with `format_breach`.
- Advisory: `signature_head` digit collapse can merge unrelated errors into one storm; escalation ignores battery state; `du -I CloudStorage` masks any directory of that name.
