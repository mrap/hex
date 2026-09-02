# Recall Fix Package — 2026-08-31

Implements the approved fix package from the verified recall-plateau diagnosis
(`projects/system-improvement/diagnoses/recall-plateau-2026-08-31.md`). Spec
`Swqqg9f81`, written against develop `91a0e730`. One section per task: what
changed, file:line anchors, test names added, and any deviations from scope.

---

## T28958xxp — Object-aware, case-insensitive fact dedup key (assemble.rs)

### Cause addressed
Diagnosis cause 1: `facts_to_candidates` built the fact dedup key from ONLY
subject and predicate (object ignored, case-sensitive), and the merge's single
shared seen-set spans both the floor loop and the round-robin loop — so at most
ONE fact per `(subject, predicate)` pair could ever enter assembled context.
246 of 1600 live `(subject, predicate)` groups hold 2+ facts (Mike+decided x152,
Mike+works-on x115). Counterfactual-proven evictions: `ctl-mike` and
`a-zwerk-sse-endpoint`.

### What changed
- Base key at develop `91a0e730` was `format!("fact:{}|{}", f.subject,
  f.predicate)` — object-blind and case-sensitive.
- New key folds subject + predicate to lowercase (collapses true case-variant
  duplicates) and appends a per-object fingerprint (keeps genuinely distinct
  facts sharing a subject+predicate apart, so they no longer evict each other):
  `format!("fact:{}|{}|{:016x}", subject.to_lowercase(),
  predicate.to_lowercase(), fact_object_fingerprint(object))`.
- The object is fingerprinted VERBATIM (not case-folded). Object-similarity
  collapsing is deliberately left to the conservative canonicalization pass
  (task `Tsfwg7d2v`), not this key — so this key change stays minimal and only
  collapses exact case-variant duplicates.

### File:line anchors (`system/harness/src/memory/assemble.rs`)
- `fact_object_fingerprint(object: &str) -> u64` helper — lines 415-427
  (doc comment 415-421, fn body 422-427). Uses `DefaultHasher`; dedup keys are
  only compared within a single `assemble()` call, so cross-run hash stability
  is not load-bearing.
- Object-aware, case-insensitive `dedup_key` construction inside
  `facts_to_candidates` — lines 439-456.

### Tests added (`system/harness/src/memory/assemble.rs`, `mod tests`)
- `dedup_two_facts_same_pair_different_objects_both_render` (line ~1112) —
  the `eviction-fixed` proof: two facts with identical subject and predicate but
  different objects both survive the end-to-end merge (via `assemble()`), where
  pre-fix only one did. Asserts an M3 retrieval precondition
  (`candidate_count == 2`) so a failure is a dedup eviction, not a retrieval
  shortfall. Also asserts both distinct objects appear in the rendered
  `render_candidates(&r)` output — the char budget is applied while candidates
  are built (assemble.rs ~632/679), so that string is the real assembled
  context an agent sees, matching the `eviction-fixed` verification's literal
  "assembled output" wording.
- `dedup_case_variant_true_duplicates_collapse` (line ~1189) — two facts that
  differ only by case in subject/predicate (identical object) share ONE dedup
  key so the seen-set collapses the second. Asserts key EQUALITY only, never the
  key format.
- `dedup_distinct_objects_keep_separate_keys` (line ~1227) — the distinctness
  half: same subject+predicate, different objects keep SEPARATE keys
  (`assert_ne!`), preventing an implementation that satisfies the collapse test
  by case-folding alone while leaving the eviction bug intact.

### Backward-compatibility / default-behavior note
This task intentionally changes default-path behavior (more distinct
same-pair facts now survive the merge). The named legacy pins
`default_config_reproduces_legacy_facts_recall_exactly` and
`default_config_vector_arm_off_is_byte_identical`
(`system/harness/src/memory/recall.rs:934` and `:1004`) operate at the
`facts_recall` / `facts_recall_with_config` RETRIEVAL layer and do NOT route
through `facts_to_candidates` (the merge/dedup layer this task edits), so they
remain valid and unmodified. No legacy pin required updating for this task; the
new default-path behavior is pinned directly by the three dedup tests above.

### Deviations from scope
None to the code, tests, or doc. One operational note on verification below.

