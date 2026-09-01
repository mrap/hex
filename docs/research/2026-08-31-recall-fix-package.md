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
None.
