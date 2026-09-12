# Residual review findings: fix/outage-followups-20260911

Source: ce-code-review run `20260911-175930-e15a1b2f` (5 reviewers: correctness, project-standards, testing, reliability, adversarial; all on Sonnet; no cross-model peer). Plan: `docs/plans/2026-09-11-1612-fix-harness-outage-followups-plan.md` (instance repo).

Applied in commit `fix(review): ...` on this branch: storm window filter (#2), storm alert key hash suffix (#3), installer journal cleared after a clean rollback (#6), self-check exec-failure tests (#5), fresh du sizes in floor alert (#11), fresh mtime in index loop (#10).

Accepted, not applied:

- #4 P1 `system/harness/src/resources.rs` `du_kb`: `du` runs with no subprocess timeout, so a hung `du` can stall the hourly resource sample. Pre-existing shape carried into the new helper. Follow-up: spawn with a deadline and kill on timeout, returning `None`.
- #7 P1 `system/scripts/macos-app-install.py:1296`: reviewer proposes running the self-check against the staged candidate before the atomic swap. Pushed back: the 2026-09-10 outage was a broken published symlink, which a pre-swap check cannot see. The post-publish check plus rollback stays (KTD1). A pre-swap smoke test could be added in addition.
- #8 P2 `system/harness/src/main.rs:3151`: `hex resources status` does not show the new top-3 reclaimable-directory hint that the alert path shows. Follow-up: share `floor_message` with `format_breach`.
- Advisory: `signature_head` digit collapse can merge unrelated errors into one storm; escalation ignores battery state; `du -I CloudStorage` masks any directory of that name.

## Round 2 (2026-09-12, run `20260912-0950-modules`, correctness + adversarial on Sonnet)

- Pushed back: "WATCH_LIST trend filter does not exclude ~/worktrees." `~/worktrees` is a watch-list entry; its 108 GB growth in 72 h is real (release-campaign worktrees with 4 GB target dirs each), so the alert is a true positive.
- FYI (anchor 50): run the self-check against the staged candidate before the symlink swap. Declined for the same reason as round 1: the 2026-09-10 failure was the published symlink itself.
- Residual: `module verify` compares file names, not content; an edited worker with the same name still passes. Follow-up: hash the worker sources into the registry.

## Round 3 (2026-09-12, operational)

- `hex upgrade --local` on 2026-09-12 10:52: the code-intel signed-app install booted out `com.hex.scipd`, published the new plist, and `launchctl bootstrap` returned `Bootstrap failed: 5: Input/output error` because the old daemon had not finished exiting. A manual bootstrap of the same plist 30 s later succeeded. Follow-up: retry bootstrap with a short backoff when the exit is 5, or wait for the booted-out pid to exit before bootstrapping.

## Round 4 (2026-09-12, release ceremony)

- `hex release cut` pins `refs/heads/develop` (the local branch) without fetching or checking it against `origin/develop`. After the first attempt left the checkout detached, later `git pull --ff-only` calls advanced HEAD, not the branch, so three further attempts tested a stale develop and blamed the parity gate. Follow-up: fetch and require local develop == origin/develop before pinning, or pin `origin/develop`.
- The Claude Code harness kills its own background shells under system memory pressure; the docker-e2e gate plus a full-priority index run reached that threshold twice. The detached `release.requested` path is the right way to run a cut from a session.
