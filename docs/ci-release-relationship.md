# CI ↔ release relationship (`core-e2e`)

**Status:** `core-e2e` is a **clean-room / PR signal**, NOT a required status check and
NOT the release gate. Written 2026-06-13.

## Why CI is not the release gate

The releaser (`hex release cut`) runs the **same** E2E suites locally as pre-push gates
before any `main` push. So requiring them again on the remote is partly redundant — and it
actively *broke releases twice*:

- The releaser direct-pushes `main` (it owns the only git path that can, via a git-guard).
  A PR-style **required status check** can never be satisfied by a direct push, so a required
  `core-e2e` check **blocks every release** regardless of test health.
- `core-e2e` was also failing ~60% of runs for environmental reasons (cold-build flakiness,
  100-min hangs), so even on PRs it was an unreliable gate.

Ruleset `15708904` ("Protect core branches") briefly required `Hex core suites` + `BOI
integration suites`. It blocked the credit-burn deploy and the v0.42.0 cut (2026-06-12) and
was removed the same day. **Do not re-add it as a required check until `core-e2e` is reliably
green + fast** (3 consecutive green runs, < 15 min). If you ever do, you MUST pair it with a
**bypass actor** for the releaser's identity so `hex release cut` can still push `main`.

> The stray comment in `core-e2e.yml` ("Combine with a required status check on tag
> protection rules") that kept nudging people to wire this up has been **removed** — this doc
> is the canonical word on the relationship.

## What CI *is* for

- **Pre-merge PR signal** — clean-room reproduction of the suites on a stock `ubuntu-latest`
  runner, catching "works on my machine" drift the local pre-push gate can miss.
- **Post-merge `main` / tag signal** — informational confirmation after a release lands.

It is allowed to be red without blocking a release; treat a red `core-e2e` as a bug to fix,
not a gate that stops shipping.

## Speed design (2026-06-13)

The two image-building jobs (`hex-core`, `harness-lifecycle`) compile the `hex-harness` crate,
whose cold build is ~8-11 min — dominated by the iii engine git deps (`mrap/hex-iii`) and
`rusqlite`'s `bundled-full` (SQLite compiled from C). That cold compile was the single biggest
time sink and ran on *every* job of *every* run.

**Fix:** cargo-chef splits dependency compilation into its own Docker layer keyed on
`recipe.json` (≈ `Cargo.lock` + the `scipd` path-dep manifest). `docker/build-push-action`
with `cache-from/to: type=gha,mode=max` persists that layer across CI runs. On a run where the
dependency graph is unchanged, the dep layer is restored from cache and only the changed hex
source recompiles (~1-2 min) instead of the full cold build.

- `mode=max` is required — the cooked-deps layer lives in an intermediate build stage, which
  only `mode=max` exports.
- Per-job `scope=` (`core-e2e` vs `harness-e2e`) keeps the release and debug dep caches
  separate.
- **No `--mount=type=cache`** in the cook/build steps: the gha backend persists *layers*, not
  cache mounts, so the registry + `target/` must land in real layers to survive ephemeral
  runners. (The old `harness-e2e` cache mounts only ever helped local rebuilds — they were a
  no-op on CI, which is why CI stayed slow.)
- The `scipd` path dep (`../code-intel`, a sibling outside `/build`) can NOT be reconstructed
  by cargo-chef from the recipe, so the real crate is copied **before** cook. The cook layer
  therefore keys on `(recipe.json + code-intel source)`; editing the harness source — the
  common case — leaves the warm cache intact, and `code-intel` changes rarely.

## Hang protection

Every job has `timeout-minutes` (hex-core 25, harness-lifecycle 25, boi-integration 60).
Before this, jobs with no timeout hung 50-100 min on a stuck step instead of failing fast.

## BOI integration: off the per-PR path (2026-06-13)

Jobs in this workflow run in parallel with no `needs:`, so a *run*'s wall-clock = the slowest
job. `boi-integration` (~50 min, host Docker, needs `ANTHROPIC_API_KEY`) is uncacheable in the
same way and would pin every run at ~50 min — making the < 15-min target unreachable for PRs no
matter how fast the image jobs get.

**Resolution:** `boi-integration` is gated `if: github.event_name != 'pull_request'`. It runs
on push-to-`main`/tags, the **nightly schedule** (`cron: '0 8 * * *'`), and manual
`workflow_dispatch` — never on PRs. So the per-PR path is just the two cached image jobs
(`hex-core`, `harness-lifecycle`), which is what lets a PR run finish in < 15 min. BOI
regressions are still caught nightly and on every merge to `main`.

Tradeoff accepted: a PR that breaks the BOI integration suites won't show red until the nightly
run or the post-merge `main` run. Given BOI is read-only from hex and the BOI suites mostly
exercise the upgrade path (rarely regressed by a hex PR), nightly coverage is sufficient.

> **The < 15-min acceptance target applies to the per-PR path** (the two cached image jobs). A
> full run that includes `boi-integration` (nightly / post-merge) is expected to take ~50 min.

## Validation (2026-06-13, PR #15)

Three consecutive green per-PR runs on `fix/ci-e2e-cache`, all < 15 min — the full
cache-state matrix:

| Run | Build context vs cache | `hex-core` wall | What it proves |
|-----|------------------------|-----------------|----------------|
| 1 (cold) | nothing cached | **14m49s** | full cold build populates the gha cache; under target |
| 2 (+source edit) | dep/cook layer warm, source changed | **7m49s** | the common case — editing harness source reuses the cook layer, hits the < 10-min stretch |
| 3 (identical context) | every layer warm | **~19s** | byte-identical context → full image-layer hit; 34/34 suites still execute against the cached image |

Local correctness alongside: core-e2e chef image 34/34 suites, harness-e2e lifecycle 8/8.

## Operating risk: gha cache eviction

The gha cache backend has a **10 GB per-repo limit**. Two `mode=max` scopes
(`core-e2e` + `harness-e2e`) each persist a full `target/`, so the two scopes together can
approach that ceiling. If the cache is evicted (size pressure, or 7-day inactivity), the next
run falls back to a **cold ~15-min build** — a thin-margin breach of the < 15-min target, not a
failure. Steady-state warm runs (~8 min, run 2) are the norm; this only bites right after an
eviction.

**Mitigation if it recurs:** drop the `harness-e2e` cache scope to `mode=min` (it exports only
the final image layer, not the intermediate cook layer — halving that scope's footprint at the
cost of a slower harness-job warm rebuild). `core-e2e` keeps `mode=max` since it's the
acceptance-gating job. Watch the cache list (`gh cache list --repo mrap/hex`) if cold runs start
appearing intermittently.
