# hex-foundation v0.13.2 — STAGED

**Status:** Awaiting Sentinel sign-off (SA-030)
**Staged:** 2026-05-06
**Commits:** ffc6f86..db56d3e (4 new commits over v0.13.1)

## What's in this release

### Fixed
- **session-start.sh**: Channel→topic checkpoint resume — `hex-<topic>` channels now surface `projects/<topic>/checkpoint.md` as additionalContext. Generalized `.hex/state/blockers/*.flag` scan (any flag file surfaces as a blocker). Topic-regex sanitization strips leading `#` from CC_SESSION_KEY.
- **hex-integration-check.sh**: `export _error_raw` bug — the `VAR=value CMD=$(...)` idiom did not propagate the env var into the command-substitution subshell. Caused 11,948+ events/day with `error: null`. Fixed. Emit-throttle added for persistent fail streaks (heartbeat every 60 consecutive checks).
- **memory_index.py**: Cascade-delete `vec_chunks` orphans on re-index. 82,377 orphan rows (58% of the vec table) had accumulated because FTS5 chunk deletion didn't cascade to the `vec0` virtual table.
- **memory_search.py**: `_rrf_merge` documented as FTS-only (KNOWN GAP). `--hybrid` was paying embedding+vec-query cost without fusing vec results into the score.
- **check-career-pipeline.sh**: Switched from broken `hex_events_cli.py status` grep to `load_policies`-based policy validation. Sanitize violations fixed (hardcoded `/Users/mrap` paths → `HEX_EVENTS_DIR` env-var; `mike@mrap.me` → `hex-test@example.com`).

### Changed
- **hex-doctor**: Two new health modules — Memory Vector Search (surfaces sqlite-vec drift) and hex-events Policy Load Errors (surfaces broken policies previously invisible to doctor).

## Commits (new since v0.13.1 / ffc6f86)
- `9e73ed6` docs: add v0.13.1 release notes (ffc6f86)
- `41b92c8` sync: session-start checkpoint resume + integration-check emit fix + memory leak fix (pre-v0.13.2)
- `315fc56` bump: v0.13.2
- `db56d3e` fix(sanitize): remove hardcoded /Users/mrap paths and mrap-specific identifier from check-career-pipeline.sh

## Gates
- [ ] Sentinel SA-030 sign-off
- [ ] release.sh (Docker E2E + gitleaks + PII scan + push + tag)
- [ ] GitHub Release
- [ ] Brand Lead notification