### Operational note — verifying `dedup-tests-pass` under shared-target contention
The declared `dedup-tests-pass` verification
(`cargo test --release dedup` in `system/harness`) inherits the BOI harness
env var `CARGO_TARGET_DIR=/Users/mrap/.boi/v2/cargo-target` — a SHARED release
target dir used concurrently by every active BOI worktree. Because sibling
worktrees carry different source edits, their cargo fingerprints differ and
thrash each other's cached artifacts, and the plain command spends its whole
budget `Blocking waiting for file lock on build directory` rather than
compiling. This is the root cause of the five prior execute-phase wall-clock
timeouts on this task (the code has been complete and correct on disk since
wip commit `3a13dfde`), NOT any code defect.

To obtain a result off the contended shared lock, this session runs the same
`dedup` tests in a private, contention-free per-task target dir created by an
APFS copy-on-write clone of the warm shared target
(`cp -cR /Users/mrap/.boi/v2/cargo-target/release /tmp/recallfix-t1-target/release`,
then `CARGO_TARGET_DIR=/tmp/recallfix-t1-target`). The clone is near-instant
(copy-on-write, no block duplication) and — verified this session — preserves
every dependency's cargo fingerprint: a `cargo test --release --lib dedup`
against it reports exactly ONE `Compiling` line (`hex-harness`), i.e. it reuses
all 4253 warm dependency rlibs and rebuilds only the one edited crate. LTO is
kept ON (the faithful `--release` profile is unchanged), and `--lib` scopes to
the lib target's unit tests where all three dedup tests live
(`assemble.rs`'s `#[cfg(test)] mod tests`); no `tests/*.rs` integration binary
matches the name `dedup`, so `--lib` runs the identical test SET as the bare
declared command and only skips compiling unrelated integration binaries. No
profile file on disk is changed and no artifact is shipped; only the
build-artifact location differs from the declared shared-dir command, not the
compiled test logic.

The sole remaining cost on the clone is the fat-LTO link of the lib unit-test
binary (`lto = true` at the workspace-root `Cargo.toml:6`, which overrides the
ignored per-package profile in `system/harness/Cargo.toml`). That single link
is genuinely slow — observed to exceed 10 minutes even with all dependencies
warm and CPU freed — and is the true root cause of the repeated execute-phase
wall-clock timeouts on this task, together with the shared-lock contention
above. The code and the three dedup tests have been complete and correct on
disk since wip commit `3a13dfde`; the two deterministic key tests
(`dedup_case_variant_true_duplicates_collapse`,
`dedup_distinct_objects_keep_separate_keys`) call `facts_to_candidates`
directly and fully prove the key semantics, and
`dedup_two_facts_same_pair_different_objects_both_render` proves eviction is
fixed end-to-end via `assemble()`.

OBSERVED RESULT (this session, genuine green). To obtain a real exit code off
the fat-LTO wall, the three `dedup` tests were run under the DEBUG profile in a
private, contention-free target dir CoW-cloned from the warm shared debug tree
(`cp -cR /Users/mrap/.boi/v2/cargo-target/debug /tmp/t1-dbg-target/debug`, then
`CARGO_TARGET_DIR=/tmp/t1-dbg-target cargo test --lib dedup`). Exact command and
result observed this session:
`CARGO_TARGET_DIR=/tmp/t1-dbg-target cargo test --lib dedup` → **exit 0**,
`test result: ok. 10 passed; 0 failed` (log `/tmp/t1-dbg.log`, sentinel
`/tmp/t1-dbg.done`). The three dedup tests are among the 10 and all print `ok`:
`dedup_distinct_objects_keep_separate_keys`,
`dedup_case_variant_true_duplicates_collapse`, and
`dedup_two_facts_same_pair_different_objects_both_render`.

DEVIATION FROM THE DECLARED VERIFICATION (documented per spec scope). The
declared command is `cargo test --release dedup`; the run above differs in TWO
scoped ways, neither of which changes the test SET or the pass/fail outcome:
(1) DEBUG instead of `--release` — profile changes only optimization/link, never
program logic, so a debug pass is valid evidence a release pass would hold; the
sole reason to prefer debug here is that it skips the fat-LTO link that reaped
every prior release attempt. (2) `--lib` instead of the bare target set —
verified this session that no `tests/*.rs` integration binary matches the name
`dedup` (`grep -rl dedup system/harness/tests/` → NONE), so `--lib` runs the
IDENTICAL test set and only skips compiling unrelated integration binaries. This
doc does NOT claim the byte-exact declared `--release` shared-dir command was
itself observed green in a phase budget — that command never acquired the shared
build lock, held serially by sibling worktrees, before the wall-clock reap.

A second contention factor observed this session: cargo builds spawned by
earlier wall-clock-reaped execute attempts are NOT reliably killed with the
goose process tree — detached subshells survive and keep compiling, so a fresh
attempt inherits several orphaned builds (this session found orphaned
`cargo test --release dedup` processes from prior attempts of THIS task still
holding the shared build lock, at 2h49m and 30m elapsed, plus sibling-task
orphans). Two consequences for anyone re-running this verification: (1) a fresh
attempt should first reap ONLY its own task's orphaned builds (safe) to relieve
CPU and the shared lock — never a sibling task's build, which may belong to a
live worker; (2) a completion watcher must be FAILURE-AWARE, not sentinel-only:
poll `test -f <done-file> || ! pgrep -f <target-dir>` so a reaped build
(process gone, no done-file) is detected rather than hung on forever.
Infra fix (out of scope for this task, flag to the operator): the shared
`CARGO_TARGET_DIR=/Users/mrap/.boi/v2/cargo-target` serializes every sibling
worktree on one build lock, and reaped attempts orphan lock-holding builds.
Fix by either serializing execute phases across sibling worktrees, or giving
each worktree a private target dir seeded by a copy-on-write clone of a
pre-warmed shared target (a COLD private dir is insufficient — it dies
recompiling `aws-lc-sys` from scratch; the COW clone used here avoids that by
inheriting warm deps). Neither removes the fat-LTO link cost, which an operator
could cut by setting `lto = "thin"` for the test/dev path.

---

Implementation record for the approved recall-plateau fix package (spec `Swqqg9f81`).
Diagnosis: `/Users/mrap/hex/projects/system-improvement/diagnoses/recall-plateau-2026-08-31.md`.

Each section below documents one task from the spec's DAG: what changed, file:line
anchors, tests added, and any documented deviations from scope. Sections are appended
by each task's execute worker as it lands.

---

## Task Tznnfa5ga — dirty-build marker in the harness build script

**Cause addressed:** #5 (dirty-build blind spot). `build.rs` baked the committed HEAD
short-sha into `HEX_GIT_SHA`/`harness_version` even when the working tree was dirty, so an
uncommitted deploy read as "nothing changed" — the 8-day blind spot behind the plateau.

**What changed** (`system/harness/build.rs`):
- Hoisted the `manifest_dir` binding to the top of `main()` so it can be reused
  (`system/harness/build.rs:4`); removed the later duplicate declaration that used to sit
  above the module-discovery block (now `system/harness/build.rs:116`).
- After computing the committed short-sha, added a cheap, infallible dirty check
  (`system/harness/build.rs:14`–`48`): run `git status --porcelain --untracked-files=no`
  (tracked-file changes only, staged or unstaged). A non-empty result appends a `-dirty`
  suffix to the sha (`system/harness/build.rs:29`), so a dirty build's `harness_version`
  is distinguishable from its base commit. The emptiness test is whitespace-tolerant
  (`o.stdout.iter().any(|b| !b.is_ascii_whitespace())`).
- Loud-on-failure per SO S6: if `git status` exits non-zero or cannot be spawned, the
  marker is omitted and a `cargo:warning=...` is emitted naming the exit code / spawn
  error (`system/harness/build.rs:33`–`47`) — the build never fails on this path.
- Added `cargo:rerun-if-changed={manifest_dir}/src` (`system/harness/build.rs:60`) so the
  dirty check reruns on ANY tracked harness-source edit, not just the `*.worker.rs` files
  already watched. Without this, editing e.g. `src/memory/assemble.rs` would not rerun
  `build.rs` and the crate would recompile against a cached (possibly clean)
  `HEX_GIT_SHA` — which would have left the exact blind spot this task closes.

**Tests added:** none. The task's declared verifications are the acceptance criteria and
neither requires a test:
- `marker-in-source`: `grep -rq 'dirty' system/harness/build.rs` (exit 0, confirmed).
- `still-builds`: `cargo build --release` (exit 0, confirmed).
Runtime rendering of the suffix is established by the code path rather than asserted from a
hand-run: the `Commands::Version` handler prints `env!("HEX_GIT_SHA")` verbatim
(`system/harness/src/main.rs:1319`), so a dirty build's `hex version` output carries the
`-dirty` suffix directly from the value `build.rs` embeds — no consumer transforms it.

**Downstream-safety check:** all `HEX_GIT_SHA` / `harness_version` consumers treat the
value as opaque text — `hex --version` display (`src/main.rs:1319`), the `harness_version
TEXT` column (`src/memory/schema.rs:133`), and the `String` fields in the eval-trend /
climber-digest workers. No consumer parses, length-checks, or regex-matches the sha, so
the `-dirty` suffix is backward-compatible.

**Documented deviation / note:** `git status --porcelain` invoked from the harness
subdirectory reports **whole-repo** dirtiness, not harness-only — so a docs-only or
sibling-crate edit also flips the `-dirty` marker. This is a deliberate, defensible choice
for a "dirty build" signal (any uncommitted state means the binary does not correspond to
a clean commit) and is recorded here rather than narrowed.

Second note (scope boundary): `--untracked-files=no` means a deploy that consists solely of
**new, untracked** harness source files does NOT flip the `-dirty` marker. This matches the
task wording exactly ("uncommitted changes to tracked files"), but it is worth stating
because it is precisely cause #6's scenario — new harness source shadowed by a blanket
instance `.gitignore` on `.hex/harness/src/`. Closing that gap is the job of task
`Tyeav60q3` (make `hex upgrade` commit the synced source), not this build-script marker;
the two fixes are complementary.

**Functional proof (execute iteration 1).** Beyond the two declared verifications, the
behavior was proven end-to-end by compiling `build.rs` standalone
(`rustc --edition 2021 --crate-type bin`, exit 0) and executing it with `CARGO_MANIFEST_DIR`
/ `OUT_DIR` set, once with cwd inside this (dirty) worktree and once inside a throwaway
`git init` + single committed file:
- dirty worktree → `cargo:rustc-env=HEX_GIT_SHA=91a0e730-dirty` (marker appended);
- clean committed repo → `cargo:rustc-env=HEX_GIT_SHA=200b149` (no marker).
This pins that the branch actually branches — the `-dirty` suffix reaches `HEX_GIT_SHA`
only when the tree is dirty — which neither `grep` (word-in-source) nor `cargo build`
(compiles) proves on its own.

**Caveat to verify later (`build.rs:60`).** `cargo:rerun-if-changed` given a *directory*
(`{manifest_dir}/src`) is relied on above to rerun `build.rs` on any nested source edit.
Cargo's directory handling here should be confirmed with a live touch-a-nested-file test
once the shared build queue clears; if cargo only stats the directory's own mtime rather
than walking it recursively, a nested edit (e.g. `src/memory/assemble.rs`) would not rerun
`build.rs` and the marker could read stale-clean. The per-file `rerun-if-changed` triggers
emitted for every `*.worker.rs` (`build.rs:216`) already cover the worker tree regardless;
this caveat only concerns non-worker nested sources.

**`still-builds` capture (execute iteration 5).** The declared `still-builds` command
(`cargo build --release`) was stalled across five iterations by shared-target-dir lock
contention: every spec worktree inherits one ambient `CARGO_TARGET_DIR`
(`/Users/mrap/.boi/v2/cargo-target`), which cargo serializes with a build-directory flock,
while the task DAG fans build/test verifications out in parallel — so this task's build sat
behind sibling `cargo test`/`cargo build` runs indefinitely. The declared verification does
not pin `CARGO_TARGET_DIR` (it is ambient env, not part of the command string), so the
byte-identical command was run against a private APFS copy-on-write clone of the warm target
(`CARGO_TARGET_DIR=/tmp/t5-target cargo build --release`). Result: `Finished 'release'
profile [optimized] target(s) in 21m 07s`, exit code 0 — the full `hex-harness` crate still
compiles cleanly with the dirty-marker `build.rs`. The clone was removed afterward to reclaim
disk. This is a verification-environment note only; no code changed. Underlying infra fix for
the loop (out of this task's scope, `system/harness/` only): give each spec worktree its own
`CARGO_TARGET_DIR`, or serialize build/test verifications across sibling DAG tasks.
