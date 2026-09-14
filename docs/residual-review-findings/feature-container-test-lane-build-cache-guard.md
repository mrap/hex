# Residual review findings: feature/container-test-lane-build-cache-guard

Source: code review of the branch diff against `develop` `0fe9bfa0` on 2026-09-14, plan `docs/plans/2026-09-14-1546-feat-container-test-lane-build-cache-guard-plan.md`. Verdict was ship. Applied: control characters are now stripped from the receipt `command` string in `system/scripts/test-lane.sh`. Left open, no tracker sink configured:

- low, `system/harness/src/modules/build_cache_guard.worker.rs` `prune_and_count` and `read_cargo_target_dir`: seven near-identical `anyhow!` wrappers. A small `io_ctx(op, path, err)` helper would cut repetition.
- low, `system/harness/src/modules/build_cache_guard.worker.rs` `prune_and_count`: the second `read_dir().count()` pass could be an incremental count in the first loop. Cosmetic.
- low, observed while running the lane: `scipd query::tests::callers_of_double` failed once in five lane runs (passed in the other four and on the host). Pre-existing flake under parallel nextest; the days 61 to 90 `nextest.toml` retries and quarantine item covers it.
