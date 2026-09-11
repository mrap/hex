# BOI phone notifications — human-readable names

Spec: `S1vwthf8e` / task `T6k580e5a`. Base commit `b8f63213ed4db03a135b2d2ef280a45a99eec1c3`.

## The bug

`boi-spec-watch` (`system/harness/src/modules/boi_spec_watch.worker.rs`) alerts
through the shared rail in `system/harness/src/alert.rs`. The third-party push
(phone) body was built by `push_body(key, title)`:

```rust
fn push_body(key: &str, title: &str) -> String {
    format!("[{key}] {title}")
}
```

`title` is a **static** string per alert class ("BOI spec terminal", "BOI task
blocked", "BOI task starved") and `key` is the internal machine dedupe key
(`boi-spec-watch:spec-terminal:{spec_id}`, etc.). The real content — the
outcome (`completed` / `failed` / `canceled`) and any job-recognizable label —
lived only in `msg`, which `push_body` never touches (by design: `msg` may
carry diagnostic detail that must never reach the third-party push service).

Net effect: every phone notification for every BOI job read identically, e.g.

```
[boi-spec-watch:spec-terminal:Sxxxxxxxx] BOI spec terminal
```

— regardless of whether the spec completed, failed, or was canceled, and with
no way to tell which job it was without decoding the id (`Sxxxxxxxx` is a
placeholder for the machine id).

## Target message contract

- Push body = a bounded, single-line **NAME** + a plain, truthful **OUTCOME**
  word. Never the raw internal alert key. Never a raw spec/task id. Never the
  diagnostic reason.
- Spec outcomes are the literal terminal status word: `completed`, `failed`,
  `canceled`. Never invent a success word for a non-completed status.
- Task outcomes use two **distinct** words so blocked and starved stay
  distinguishable on the phone:
  - blocked → `needs attention`
  - starved (active, no live `phase_run` for 30m) → `stalled`

  **Deviation from the spec's suggested vocabulary:** the spec lists
  `waiting to run` as one of five suggested plain outcomes. It is not used
  here. "Waiting to run" reads as benign — the operator assumes the scheduler
  will get to it — which under-alarms exactly the failure mode this task fixes:
  a task that is `active` but has made no progress for 30 minutes needs a human
  to look, not a "still queued" read. `stalled` is used instead. Pinned in
  `red_task_blocked_and_starved_push_distinct_truthful_outcomes_with_label`.
- Every terminal spec push and every task push carries a **name slot** — a
  resolved name, or the bounded fallback `Unnamed job` — never an outcome word
  alone with nothing to identify the job.
- Name resolution:
  - **Spec alerts**: prefer the current spec version's human `title`.
  - **Task alerts**: the existing `ref` label
    (`Transition::{TaskBlocked,TaskStarved}.ref_`) — already computed today,
    just never routed to the push body before (it only reached `msg`, i.e.
    email/stderr/telemetry).
- Missing, blank, malformed, or unavailable display metadata (`None`, `""`,
  whitespace-only, or a path/credential-shaped value) must **not** prevent the
  alert. It falls back to `Unnamed job`. Chosen approach: **reject and fall
  back**, not partial redaction — a name is either trustworthy and shown, or it
  isn't and `Unnamed job` ships instead.
  - The path check rejects path-shaped values and embedded private/system
    paths (`/…`, `~/…`, `/Users/…`, `/private/…`, `/var/…`, `/home/…`, and
    `/etc/…`). Ordinary slash-bearing prose such as `docs/testing.md refresh`
    remains valid. The label source (`Transition.ref_`, a BOI task `ref`) is
    operator-authored and not a filesystem path in practice.
  - The credential check rejects well-known secret prefixes (`ghp_`,
    `gho_`, `ghu_`, `ghs_`, `ghr_`, `github_pat_`, `xoxb-`, `xoxp-`, `AKIA`).
- Names are folded to a single line and length-bounded (≤120 bytes of name +
  ellipsis) without splitting a UTF-8 character (no panics, no mojibake). The
  outcome word is appended AFTER the bounded name, so truncation never eats it.
- Machine ids (`spec_id`, `task_id`) and diagnostic detail (`blocked_reason`,
  the spec scope/instructions) stay in local logs, telemetry, and the
  first-party email body — never the primary phone body.
- Internal dedupe keys, priority/email `AlertClass` routing, the hourly push
  cap, per-task flap/anti-storm behavior, transition detection, and baseline
  persistence are all unchanged by this work.

## Design decision (execute): push-only override seam — option (b)

`notify_at_with_class`'s single `title` parameter feeds BOTH `push_body(key,
title)` AND the email subject/body. The spec explicitly authorized job/task
names in the **phone** alerts, not a wider change. Two options were on the
table:

- (a) pass the pre-composed `name + outcome` string in as `title` — but that
  still yields a push body of `[key] title`, which re-embeds the machine key
  and id, and it also changes the email subject.
- (b) **chosen** — keep `title` as today's static string for the email/stderr
  rail, and thread the rendered `name + outcome` string to the **push body
  only**, via a new crate-internal seam.

Option (b) is implemented as an additive, `pub(crate)` push-body override:

- `alert::notify_with_class_push(key, title, msg, class, push_override:
  Option<&str>)` and its inner `notify_at_with_class_push(...)`. When
  `push_override` is `Some`, the third-party push carries exactly that string;
  the stderr line, telemetry row, macOS banner, and email subject/body are
  **unchanged** and still use `key`/`title`/`msg`.
- `alert::push_body(key, title)` and every other caller of the shared alert
  path are untouched — the generic-payload contract (and its test
  `push_body_contains_no_path_email_or_personal_tokens`) still holds for every
  other alert in the codebase.
- Only `boi-spec-watch`'s `emit_spec_alert` / `emit_task_alert` pass an
  override. **Result:** the phone push changes; the email subject/body for a
  `WorkOrderFailed` spec still reads `[hex alert] BOI spec terminal` with the
  full `msg` (email is first-party, so it keeps carrying full detail). A
  reviewer can check the email body against this stated intent.

The `_push` entrypoints are `pub(crate)`, not `pub`, so the "arbitrary push
body" seam never becomes public API.

## Schema verification (independent, from the DDL — not a live row)

The spec-title lookup was verified against the **current** BOI schema, read
from the table DDL and foreign key, not an assumed layout:

- `spec_runtime(spec_id TEXT PRIMARY KEY, current_version INTEGER NOT NULL,
  status TEXT NOT NULL, …)` with
  `FOREIGN KEY (spec_id, current_version) REFERENCES spec_versions(spec_id,
  version)`.
- `spec_versions(spec_id TEXT, version INTEGER, snapshot JSON NOT NULL, …,
  PRIMARY KEY (spec_id, version))`, where `snapshot` is a JSON blob with a
  top-level `"title"` key.

So the version-correct title is:

```sql
SELECT sv.snapshot
FROM spec_versions sv
JOIN spec_runtime sr
  ON sr.spec_id = sv.spec_id AND sr.current_version = sv.version
