# Testing standard for hex and every future project

Version 1, 2026-09-14. Applies to hex-foundation, the personal instance, BOI specs, and any new project in any language. Written to be checked by a reviewer, human or agent, without judgment calls. This file is the source of truth (SO S1). The instance copy at `$HEX_DIR/projects/hex-ops/standards/testing-standard.md` mirrors it. Background: `$HEX_DIR/projects/hex-ops/plans/cto-testing-posture-2026-09-14.md` (the pyramid), `$HEX_DIR/projects/hex-ops/audits/test-audit-2026-09-14.md` (the numbers behind these rules).

## 1. Principles

1. Test behavior at the boundary a user or another program touches. Never test that the code does what the code does.
2. Scope tests per task. Run the full suite once at merge, in the lane. Cheap is not free.
3. A bug gets a failing test before it gets a fix. The test names the incident.
4. Every failure is loud. No `#[ignore]` for flakiness, no swallowed exit codes, no filter that matches zero tests and exits 0.
5. Delete tests as readily as you add them. A test that has never failed for a real reason is cost, not coverage.

## 2. What to test, by layer

The pyramid from the posture doc, with ratios and wall-clock budgets. Ratios are shares of all test functions in a repo.

| Layer | What it covers | Share | Per task | Per merge | Nightly |
|---|---|---|---|---|---|
| L0 unit | Pure functions, parsers, state machines, error mapping. In process. Temp dirs allowed, no subprocess, no network | 80 to 85% | `-p <crate> --lib` for touched crates. Budget 3 min warm | Included | Included |
| L1 integration | One module's public contract with real files in a temp dir and fake subprocesses. In process | About 10% | Only the `--test <file>` binaries the task names. Budget 5 min with L0 | Included | Included |
| L2 CLI boundary | Spawn the built binary. Assert argv, stdin, exit code, stdout, stderr, files written. One per subcommand, hook, worker, and hook stdin contract | 5 to 8% | Only when the task changes that surface | Full workspace in the lane. Budget 15 min warm, 25 min cold | Included |
| L3 e2e | Install, upgrade, release cut, doctor in a container or on the host. Scripts | 10 scripts or fewer per repo | Never | At `hex release cut` | Included |
| Nightly | Everything above plus `--run-ignored` model tests, clippy, fmt, flaky tally | | | | Harness `.on_cron` worker. Budget 60 min |

Rules that hold the table together:

- A task-level verification names a crate (`-p`) or a test binary (`--test`). `--workspace` and `--all-targets` appear only in `[contract].verifications`.
- Full-workspace runs go through `system/scripts/test-lane.sh`, which shares one target across worktrees and prints one receipt JSON line. Host runs of `cargo test --workspace` are not evidence.
- macOS-only tests run on the host, serialized, at release cut and when the diff touches signing or install paths. They never move into the container and never get `#[ignore]` to make the container green.
- Anything scheduled is a harness `.on_cron` worker with a row in `events.db`. Never launchd, never a polling loop.
- New integration test file only when the tests need their own process (they spawn the binary with process-wide effects). Otherwise add to an existing file or to `src` unit tests. Each Rust integration file is a separate 165 MB link.

## 3. What not to test

| Anti-pattern | One-line example | What to do instead |
|---|---|---|
| Tautological mock | `mock.expect_send().returning(Ok(())); assert!(send(&mock).is_ok())` | Assert what the caller does with the result: the retry, the log line, the file it writes |
| Grep-shaped verification | `assert!(read_to_string("src/throttle.rs")?.contains("pub fn apply"))` (was `throttle_max_flag.rs:24`) | Call `apply` with an input and assert its output |
| Source existence | `assert!(Path::new("src/throttle.rs").exists())` | Delete it. The compiler already proves it |
| Docs grep | `assert!(docs_text.contains("claude-runs.toml"))` | Delete it, or make it a lint gate at merge, never a test |
| Testing a library | `assert_eq!(serde_json::to_string(&Config::default())?, "{...}")` on a plain derive | Test your own parsing rules, defaults, and error messages |
| Snapshot of implementation | `assert_eq!(worker.cron(), "0 9 * * *")` per worker | Simulate 7 days of clock and assert each worker fires at least once |
| Removal guard past its date | A file whose header says "delete after 2026-09-01" still running on 09-14 | Date every removal guard and delete it on that date |
| Sleep as synchronization | `sleep(500ms); assert!(daemon.is_ready())` | Poll with a deadline, use a channel, or inject a clock |
| Help-text scatter | Three files that each spawn the binary to check one `--flag` in `--help` | One table-driven test that walks the clap command tree |

