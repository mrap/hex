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
(`system/harness/src/memory/recall.rs:1205` and `:1275`) operate at the
`facts_recall` / `facts_recall_with_config` RETRIEVAL layer and do NOT route
through `facts_to_candidates` (the merge/dedup layer this task edits), so they
remain valid and unmodified. No legacy pin required updating for this task; the
new default-path behavior is pinned directly by the three dedup tests above.

### Deviations from scope
None to the code, tests, or doc. One operational note on verification below.

### Operational note — verifying `dedup-tests-pass` under shared-target contention
The declared `dedup-tests-pass` verification
(`cargo test --release dedup` in `system/harness`) inherits the BOI harness
env var `CARGO_TARGET_DIR=~/.boi/v2/cargo-target` — a SHARED release
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
(`cp -cR ~/.boi/v2/cargo-target/release /tmp/recallfix-t1-target/release`,
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
(`cp -cR ~/.boi/v2/cargo-target/debug /tmp/t1-dbg-target/debug`, then
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
`CARGO_TARGET_DIR=~/.boi/v2/cargo-target` serializes every sibling
worktree on one build lock, and reaped attempts orphan lock-holding builds.
Fix by either serializing execute phases across sibling worktrees, or giving
each worktree a private target dir seeded by a copy-on-write clone of a
pre-warmed shared target (a COLD private dir is insufficient — it dies
recompiling `aws-lc-sys` from scratch; the COW clone used here avoids that by
inheriting warm deps). Neither removes the fat-LTO link cost, which an operator
could cut by setting `lto = "thin"` for the test/dev path.

---

Implementation record for the approved recall-plateau fix package (spec `Swqqg9f81`).
Diagnosis: the 2026-08-31 recall-plateau diagnosis (operator instance workspace).

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
(`~/.boi/v2/cargo-target`), which cargo serializes with a build-directory flock,
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

---

## Tkmz6c46q — Matching batch (assemble.rs + recall.rs)

### Cause addressed
Diagnosis cause 3 (fetch-stage matching gaps), the sub-set assigned to this task:
predicate-cue table lacks "blocker(s)"; camelCase predicates (`knowsAbout`) index
as one token and are unreachable by the split words; the facts tokenizer drops
2-char tokens like `v2`; entity detection + the slug arm miss hyphen-delimited
and multi-word subjects (`fleet-coordinator`, `hex project`, `hex-v2-arch`); and
M2 orders purely by importance, ignoring query relevance
(`b-brand-lead-restrictions`). Named cases used as fixture shapes: `c-13`
(blockers), `a-mike-knowsabout` (camelCase), `c-14` (`v2`),
`b-brand-lead-restrictions` (M2 relevance), plus the multi-word/hyphen subject
class.

### What changed — six changes

1. **blocker/blockers cue** — `predicate_cues` (`system/harness/src/memory/assemble.rs:82-90`).
   Added the NOUN forms `blocker`/`blockers` to the `blocked-by` cue tuple.
   `predicate_cues` does exact `HashSet` membership (no stemming), so the nouns
   needed explicit entries; the verb-only cues (`block`/`blocked`/`blocking`)
   never caught "who are the blockers".

2. **camelCase predicate reachability** — new retrieval-side arm in
   `facts_recall_with_config` (`system/harness/src/memory/recall.rs:245-302`),
   fused at `recall.rs:316-317`, backed by the `split_camel_words` helper
   (`recall.rs:61-77`). unicode61 indexes `knowsAbout` as the single token
   `knowsabout` (empirically confirmed this session), so `know`/`about` can
   never FTS-match it. The arm splits each DISTINCT predicate on internal
   lower→upper case transitions and, when a query term prefix-matches a split
   word, fuses that predicate's facts as their own ranked arm — structurally the
   same device as the pre-existing slug arm (which exists because FTS "can't see
   inside slugs"). Restricted to genuine case-transition predicates (2+ split
   words); single-token and hyphenated predicates (`decided`, `works-on`) are
   already unicode61-tokenized and are deliberately excluded so the arm does not
   become a new flooding path.

3. **2-char digit tokens kept** — facts tokenizer in `facts_recall_with_config`
   (`system/harness/src/memory/recall.rs:120-127`). The sub-3-char filter now
   keeps 2-char alphanumeric tokens that carry a digit (`v2`, `k8`, `m1`); pure
   2-char alpha words (`of`, `to`, `an`) stay dropped. `v2` was the single most
   distinctive term of case `c-14` and was previously discarded.

4. **Entity detection for hyphen/multi-word subjects** — `detect_entity_subjects`
   (`system/harness/src/memory/assemble.rs:148-172`). Strips an optional leading
   `type:` prefix (so the type token never triggers a match), then splits the
   remainder on every word separator `[':' '-' '_' '/' ' ']` and matches any
   piece (len ≥ 3) against the query tokens. Reaches `fleet-coordinator`,
   `hex project`, `hex-v2-arch`, `person:brand-lead` — none of which is ever a
   single query token.

5. **Slug arm for hyphen/word-boundary subjects** — `facts_recall_with_config`
   (`system/harness/src/memory/recall.rs:180-243`). The slug LIKE pattern was
   colon-only (`%:tok%`); it now matches the token at any word boundary
   (`:`,`-`,`_`,`/`,space) plus start-anchored arms for a token that is the
   subject's FIRST word (next char a separator) or the whole subject. The query
   token is escaped (backslash/percent/underscore) and bound, never interpolated
   (no injection). Two LIKE-metacharacter fixes were made after four review
   rounds (redo 2026-09-02): (a) the underscore separator is written `\_` with an
   explicit `ESCAPE '\'` clause — a literal `_` in a LIKE pattern is a
   single-char wildcard, which had degraded `%_tok%` into an unanchored substring
   match (token "art" wrongly retrieved subject `person:bart-smith`); (b) the
   start-anchored branch, formerly a bare `?1 || '%'` prefix, now requires the
   char after the token to be a separator or the token to equal the whole subject
   (`subject LIKE ?1`, exact yet ASCII-case-insensitive) — a bare prefix had bled
   token "hex" into subject `hexagon`. **Deliberate narrowing (test-pinned):** a
   subject reachable ONLY as a bare prefix of one word — e.g. `alexandra` via
   query token "alex", with no separator and not an exact match — is no longer
   surfaced by the slug arm. This is the intended fix for the prefix-bleed and is
   pinned by `slug_arm_start_anchor_requires_word_boundary`; separator-first-word
   matches (`hex` → `hex-v2-arch`) stay reachable, pinned by
   `slug_arm_first_word_still_matches_at_separator`.

6. **Query-relevance blend into M2** — `m2_entity` (`system/harness/src/memory/assemble.rs:341-407`)
   with helpers `query_terms` (`assemble.rs:180`) and `object_relevance`
   (`assemble.rs:210`). M2 was importance-only, so a low-importance fact that
   actually answers the query was buried below generic high-importance facts
   under the same subject and never entered the per-subject top-K window. M2 now
   fetches a WIDER importance-ordered window (`TOP_K_PER_MOVE * 3`) per subject,
   re-ranks it by (query-relevance, importance), and keeps the top-K — so the
   relevant fact is re-surfaced without changing how many candidates the move
   ultimately contributes.

### Tests added
`system/harness/src/memory/assemble.rs` (`mod tests`):
- `assemble_camelcase_predicate_reachable_by_split_words` (`:1030`) — the
  `camelcase-reachable` proof end-to-end through `assemble()`, with a control
  query that must NOT surface the fact. (Authored in write_red_tests; now green.)
- `predicate_cue_blockers_maps_to_blocked_by` (`:1091`) — "blockers" cues
  `blocked-by`. (Authored in write_red_tests; now green.)
- `entity_detection_reaches_hyphen_and_multiword_subjects` (`:1106`) — hyphen,
  space, and hyphen+version subjects match; the bare `person:` type prefix does
  NOT match every subject of that type.
- `m2_blends_query_relevance_over_importance` (`:1151`) — direct `m2_entity`
  call: a low-importance query-relevant fact, buried past the top-K by
  importance alone, is re-surfaced AND ranks first under the blend.
- `assemble_two_char_versioned_token_reaches_fact` (`:1199`) — a fact sharing
  only the 2-char token `v2` with the query is retrievable end-to-end.

`system/harness/src/memory/recall.rs` (`mod plan2_tests`):
- `split_camel_words_splits_on_case_transition_only` (`:1363`) — unit for the
  helper: camelCase yields 2+ words; hyphenated/single yield one.
- `facts_recall_camelcase_predicate_arm_surfaces_fact` (`:1378`) — the arm at
  the `facts_recall` layer, with the unrelated-query control.
- `facts_recall_keeps_two_char_digit_token` (`:1417`) — `v2` retrievable at the
  `facts_recall` layer.
- `slug_arm_literal_underscore_not_wildcard` (`:996`) — RED→green: token "art"
  must NOT retrieve `person:bart-smith` (the `\_` ESCAPE fix; a literal `_` was
  read as a single-char wildcard).
- `slug_arm_start_anchor_requires_word_boundary` (`:1033`) — RED→green: token
  "hex" must NOT retrieve `hexagon` (the start-anchor now requires a separator or
  exact match, not a bare prefix).
- `slug_arm_keeps_literal_underscore_subject` (`:1069`) — GREEN guard: the
  escaped underscore branch is KEPT, not dropped — `fleet_coordinator` stays
  reachable by "coordin".
- `slug_arm_first_word_still_matches_at_separator` (`:1101`) — GREEN guard:
  anchoring must not kill a legitimate first-word match — `hex-v2-arch` stays
  reachable by "hex".

### Deviations from scope
1. **camelCase split is retrieval-side, not index-side.** The behavior text says
   "with whatever index-side normalization that needs" and the write_red_tests
   comment says "needs index-side camelCase token split feeding facts_fts"; this
   task instead adds a retrieval-time predicate arm. Rationale (verifiable, not a
   preference): `facts_fts` is external-content (`schema.rs:76-84`,
   `content=facts, content_rowid=rowid`). External-content FTS5 rebuilds from
   `facts.predicate` VERBATIM (`schema.rs:220` `INSERT INTO facts_fts VALUES('rebuild')`)
   and the `'delete'` commands in the `facts_fts_ad`/`facts_fts_au` triggers pass
   `old.predicate` raw, so any trigger-time transform of the stored predicate
   desyncs from the content table and corrupts the index. Pure SQL cannot split
   on case transitions, and a custom scalar SQL function referenced from the
   triggers would make every fact INSERT fail on any connection that had not
   registered it — 9+ write sites (distill, consolidate, maintain_facts,
   dedup, …). The retrieval-side arm is the same class of fix already precedented
   by the slug arm and carries zero write-path blast radius. The
   `camelcase-reachable` verification (a `knowsAbout` fact retrievable by
   `know`/`about`) is satisfied either way.
2. **`v2` reachability for `hex-v2-arch`.** The 2-char-token fix makes `v2`
   retrievable via the facts FTS arm (object/subject text). Entity detection and
   the slug arm still gate word pieces at len ≥ 3, so `hex-v2-arch` is reached by
   its `hex`/`arch` pieces, NOT by the `v2` piece. This is deliberate (a 2-char
   entity piece is too weak a signal to widen M2/slug matching on) and is called
   out so the `v2` case is not over-claimed.

### Cross-task note for the flooding task (T8s8bq3th)
Change 4 broadens `detect_entity_subjects`, so M2's firing rate goes UP relative
to develop `91a0e730`. T8s8bq3th's "when M2 detects entities, intersect M3/M4
before the top-K window" work sits directly downstream of this — its baseline
moved; the wider M2 match surface is the intended input to that entity-intersect
fix, which is what keeps the broader match from flooding the merge.

### Backward-compatibility / default-behavior note
This task changes default-path retrieval (a new fused arm, a widened tokenizer,
a broader slug/entity surface, a relevance-blended M2). The named legacy pins
`default_config_reproduces_legacy_facts_recall_exactly` (`recall.rs:1205`) and
`default_config_vector_arm_off_is_byte_identical` (`recall.rs:1275`) compare two
config PATHS of the same function against each other (live default vs explicit
default literals), not against a frozen external baseline, so an additive change
applied uniformly to both paths leaves them equal — verified: their fixtures use
no camelCase/2-char/hyphen shapes, so the new arm/tokenizer contribute nothing
to those fixtures and the pins hold unmodified. No legacy pin required updating;
the new behavior is pinned directly by the eight tests above.

---

## T8s8bq3th — Flooding fixes (recall.rs + assemble.rs)

### Causes addressed
Diagnosis cause 3 "fetch-stage design limits", the flooding sub-class:
- **Generic-token flooding.** OR-expanded facts FTS queries matched 1,300–1,900
  facts and drowned rank-17..52 correct answers outside the top-K window
  (cases a-hex-startup-skill, b-hex-project-correction-rule, c-10).
- **Global per-predicate / per-day windows.** M3 ran one GLOBAL top-6 per
  predicate and M4 one GLOBAL recency window, with NO entity scoping, so a query
  naming one entity was crowded out by higher-importance (M3) or same-day (M4)
  facts from OTHER subjects sharing the predicate (case hex-focus).
- **M4 fires on bare `now`.** The lone filler word `now` fired the temporal move
  and flooded non-temporal questions with same-day facts (case a-mike-building).

### What changed

**1. Drop generic question words AND corpus-ubiquitous tokens from the
OR-expanded facts FTS query** (`system/harness/src/memory/recall.rs`).
- **1a — generic question / filler words.** Extracted the inline OR-expansion
  stopword set into a named predicate `is_generic_query_word(t: &str) -> bool`
  (`recall.rs:99`), preserving the original set and adding generic question /
  filler words: `where, when, why, which, whose, will, can, could, would,
  should, have, has, had, about, here, your, you, our, this, that, with, from,
  into`. A natural-language question's OR expansion narrows to the terms that
  actually discriminate, so it no longer OR-matches a broad slice of the corpus.
- **1b — corpus-ubiquitous CONTENT tokens (dynamic, foundation-safe).** The
  inline token filter was moved into a helper `fts_query_tokens(conn, query) ->
  Vec<String>` (`recall.rs:185`) that runs a THREE-stage filter: (1) sub-3-char
  drop keeping 2-char digit tokens like `v2` (task Tkmz6c46q, unchanged);
  (2) `is_generic_query_word`; (3) a DYNAMIC document-frequency drop of tokens
  appearing in more than half of all facts (`hex` in >50%, the diagnosis's own
  threshold). `facts_recall_with_config` now builds its query as
  `fts_query_tokens(conn, query).join(" OR ")` (`recall.rs:278`). Stage 3 is
  corpus-derived — NO instance-specific token is hardcoded into this foundation
  code — and double-guarded so it can never starve retrieval (see the DELIVERED
  section below for the constants and guards).

**2. Entity-intersected M3/M4 windows**
(`system/harness/src/memory/assemble.rs`).
- `detect_entity_subjects` now runs ONCE in `assemble_with_config`
  (`assemble.rs:744`) and its result is threaded to M2, M3, and M4.
- `m2_entity` takes the pre-detected `subjects: &[String]` instead of detecting
  internally (`assemble.rs:354`) — single source of truth, one DB scan.
- `m3_predicate` (`assemble.rs:434`) and `m4_temporal` (`assemble.rs:494`) take
  `entity_subjects: &[String]`. When NON-empty, they add `AND subject IN (...)`
  to their fetch BEFORE the top-K / recency cut, so foreign-subject facts can no
  longer flood the window. Built with dynamic SQL + `rusqlite::params_from_iter`
  over a `Vec<rusqlite::types::Value>` to avoid mixing `?1` and bare-`?`
  parameter numbering. When EMPTY (no entity named), the SQL is the previous
  global window — the no-entity path stays byte-identical.
- **Known risk (documented, not pre-mitigated).** The intersection is a HARD
  scope: if `detect_entity_subjects` returns a SPURIOUS match (Tkmz6c46q widened
  it to any ≥3-char slug piece, so a weak incidental piece can now fire M2), M3
  is hard-restricted to that wrong subject and may lose a result the old global
  window would have surfaced. The task mandates the intersection, so it ships as
  written; a fallback ("if the entity-scoped M3/M4 fetch returns empty, retry
  the global window") is deliberately NOT added pre-emptively — it would weaken
  the `recall_entity_scoped_window_beats_flooding` discriminator (whose whole
  point is that scoping removes the foreign facts). If a future eval shows a
  regression of exactly this shape (an entity query losing its answer to an
  over-eager M2 match), that empty-intersection-falls-back-to-global retry is
  the intended fix. The full lib suite shows no such regression today.

**3. Gate M4 off a lone `now`** (`system/harness/src/memory/assemble.rs`).
- `is_temporal` (`assemble.rs:113`) splits temporal cues: strong cues
  (`current, latest, today, recent, recently`) always fire M4; a lone `now` is
  treated as NON-temporal. `now` fires M4 only inside an explicit temporal
  phrase (`right now`, `as of now`, `just now`), where it is genuinely temporal.

### Tests added
- `recall.rs:1581` `recall_generic_query_words_classified_droppable` — pins the
  drop-list predicate directly (question words droppable; content tokens incl.
  `v2` survive).
- `recall.rs:1616` `recall_generic_words_dropped_from_or_expanded_fts_query` —
  end-to-end: a lone distinctive content token still retrieves the fact; a
  pure-filler query surfaces nothing (all tokens dropped from the OR expansion).
- `recall.rs:1663` `recall_ubiquitous_token_dropped_above_corpus_floor` — the
  corpus-ubiquitous drop (change 1b): above the corpus floor a token in >50% of
  facts (`hex`) is dropped while a distinctive token (`parallax`) survives and
  still retrieves its fact; below the floor the same token is KEPT (protects
  small fixtures); a query of only-ubiquitous tokens is not emptied
  (zero-survivor guard). Foundation-safe: the drop is derived from live document
  frequency, never a hardcoded token list.
- `recall_entity_scoped_window_beats_flooding` (the phase's red test, now green)
  — the `entity-scoped-windows` verification: a `blocker`→`blocked-by` query
  naming `person:tara`; with M2 entities present, M3's window is scoped to those
  subjects, so NO foreign subject's `blocked-by` fact reaches the assembled
  context; the target entity's low-importance (0.05) fact wins over 10
  higher-importance (0.90) foreign facts. See the fixture-correction deviation
  below for why the cue/predicate is `blocker`/`blocked-by` and not the
  originally-written `preference`/`prefers`.
- `assemble.rs:1758` `recall_m4_gate_ignores_lone_now` — is_temporal gate: lone
  `now` does NOT fire; strong cues and explicit `right now`/`as of now`/`just
  now` phrases DO.
- `assemble.rs:1792` `recall_lone_now_leaves_m4_unfired_in_assemble` —
  end-to-end proof M4 stays unfired for a lone-`now` question in the pipeline.
- `assemble.rs` `m2_blends_query_relevance_over_importance` (task Tkmz6c46q's
  test) updated to pass the pre-detected subjects to the new `m2_entity`
  signature (`detect_entity_subjects` then `m2_entity(..., &subjects, &cfg)`).

### Backward-compatibility / default-behavior note
- The generic-word drop, the corpus-ubiquitous drop, and the entity
  intersection all change default-path behavior deliberately. The named legacy
  pins `default_config_reproduces_legacy_facts_recall_exactly`
  (`recall.rs:1339`) and `default_config_vector_arm_off_is_byte_identical`
  (`recall.rs:1409`) compare two config PATHS of the SAME function against each
  other and route through the identical (now-extended) query expansion, so they
  remain equal; their fixture query `what powers the vector store` uses none of
  the newly-dropped generic words. Crucially the corpus-ubiquitous drop is
  INERT on that fixture: its 4-fact corpus is far below `UBIQUITOUS_MIN_CORPUS`
  (50), so the df stage is skipped entirely and the fixture's FTS query stays
  byte-identical — the `!live.is_empty()` half of the pin cannot be broken by
  it. (The largest test fixture anywhere in the memory module is 30 facts, so
  the 50-fact floor keeps the df stage inert across the whole existing suite;
  only a real instance with thousands of facts engages it.) No legacy pin
  required updating; the new behavior is pinned directly by the six tests above.
- The entity intersection only alters behavior for queries where
  `detect_entity_subjects` returns a non-empty set; every no-entity query keeps
  the prior global M3/M4 SQL byte-for-byte, which is what protects
  `privacy_excludes_private_facts_when_for_agent`,
  `per_move_quota_protects_fired_fact_moves_from_m1_domination`, and
  `floor_places_m1_top1_first_and_each_fired_move_top1` (verified: those
  fixtures detect no entity, or their facts are still covered by M5).

### DELIVERED — corpus-ubiquitous token drop (dynamic, foundation-safe)
The task behavior says "drop or down-weight generic question words AND
corpus-ubiquitous tokens". BOTH halves are now delivered. The
corpus-ubiquitous half (the diagnosis names `hex` in >50% of facts) is
implemented as change 1b: `fts_query_tokens` (`recall.rs:185`) drops any
surviving token whose live document frequency exceeds half the corpus.

Design constraints and how each is met (an earlier draft of this task cut this
half over these same concerns; the guards below resolve them, so it is shipped
rather than deviated):
1. **No hardcoded instance vocabulary.** `hex`/`mike` are ubiquitous only in
   mrap-hex's corpus; this is foundation code shipping to every instance. The
   drop is therefore DYNAMIC — derived per query from live document frequency
   via the SAME porter-tokenized `facts_fts` the arms use (`recall.rs:219-226`)
   — so no instance-specific token is ever baked into shared code. Constants:
   `UBIQUITOUS_DF_FRACTION = 0.5` (`recall.rs:154`, "more than half of all
   facts").
2. **Cannot empty a small corpus / break the legacy pin.** The one objection
   that killed the earlier draft — on the 4-fact
   `default_config_reproduces_legacy_facts_recall_exactly` fixture, `vector`
   and `store` both exceed half and would be dropped, leaving only `powers`
   (matches nothing) and emptying the result — is defused by a corpus-size
   floor: `UBIQUITOUS_MIN_CORPUS = 50` (`recall.rs:148`). Below 50 facts the df
   stage is skipped entirely (df is not a stable signal on a tiny store), so
   the 4-fact fixture (and every ≤30-fact test fixture in the module) keeps its
   FTS query byte-identical. df is a stable "corpus-ubiquitous" signal only on a
   real store (3,451 facts on the diagnosis snapshot).
3. **Cannot starve retrieval.** A second guard: if dropping the ubiquitous
   tokens would leave NO tokens (a query built entirely of ubiquitous terms),
   the pre-drop set is kept (`recall.rs`, zero-survivor guard). A single
   surviving token is never probed or dropped (it is the query's only signal).
4. **Bounded cost & loud-safe.** Per-token df probes are capped at
   `UBIQUITOUS_MAX_PROBES = 12` (`recall.rs:159`); tokens past the cap are kept
   unprobed. Every probe failure (MATCH syntax error, DB error) reads df as 0,
   which KEEPS the token — the drop biases toward keeping, never toward silently
   dropping (SO S6: no quiet mis-drop).

Note on "down-weight": bm25 weighting in FTS5 is per-COLUMN, not per-term (the
codebase's own `arm_weights.content_sql()` / `entity_sql()` vectors like
`1.0, 0.25, 2.0` weight the subject/predicate/object columns), so "down-weight
this one token" is not expressible in a MATCH query; the drop form is the
available realization of the behavior's "drop OR down-weight"; the
guards above make the drop safe. This flooding class is ALSO attacked from the
ranking side by the entity-intersection window (change 2, this task) and the M2
query-relevance blend (task Tkmz6c46q); the df drop additionally shrinks the
candidate SET (the per-arm `LIMIT k*3` window) so ubiquitous-only matches no
longer crowd it. No deviation on this half.

### DEVIATION — red-test fixture correction (write_red_tests output)
The `write_red_tests` phase wrote `recall_entity_scoped_window_beats_flooding`
with a `preference`→`prefers` fixture and a stated discriminator: the 10 foreign
facts "can reach the assembled output through ONE path only, M3's global
window." That premise is FALSE against the live tokenizer, so the test could
never go green from the in-scope M3/M4 fix alone. `facts_fts` uses a `porter`
tokenizer (`system/harness/src/memory/schema.rs:77-83`, which indexes the
`predicate` column), and porter stems the cue word `preference` to `prefer` —
identical to the stem of the indexed predicate `prefers`. So M5's relevance FTS
arm matched EVERY `prefers` fact via the predicate column and surfaced foreign
subjects independently of M3. Evidence (this session):

    sqlite3 ':memory:' "CREATE VIRTUAL TABLE t USING fts5(s,p,o,tokenize='porter unicode61');
      INSERT INTO t VALUES('person:subject0','prefers','the lightweight variant choice');
      INSERT INTO t VALUES('person:tara','prefers','the zzmarker editing style');
      SELECT s FROM t WHERE t MATCH 'preference';"
    -- returns BOTH person:subject0 AND person:tara

And the observed failure with the correct M3 fix in place: assembled `prefers`
subjects were `[person:tara, person:subject0..4]` — the 5 foreign subjects came
from M5, not M3 (scoped M3 contributed only `person:tara`).

The task behavior scopes the fix to M3 and M4 explicitly; a predicate-token
match in M5 is relevance working as designed, not flooding, so M5 was
deliberately NOT entity-intersected. Instead the fixture was corrected to a
predicate whose cue does NOT stem-collide, so the test validly isolates the
M3/M4 behavior it claims to: `blocked-by` cued by `blocker`. `blocker` is an
exact `predicate_cues` entry (fires M3) but porter does not stem it to
`blocked`/`block`, verified the same way:

    sqlite3 ':memory:' "CREATE VIRTUAL TABLE t USING fts5(s,p,o,tokenize='porter unicode61');
      INSERT INTO t VALUES('person:subject0','blocked-by','the lightweight variant choice');
      SELECT s FROM t WHERE t MATCH 'blocker';"
    -- returns nothing

With `blocked-by`/`blocker` the foreign facts reach the assembled output only via
M3's global window, so the test is RED pre-fix (observed: foreign subjects
present) and GREEN post-fix (observed: only `person:tara`). The fixture change
STRENGTHENS the test into a valid discriminator; it does not weaken it (it still
fails pre-fix and passes post-fix, and now genuinely proves the entity
intersection rather than an M5 artifact).

### Verification execution — observed results (execute redo, 2026-09-03)
This execute session added change 1b (the corpus-ubiquitous df drop) on top of
the prior in-flight implementation already on disk, then verified the whole
task. The shared `CARGO_TARGET_DIR=~/.boi/v2/cargo-target` release
tree was under the fat-LTO + cross-worktree lock contention documented under
T28958xxp and Tznnfa5ga (this session a sibling worktree held the release lock,
and a CoW clone of the release tree via `cp -cR` timed out after 2 min). Per the
established precedent of every prior task in this spec, the test LOGIC was
therefore proven under the DEBUG profile on the uncontended shared debug tree;
the profile change affects only optimization/link, never program logic (the
debug frontend runs the identical type/borrow checking, so a clean debug build
proves the source compiles). Exact commands and exit codes observed THIS session:

- `flooding-tests-pass` (declared `cargo test --release recall`): run as
  `cargo test recall` (DEBUG profile, ambient shared debug target) → **exit 0**;
  the lib unit-test set matching the `recall` filter reported **42 passed;
  0 failed; 0 ignored** (log `/tmp/recallfix-t4-debug.log`). All six task tests
  are among them and print `ok`:
  `recall_generic_query_words_classified_droppable`,
  `recall_generic_words_dropped_from_or_expanded_fts_query`,
  `recall_ubiquitous_token_dropped_above_corpus_floor`,
  `recall_entity_scoped_window_beats_flooding`,
  `recall_m4_gate_ignores_lone_now`,
  `recall_lone_now_leaves_m4_unfired_in_assemble`. The bare `cargo test recall`
  also builds and runs every `tests/*.rs` integration binary; none defines a
  test matching `recall` (each reports `0 passed; N filtered out`), so the run's
  pass SET equals the declared `cargo test --release recall` command's set.
  DEVIATION (documented): DEBUG profile instead of `--release`. The declared
  command's `--release` fat-LTO link of the cfg(test) binary is the wall that
  reaped 5+ prior execute attempts on this spec (see the T28958xxp verification
  note); a debug build compiles and runs the identical test logic and is the
  precedent stand-in used by T28958xxp and Tznnfa5ga. No `--release` test binary
  was linked this session.
- `build-clean` (declared `cargo build --release`): NOT re-run to completion
  this session (the release lock/LTO wall above; the CoW clone attempt timed
  out). Source compilation is instead proven by the DEBUG builds underlying the
  two test runs below, which compile the full `hex-harness` lib (frontend
  type/borrow checking is profile-independent). The prior in-flight session
  recorded a genuine `cargo build --release` → exit 0 in 14m 27s on the
  PRE-change-1b tree; change 1b adds only ordinary `rusqlite` query calls and
  three `const`s (no new deps, no unsafe, no macro), so it does not alter the
  release-compilability the debug build confirms.
- `entity-scoped-windows`: satisfied by
  `recall_entity_scoped_window_beats_flooding` (`assemble.rs:1671`) passing
  green — the target subject `person:tara`'s low-importance (0.05) `blocked-by`
  fact wins over 10 higher-importance (0.90) foreign facts once M3 is
  entity-scoped. The discriminator's validity was re-confirmed this session via
  the two porter probes quoted above (run this session): `blocker` does NOT
  porter-stem to `blocked`, so the foreign facts reach the assembled output ONLY
  through M3's global window — never M5 — which is what makes the test a genuine
  M3/M4 discriminator rather than an M5 artifact.
- Full-suite regression (spec-level `tests-green`): `cargo test --lib` (DEBUG,
  ambient shared debug target) → **688 passed; 0 failed; 7 ignored; 0 measured;
  exit 0** across 695 lib tests (log `/tmp/recallfix-t4-fulllib.log`) — no
  regression anywhere in the harness lib from the df drop, the entity
  intersection, the M4 gate, or the generic-word drop. This is the whole harness
  lib unit-test set, a superset of the `recall`-filtered run.
- Red-before-green (prior in-flight session, corroborating): the prior execute
  session directly observed the entity-intersection TDD gate — with the
  `m3_predicate` intersection block neutered, the entity-scoped test failed with
  assembled `blocked-by` subjects `[person:tara, person:subject0..5]` (6 foreign
  from the global M3 window, none from M5); restoring the block flipped it to
  `ok`. This session did not re-run that neutered-code experiment (it would
  require editing then reverting `m3_predicate`); the same conclusion follows by
  construction from the porter probes this session ran (M5 cannot reach the
  `blocked-by` foreign facts), so M3's global window is their only pre-fix path.
  change 1b does not touch M3/M4, so this observation is unaffected by it.

---

## Thbgp5304 — Tuner v2: widen the sweep + observable held-out vetoes (recall_tune.worker.rs)

### Cause addressed

Diagnosis cause 4 (verdict 3, adversarial round): the Sunday recall tuner is
healthy — its zero-lands record comes from the held-out zero-regression gate
correctly vetoing the only candidates the (narrow) knob space produced, which
were all trades (one gain for one loss). Two structural weaknesses: (a) the
sweep only stepped one knob at a time, so a fix needing two parameters to move
together was unreachable; (b) a vetoed trade recorded only a bare regression
count, so a human reading `regret_log` could not tell WHICH held-out cases were
gained and lost. This task widens the candidate space and makes the veto
observable, while keeping the accept rule byte-identical.

### What changed — three changes

1. **Two-knob widening + raised cap.** `propose_variants` now emits the original
   9 single-knob variants PLUS 12 two-knob combinations (three axis pairs, a
   bounded 2x2 cross-product each) — a static 21 variants. `MAX_VARIANTS` raised
   12 -> 24 to admit them with headroom. Every perturbation is still computed
   relative to the CURRENT (base) value, so a two-knob variant is an independent
   two-axis step, not a compounding one. `propose_variants` still takes ONLY the
   config (no cases path), so held-out isolation is unchanged.
2. **`should_land` extraction (accept rule UNCHANGED).** The previously-inline
   land decision `best_heldout >= current_heldout && heldout_regressions == 0`
   is extracted verbatim into a named `should_land(best, current, regressions)`
   function so it can be pinned by a unit test. This is a pure extraction — the
   boolean expression is byte-identical, moved not modified. Per the spec
   exclusion ("do not touch the held-out zero-regression gate's accept rule —
   observability only"), the gate's behavior is preserved exactly.
3. **Observable vetoes.** A new `veto_record(cfg, best, current)` builder derives
   `lost_cases` (held-out regressions) and `gained_cases` (new passes) from
   `eval::compare(best, current)` — the same call and orientation the gate's own
   regression count uses — and embeds the rejected config. The reject branch now
   writes this record to `regret_log.params_json` and emits the named gained/lost
   case lists loudly on stderr AND in the `recall_tune.rejected` telemetry
   (previously a bare count only), per S6.

### File:line anchors (`system/harness/src/modules/recall_tune.worker.rs`)

- `MAX_VARIANTS` raised to 24 — `recall_tune.worker.rs:57`
- `propose_variants` two-knob widening — `recall_tune.worker.rs:106` (two-knob
  cross-products begin at the `rrf_pair`/`unfired_pair`/`content_pair` block)
- `should_land` (extracted accept rule) — `recall_tune.worker.rs:379`
- `veto_record` (named gained/lost builder) — `recall_tune.worker.rs:394`
- full-compare capture `(heldout_lost, heldout_gained)` in the run — `recall_tune.worker.rs:585`
- `let land = should_land(...)` call site — `recall_tune.worker.rs:592`
- reject branch using `veto_record` + loud emit — `recall_tune.worker.rs:635`

### Tests added (`recall_tune.worker.rs`, `mod tests`)

- `propose_variants_widens_to_two_knob_combinations_within_cap` (`recall_tune.worker.rs:951`)
  — asserts the sweep exceeds the old 9 single-knob variants, pins the exact
  static count 21, pins `MAX_VARIANTS == 24`, asserts the count stays within the
  cap, and asserts at least one variant perturbs two or more knobs at once.
- `should_land_holds_the_zero_regression_accept_rule` (`recall_tune.worker.rs:996`)
  — pins the accept rule: `(8,7,0)` and `(7,7,0)` land; the discriminating
  `(8,7,1)` (score up but one regression) is vetoed; `(6,7,0)` (score drop) is
  vetoed. Catches any loosening of the gate.
- `veto_record_names_gained_and_lost_cases` (`recall_tune.worker.rs:1017`) — an
  asymmetric trade fixture (case `c-5` lost, `hex-focus` gained) proves the
  record NAMES both, that `heldout_regressions` equals the lost count, that the
  reason is `heldout_regressions`, and that the config is embedded. The
  asymmetry catches a gained/lost inversion.

### Backward-compatibility / default-behavior note

The accept rule is byte-identical (change 2 is a pure extraction, pinned by
`should_land_holds_the_zero_regression_accept_rule`); the tuner lands exactly the
same candidates it would have before. The widened sweep only enlarges the set of
candidates SCORED, never loosens the gate that admits them — so a wider sweep can
only ever find MORE clean lands, never a worse one. No existing test asserted a
variant count or `MAX_VARIANTS` value outside this file (verified by grep), so no
external pin needed updating. `regret_log.params_json` gains two keys
(`lost_cases`, `gained_cases`) and the reject path's `reason` derivation is
unchanged; a grep confirmed no code outside this worker deserializes
`params_json`/`regret_log`, so the additive payload shape breaks no reader.

### Deviations from scope

None in the code. All work is within `system/harness/`; the accept rule is
untouched behaviorally; the vector arm is not touched. One deviation in the
VERIFICATION METHOD (not the code), documented next, matching the precedent set
by T28958xxp / Tznnfa5ga / T8s8bq3th on this spec's fat-LTO wall.

### Verification execution — observed results (execute, 2026-09-03)

- `build-clean` (declared `cargo build --release`): run to completion this
  session on the shared release target → **Finished `release` profile
  [optimized] in 13m 02s, 0 errors, exit 0** (log `/tmp/recallfix-build.log`).
  This is the byte-exact declared command; the release binary compiles with the
  tuner-v2 changes.
- `tuner-tests-pass` (declared `cargo test --release recall_tune`): the `recall_tune`
  module's tests were run GREEN via a private, contention-free target dir CoW-cloned
  from the warm shared DEBUG tree (`cp -cR ~/.boi/v2/cargo-target/debug
  /tmp/recallfix-t6-target/debug`, then `CARGO_TARGET_DIR=/tmp/recallfix-t6-target
  cargo test --lib recall_tune`). Observed: **`running 10 tests` ... `test result:
  ok. 10 passed; 0 failed; 0 ignored; 688 filtered out`, exit 0** (log
  `/tmp/recallfix-t6.log`). The filter matched the module path (10 selected, NOT a
  vacuous zero-test match); the three added tests appear and pass:
  `propose_variants_widens_to_two_knob_combinations_within_cap`,
  `should_land_holds_the_zero_regression_accept_rule`,
  `veto_record_names_gained_and_lost_cases`.
  DEVIATION (documented, precedent stand-in): DEBUG profile instead of `--release`,
  and `--lib` instead of the bare target set. The declared `--release` fat-LTO link
  of the cfg(test) binary is the >10-minute wall that reaped 5+ prior execute
  attempts on this spec (see the T28958xxp operational note); a debug build compiles
  and runs the IDENTICAL test logic (profile changes only optimization/link, never
  program logic), and `--lib` runs the identical test SET because no `tests/*.rs`
  integration binary matches `recall_tune` (`grep -rl recall_tune system/harness/tests/`
  → NONE this session). Release COMPILABILITY of the same source is independently
  confirmed by the `cargo build --release` → exit 0 above.
- `veto-observability`: satisfied by `veto_record_names_gained_and_lost_cases`
  passing green — the asymmetric fixture proves the record NAMES the gained
  (`hex-focus`) and lost (`c-5`) held-out cases, not just a count — and by
  `should_land_holds_the_zero_regression_accept_rule` passing green, which pins the
  accept rule byte-identical (the `(8,7,1)` trade is still vetoed).

---

## Tsfwg7d2v — Conservative fact-canonicalization pass in consolidation (consolidate.rs)

### Cause addressed
Diagnosis cause 2: fact-store duplication and subject fragmentation. Near-duplicate
facts are re-extracted many times (one decision logged 6+ times; the fleet
coordinator split across 3 subject spellings — `fleet-coordinator`,
`Fleet Coordinator`, `hex-fleet-coordinator`) and crowd the fixed top-6 recall
windows, fragmenting signal. There was no fact canonicalization/dedup in the
consolidation pipeline.

### What changed
A new deterministic consolidation op, `fact-canonicalize`, runs inside
`memory::consolidate::run` (no LLM; safe for the nightly/unattended quick pass).
It folds case-variant and separator-variant subject spellings onto ONE canonical
grouping key, and within each `(canonical-subject, predicate)` group collapses
high-confidence near-duplicate facts down to the single best fact — the newest,
most-complete wording — by TOMBSTONING the rest. It NEVER deletes a row.

Design (all conservative by construction):
- **Canonical subject** is a GROUPING KEY only: lowercased, separator runs
  (space/tab/underscore/hyphen) folded to a single hyphen, ends trimmed. Pure
  case+separator variants collapse (`Fleet Coordinator` == `fleet-coordinator`);
  a spelling that adds/removes a whole token (`hex-fleet-coordinator`) stays
  DISTINCT. The stored `subject` column is NEVER rewritten — lowercasing live
  subjects would destroy display/eval spellings (`Mike` -> `mike`) and break the
  subject-exact queries the tests pin.
- **Object similarity** is the overlap coefficient `|A ∩ B| / min(|A|, |B|)` over
  lowercased alphanumeric token SETS, threshold `0.8`, AND at least 2 shared
  substantive tokens. Superset-tolerant on purpose (a decision restated more
  completely scores 1.0 against its shorter form); the 2-token floor stops a
  single common word from ever triggering a collapse.
- **Polarity guard** (added this iteration after an adversarial review found a
  contradiction-collapse). The raw overlap coefficient scores a strict superset
  1.0 even when the extra tokens FLIP the claim: `use Postgres` vs
  `do not use Postgres` share `{use, postgres}`, min-length 2, overlap 1.0 — the
  old code tombstoned the shorter as a "near-duplicate" of its own negation. So
  before scoring, if the tokens that DIFFER between the two objects (their
  symmetric difference) contain any explicit negation (`is_polarity_token`: not /
  no / never / none / cannot / without / neither / nor / … plus common
  no-apostrophe contraction spellings), the pair is never a duplicate. A
  token-overlap metric cannot tell a claim from its negation with high
  confidence, so the conservative choice is to keep both (spec HARD CONSTRAINT /
  `conservative-collapse`). The guard fires only when the negation is the
  DISTINGUISHING token — two facts that both contain `no` collapse normally on
  their non-negation difference.
- **Best-first leader clustering** within a group: sort by newest `updated_at`,
  then longest object, then id; a candidate is absorbed only if it is directly
  near-duplicate with the SURVIVING leader (no transitive chaining through a
  bridge fact).
- **Loud, never-silent collapse** (Rule S6 / spec constraint): every collapse
  logs to stderr with BOTH fact ids AND BOTH objects, records a `fact_history`
  UPDATE row naming the survivor (durable ledger), and emits a
  `fact-canonicalize::collapse` telemetry event. A live row with a NULL id is
  counted and warned about, not silently skipped, and excluded from the pass.
  A collapse guarded on `tombstone = 0` keeps re-runs idempotent.

### File:line anchors (`system/harness/src/memory/consolidate.rs`)
- Op registered in `run` — line 27 (`iso!("fact-canonicalize", op_fact_canonicalize(conn))`),
  placed after `catchup-distill` and before the `dedup` stub.
- `const OBJECT_SIM_THRESHOLD: f64 = 0.8` — line 105.
- `struct CanonFact` (loaded live-fact row) — line 108.
- `fn canonical_subject_key` (grouping key; documents the no-rewrite rule) — line 129.
- `fn object_token_set` (tokenizer) — line 149.
- `fn is_polarity_token` (negation/polarity token set for the collapse guard) — line 163.
- `fn objects_are_near_duplicates` (polarity guard + overlap coefficient + 2-token floor) — line 207.
- `fn op_fact_canonicalize` (NULL-id guard, grouping, best-first leader clustering) — line 243.
- `fn tombstone_duplicate_fact` (tombstone-not-delete + loud triple-channel log) — line 343.

### Tests added (`system/harness/src/memory/consolidate.rs`, `mod tests`)
Three tests were authored in the `write_red_tests` phase and drive the stable
`consolidate::run` entry point; this task's implementation turns the two positive
ones green while keeping the negative guard green. A FOURTH negative test was
added in the execute phase after an adversarial review surfaced the
contradiction-collapse class (see the polarity guard above).
- `insert_canon_fact` helper (line 611) — inserts a live fact with explicit id +
  timestamps.
- `canonical_folds_fleet_coordinator_case_and_separator_spellings` (line 641) —
  the three-spelling case: all 3 rows survive (tombstone-not-delete); the two
  pure case+separator variants (`fleet-coordinator` / `Fleet Coordinator`,
  identical object) fold to ONE live row (proves subject canonicalization, since
  the collapse rule is keyed on canonical-subject); the extra-token
  `hex-fleet-coordinator` row survives untouched.
- `canonical_collapses_repeated_decision_near_duplicates` (line 721) — four
  near-duplicate `Mike/decided` facts collapse to the single newest, most-complete
  wording (`dec-best`); all 4 rows still present (tombstoned, not deleted).
- `canonical_keeps_distinct_facts_sharing_subject_and_predicate` (line 787) — the
  negative guard: two genuinely distinct `Mike/works-on` claims both survive live.
  Pins conservatism (`conservative-collapse` verification).
- `canonical_keeps_polarity_flipped_near_superset_facts` (line 835) — the
  contradiction-class negative guard: `Mike/decided/use Postgres` and
  `Mike/decided/do not use Postgres` share a subject+predicate and one object is a
  strict superset of the other (overlap 1.0), yet they state OPPOSITE claims, so
  both must survive live. Confirmed RED against the pre-guard code (the log showed
  `use Postgres` tombstoned as a near-duplicate of `do not use Postgres`), GREEN
  after the polarity guard. Hardens `conservative-collapse` for the highest-volume
  `decided` predicate group.

### Backward-compatibility / default-behavior note
This adds a NEW deterministic op to `consolidate::run`; it changes default
consolidation behavior (near-duplicate facts now collapse). The two pre-existing
consolidate tests are unaffected and stay green: `consolidate_stamps_last_consolidated_metadata`
(inserts no facts) and `prune_is_paused_old_facts_survive` (its single fact is a
group of one — nothing to collapse — and its NULL id is handled by the loud
NULL-id guard, so the op leaves the row untouched with `tombstone = 0`). No
`default_config_reproduces_legacy_facts_recall_exactly`-style retrieval pin routes
through consolidation, so none required updating.

### Deviations from scope
- **Strict token-subset objects are collapsed to the longer wording.** The
  repeated-decision test requires collapsing `reply in GDD style` into
  `reply in GDD style for all design replies` (overlap coefficient exactly 1.0;
  length ratio exactly 0.5, so a length-ratio guard would either be a no-op or
  fail the test outright). This is a policy the tests already decided, not a STOP
  CONDITION. Residual risk: a more-specific claim could be absorbed into a more
  general one that shares all the general one's tokens; mitigated by the 2-shared-
  token floor and by tombstone-not-delete (every collapse is reversible and
  logged with both ids/objects).
- **The third fleet spelling (`hex-fleet-coordinator`) is intentionally NOT
  merged.** It carries an extra token, which cannot be distinguished from a
  genuinely more-specific entity with high confidence — so per the spec STOP
  CONDITION on indistinguishable classes, only case+separator variants merge; a
  token-difference does not.
- **The stored `subject` column is grouped, not rewritten** (deviation from the
  behavior text "canonicalize … subject spellings to one canonical subject"). The
  behavior is honored at the CONSOLIDATION boundary: case+separator variant
  spellings are folded onto one canonical grouping key and their near-duplicate
  facts collapse onto a single surviving row, so the fragmentation the diagnosis
  targets is removed. But the surviving row keeps its ORIGINAL subject string —
  the pass never issues `UPDATE facts SET subject = …`. Rewriting live subjects
  (`Fleet Coordinator` -> `fleet-coordinator`, `Mike` -> `mike`) would destroy the
  display/eval spellings and break the subject-exact queries the tests pin, and a
  non-duplicate fact under a variant spelling would need its own rewrite with no
  duplicate to anchor "canonical" — a lossy guess this pass deliberately avoids.
  Net: spellings are unified for GROUPING and duplicates collapse; distinct facts
  retain their as-written subject. Conservative and reversible; flagged here so a
  validator reads it as an intentional deviation, not an omission.
- **Contradiction-collapse guard added post-review** (see the polarity guard in
  "What changed" and the `canonical_keeps_polarity_flipped_near_superset_facts`
  test). Not a scope change — it strengthens the conservatism the spec HARD
  CONSTRAINT already required — but recorded because it altered
  `objects_are_near_duplicates` after the first execute pass.

### Operational note — verifying `canon-tests-pass` under shared-target contention
Same infra reality as T28958xxp above: the declared `canon-tests-pass`
verification (`cargo test --release canonical` in `system/harness`) inherits the
BOI harness env `CARGO_TARGET_DIR=~/.boi/v2/cargo-target`, a SHARED
release target thrashed by every sibling worktree's build lock, and the fat-LTO
link (`lto = true` at workspace-root `Cargo.toml:8`, release-only) of the test
binary is the >10-minute wall that reaped prior attempts. To obtain a genuine
exit code off that wall, this session ran the same `canonical` lib tests in a
private, contention-free target dir CoW-cloned from the warm shared tree
(`cp -cR ~/.boi/v2/cargo-target/debug /tmp/recallfix-t2-target/debug`,
then `CARGO_TARGET_DIR=/tmp/recallfix-t2-target cargo test --lib canonical`).

---

link (`lto = true` at workspace-root `Cargo.toml`, release-only) of the test
binary is the >10-minute wall that reaped the earlier `validate` phase.

DEVIATION FROM THE DECLARED VERIFICATION (documented per spec scope), identical in
kind to T28958xxp: the genuine exit code was obtained in DEBUG (`cargo test --lib
canonical`) rather than `--release`. Justification: (1) the release profile
changes only optimization/link, never program logic, so a debug pass is valid
evidence a release pass would hold; (2) the sole reason to prefer debug is that it
omits the fat-LTO link that reaped every release attempt; (3) `--lib` skips only
ONE integration binary — the `canonical` name filter matches, across
`system/harness/tests/*.rs`, exactly one test, `memory_consolidate_is_the_canonical_subcommand`
in `tests/consolidate_subcommands_removed.rs` (verified this session by
`grep -rn 'canonical' system/harness/tests/`), which asserts CLI-subcommand
routing and is unrelated to this task's memory-DB change — so the four
fact-canonicalization tests and the three pre-existing `*canonical*` lib tests
run identically under `--lib`.

OBSERVED RESULT (execute iteration 1, this session — genuine). All runs used the
shared debug target `~/.boi/v2/cargo-target` directly (debug builds were
already warm, so no lock contention hit them):

1. Baseline (before the polarity guard): `cargo test --lib canonical` → **exit 0**,
   `test result: ok. 6 passed; 0 failed; ... 674 filtered out` (build finished in
   6.05s on warm deps; tests in 1.46s).
2. RED evidence for the contradiction bug: `cargo test --lib
   canonical_keeps_polarity_flipped_near_superset_facts` → **FAILED**
   (`1 failed`), stderr showing `COLLAPSE tombstoning fact id=pol-1 object="use
   Postgres" as near-duplicate of surviving fact id=pol-2 object="do not use
   Postgres"` — the pre-guard code collapsed a claim into its own negation.
3. GREEN after the polarity guard: `cargo test --lib canonical` → **exit 0**,
   `test result: ok. 7 passed; 0 failed; ... 674 filtered out` (log
   `/tmp/canon-green.log`). The 7 tests are the four fact-canonicalization tests
   added for this task —
   `canonical_folds_fleet_coordinator_case_and_separator_spellings`,
   `canonical_collapses_repeated_decision_near_duplicates`,
   `canonical_keeps_distinct_facts_sharing_subject_and_predicate`,
   `canonical_keeps_polarity_flipped_near_superset_facts` — plus three pre-existing
   unrelated `*canonical*` lib tests (`ledger_canonical_json_is_stable`,
   `maintain_sweeps_orphan_vectors_and_canonicalizes_transcript_files`,
   `github_canonical_url_is_not_flagged`), all `ok`.

The byte-exact declared `cargo test --release canonical` command WAS launched this
session (`system/harness`, shared target, exact declared string) but never reached
the compile step: its log shows `Blocking waiting for file lock on build
directory` with ~16 sibling cargo/rustc processes holding the shared release lock
for their own LTO builds; it was moved to the background at the 600s tool limit
with no exit code within the phase budget. So it is NOT claimed observed-green —
the debug run (3) stands as the genuine verification, valid evidence for release
since the profile never changes program logic.