WHERE sv.spec_id = ?1;
```

No row from the live database is embedded in this document or in any test.

## Implementation

`system/harness/src/alert.rs`:

- Added `notify_with_class_push` / `notify_at_with_class_push` (`pub(crate)`)
  and threaded an `Option<&str>` push-body override to `deliver_rails`. Existing
  `notify` / `notify_at` / `notify_with_class` / `notify_at_with_class`
  signatures and behavior are unchanged (they delegate with `None`).

`system/harness/src/modules/boi_spec_watch.worker.rs`:

- `parse_spec_title(json) -> Option<String>` — extract the top-level `title`;
  malformed JSON / missing key / non-string value → `None`.
- `spec_title_from_conn(&Connection, spec_id) -> Option<String>` — the
  version-correct lookup (the join above), unit-tested against a temp SQLite
  fixture. Query error → LOUD stderr (S6) then `None`.
- `resolve_spec_title(spec_id)` — production opens `~/.boi/v2/boi.db`
  READ-ONLY; absent db → quiet `None`, open error → LOUD then `None`.
  `emit_spec_alert_from_db` accepts an explicit fixture or missing path for
  tests, so worker tests never fall back to the live database. The lookup core
  is also covered directly against an in-memory fixture.
- `display_name(Option<&str>)`, `looks_like_path`, `looks_like_credential`,
  `truncate_name`, `render_push_body(name, outcome)` — the sanitize + compose
  seam.
- `emit_spec_alert` / `emit_task_alert` now build a `name + outcome` push
  override and call `notify_with_class_push`. The diagnostic `msg`
  (reason/id/flap) is unchanged and still goes to stderr/telemetry/email only.

A missing / blank / malformed / unavailable title or label never blocks a state
transition or its alert — every failure path degrades to `Unnamed job`.

## Rendered examples (synthetic)

Before (the bug):

```
[boi-spec-watch:spec-terminal:Sxxxxxxxx] BOI spec terminal
```

After (all names below are synthetic):

| Case | Push body |
|---|---|
| spec completed, title resolved | `nightly backfill: rebuild fact index — completed` |
| spec failed, no title resolvable | `Unnamed job — failed` |
| spec canceled | `Unnamed job — canceled` |
| task blocked | `nightly-ingest — needs attention` |
| task starved | `weekly-report — stalled` |
| label is a path (`/Users/…/id_rsa`) | `Unnamed job — needs attention` |
| label is a token (`ghp_…`) | `Unnamed job — needs attention` |

In every case the internal key (`boi-spec-watch:…`), the raw spec/task id, the
`blocked_reason`, and any path/credential are absent from the push body; they
remain in the first-party email body and local stderr/telemetry.

## Verification log

Environment: the inherited `CARGO_TARGET_DIR` (unchanged — not set by this
work). `PATH` prefixed with `/opt/homebrew/bin`. Commands run from
`system/harness/`. All commands run in the foreground; the counts and exit codes
below are from real runs.

- `cargo test --lib boi_spec_watch::` — **35 passed, 0 failed** (exit 0).
  Includes the 8 `red_*` tests (now GREEN), the diagnostic-reason canary, and
  the 4 execute-phase GROUP 3 unit tests
  (`spec_title_lookup_prefers_current_version_snapshot`,
  `spec_title_lookup_missing_or_untitled_is_none`,
  `parse_spec_title_handles_malformed_and_typed_edges`,
  `display_name_sanitizes_all_edges`).
- `cargo test --lib alert::` — **11 passed, 0 failed** (exit 0). The
  generic-payload privacy test
  `push_body_contains_no_path_email_or_personal_tokens` still passes — the
  shared `push_body` is unchanged.
- `cargo fmt --check` — clean, exit 0.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — exit 0
  (clean). `truncate_name` builds the bounded string char-by-char rather than
  slicing, satisfying `clippy::string_slice`.
- `cargo test --workspace --all-targets --locked` — exit 0. Aggregated across
  the 40 test binaries: **1232 passed, 0 failed, 8 ignored**; every suite
  reported `0 failed` (the 8 ignored are pre-existing `#[ignore]` tests,
  unrelated to this change).

No claim above is for a command that was not actually run. No model-facing or
production credentials were used. The live `~/.boi/v2/boi.db` was read
READ-ONLY, in an ad-hoc `sqlite3` shell, purely to confirm the schema DDL and
the `snapshot` JSON shape. Worker tests pass explicit fixture or missing paths
and never read the live database.

## Deployment boundary

This task implements the name-resolution and rendering seam and the doc.
Per the task contract, a routine local merge to `develop` is authorized after
the required gates pass; this worker does not push branches, bump versions, run
a release, restart services, or deploy. Root handles remote verification and
deployment.