## 4. Rules for AI-written tests

These apply to Claude Code sessions, BOI workers, and any agent that edits a repo.

Scope

- Per task: `cargo nextest run -p <crate> --lib` plus `--test <file>` for each binary the task names. Never `--workspace` at task level. A BOI spec lint enforces this once the BOI item ships; until then the reviewer checks it.
- Per merge: one lane run, `bash system/scripts/test-lane.sh --no-build`, receipt attached to the PR or the spec verdict.
- Use `--no-tests=fail` (nextest) or the language equivalent, so a filter that matches nothing is a failure, not a pass.

Bug reports (TDD)

- Task 1 writes the failing test. Its `behavior` line cites the incident path (`$HEX_DIR/projects/system-improvement/incidents/<name>`) or the spec ID. Verification: the named test fails.
- Task 2 fixes. Verification: the named test passes, by name, with `1 passed` in the output.
- The failing-test commit lands before the fix commit. `git log` order is the proof.

Flakiness

- Never `#[ignore]` a flaky test. `#[ignore]` is reserved for tests that need a model, the network, or a live keychain, and each carries a reason string.
- A test that fails then passes is a flaky pass. Record it in the PR or spec verdict as `test.flaky <name>`. Three in 7 days moves it to a quarantine override in `.config/nextest.toml` (`retries = 3`) by a spec Mike approves. Quarantined tests still run and still report.
- No `sleep` without a bounded poll. No assertion on wall-clock time.

Write a verification that proves behavior

- Bad: `grep -q 'pub fn apply' system/harness/src/throttle.rs`.
- Bad: `cargo test 2>&1 | tail -5 | grep ok` (the tail's exit code wins).
- Good: `export PATH="/opt/homebrew/bin:$PATH" && cargo nextest run -p hex-harness --test harness_cli_test -E 'test(=consolidate_quick_honors_max)' --no-tests=fail`.
- Good: run the binary with a fixture, then assert exit code and a file it must write: `out=$(hex doctor --hex-dir "$fix"); test $? -eq 1 && grep -q 'file over 256 MB' <<<"$out"`.
- A `grep` on source or docs is allowed only as a lint-class gate in `[contract].verifications`, and only next to a behavioral test for the same change.

Naming and placement

- Test names are behavior sentences (`upgrade_reloads_every_sanctioned_launchd_job`), never task IDs (`red_test_for_Tx8a72zfh`). Task IDs go in a comment.
- Every edit happens in a worktree (SO 7). Test commands run in that worktree; the lane mounts it at `/work`.
- Test-writing phases in a BOI spec run on `claude-sonnet-5` unless the spec states why frontier judgment is needed (SO 3b).

## 5. Definition of done for a change

- [ ] Behavior is tested at the lowest layer that can observe it. A changed subcommand, hook, or worker has an L2 test.
- [ ] Bug fix: failing test committed before the fix; commit message cites the incident or spec.
- [ ] Per-task runs were crate-scoped. No new `--workspace` at task level.
- [ ] One lane receipt attached: `exit_code` 0, `tree_hash` equals the merged tree, `crates_compiled` recorded.
- [ ] No new `#[ignore]`, no new `sleep` without a deadline, no test that reads `src/` or `docs/` text.
- [ ] No new integration test file unless it spawns a process with global effects.
- [ ] Flaky passes during the work are listed in the PR as `test.flaky <name>`.
- [ ] Added or removed a script test: `docs/testing.md` (or the repo's test matrix) updated in the same commit.

## 6. Review checklist

Eight mechanical checks. Each is yes or no.

1. Diff touches `src/`: the diff also touches a test, or the PR names the file and says why not.
2. Every new test asserts on output, exit code, state, or file content. None reads `src/` or `docs/` as text.
3. No new `#[ignore]` without a reason string naming model, network, or keychain.
4. No `sleep` without a bounded poll. No wall-clock assertion.
5. Task verifications name `-p` or `--test`. `--workspace` appears only at contract level.
6. Lane receipt present with `exit_code: 0` and a `tree_hash` that matches the reviewed head.
7. Bug fix: the failing-test commit precedes the fix commit in `git log`.
8. Integration file count unchanged, or the new file spawns a process with global effects. Test names read as behavior sentences.

## 7. Language appendix

Rust

- Runner: `cargo nextest` in the lane image (`tests/lane/Dockerfile`, `rust:1.96`, cargo-chef, nextest). Doctests do not run under nextest, so no behavior lives in doctests.
- Profile: `debug = "line-tables-only"`, `split-debuginfo = "packed"` for dev and test. Same posture on the host `~/.cargo/config.toml` and in the image. Full debuginfo caused an OOM link in the container and 139k `.rcgu.o` files on the host.
- Add `.config/nextest.toml`: `retries = 1`, `slow-timeout = { period = "60s", terminate-after = 3 }`, `--no-tests=fail` default. Quarantine overrides live here.
- Spawn the binary with `env!("CARGO_BIN_EXE_hex")` and a temp `HEX_DIR`. Never touch the real `~/.hex`, `~/.boi`, or keychain.
- `HEX_DIR` is sandboxed for every process cargo or nextest launches: `.cargo/config.toml` forces it to `/tmp/hex-test-hex-dir`, so a test that forgets to isolate cannot reach a live store (`tests/telemetry_store.rs` proves the sandbox is active). To point a cargo-built binary at a live instance, run `target/debug/hex` directly with `HEX_DIR` set.
- What the lane gives you: shared target at `/target`, 0 crates compiled on a no-change rerun (27 s), a receipt JSON line, and freedom from Gatekeeper walks.

Python

- Runner: `pytest`. Markers: `unit`, `integration`, `macos`, `live`. Default invocation `pytest -q -m "not live and not macos" --timeout=60`.
- Mock only at the process boundary: `subprocess`, network, keychain, clock. Never mock your own functions.
- Per task: the test files the task touches. Per merge: the default invocation. `macos` runs on the host at release cut. `live` runs nightly with a key.

TypeScript

- Runner: `vitest` for L0 and L1, `playwright` for L2 and L3. `vitest run <path>` per task; `vitest run` plus `playwright test` in a container at merge.
- Mock `fetch` at the boundary (`msw`), never module internals. No `jsdom` unless the code touches the DOM.
- Playwright tests assert visible state and network calls, never CSS selectors that encode layout.

Shell

- Anything with a branch gets a `bats` test. A `.sh` test uses `set -euo pipefail`, prints PASS and FAIL counts, and exits non-zero on any FAIL. The exit code is the verdict.

## 8. Metrics and review cadence

The five metrics from the posture doc, section 6, are the health signal for this standard:

| Metric | Target | Source |
|---|---|---|
| Compile minutes per task | Under 3 min warm | Lane receipt `duration_secs` split, BOI receipt `build_secs` |
| Test minutes per task | Under 3 min for L0 plus L1 | Receipt `run_secs`, JUnit totals |
| Cache hit rate (crates `Fresh` over total) | Above 90% at L0 and L1; near 100% on a no-change rerun | Receipt `crates_compiled` |
| Time to red on a planted regression | Under 15 min dispatch to red | Weekly canary spec |
| Target volume size and cardinality | Under 40 GB; `deps` entries under 20,000; zero `.rcgu.o` | `hex-build-cache-guard` worker, `docker system df -v` |

Two shape metrics from the audit, checked at review: integration binaries with 3 or fewer tests (target 0; today 9), and tests that read `src/` or `docs/` text (target 0; today 8).

Review cadence: monthly, first Monday, by the CTO session. Output is a new `$HEX_DIR/projects/hex-ops/audits/test-audit-YYYY-MM-DD.md` with the same scorecard table and a diff against the previous month. Any metric outside target for two consecutive reviews becomes a BOI spec, not a note.
