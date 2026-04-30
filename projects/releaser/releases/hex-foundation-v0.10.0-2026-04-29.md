# hex-foundation v0.10.0 — BOI v1.1.0 integration + containerized BOI E2E

**Date:** 2026-04-30
**Tag:** v0.10.0
**Base:** v0.9.0 (1f1ac83) + test/boi-integration-e2e-coverage (7b1c016)

---

## BOI Release Evidence

- **Tag:** v1.1.0 (56ceccc) — created 2026-04-30T01:02:00Z
- **Build:** `cargo build --release` CLEAN (boi v1.1.0)
- **Tests:** 194 passed; 0 failed (`cargo test --lib`)
- **Verification:**
  - `boi --version` → `boi 1.1.0` ✓
  - `boi dashboard --help` → `Launch interactive TUI dashboard / Usage: boi dashboard` ✓
  - `boi status` → RUNNING queue with active specs ✓
- **Branches merged into BOI main:** pipeline-v2 (2ace678), SBF49, SDA0F
- **Excluded:** SD979 (batched dequeue, in-flight), DAG reassess (C&C failure)

## hex-foundation Changes

### Commits included
- `7b1c016` — test: containerized BOI integration coverage (install / upgrade / doctor)
- merge: containerized BOI E2E coverage from test branch
- bump: BOI v1.1.0 (pipeline-v2 + dashboard + spec-quality phases)
- bump: version 0.9.0 → 0.10.0

### Files changed
- `VERSIONS` — BOI_VERSION v1.0.0 → v1.1.0
- `system/version.txt` — 0.9.0 → 0.10.0
- `README.md` — v0.10.0 roadmap entry added
- `tests/core-e2e/suites/test-boi-install.sh` — containerized fresh install suite
- `tests/core-e2e/suites/test-boi-upgrade.sh` — containerized upgrade suite (catches stale-symlink)
- `.github/workflows/core-e2e.yml` — CI gate for E2E suites
- `system/scripts/doctor.sh` — +127 lines of runtime BOI health checks
- `CONTRIBUTING.md` — E2E coverage mandate for new features
- `docs/testing.md` — testing architecture documentation
- `tests/test_doctor.bats` — unit tests for doctor BOI checks
- `tests/core-e2e/README.md` — E2E test documentation

## Known Issues (deferred)

- BOI v1.1.0 ships OpenRouter runtime code but it's not yet honored at dispatch (runtime field dropped between registry and runner). Tracked.
- Daemon batched dequeue + SIGHUP hot-reload not in this release (in-flight as SD979).

## Sentinel Review

Awaiting Sentinel PASS — mechanical gate before release.sh.

## Release Pipeline

[ ] Sentinel PASS received
[ ] release.sh executed — Docker E2E PASS, gates clean
[ ] Tag v0.10.0 pushed to origin
[ ] GitHub Release created
[ ] Brand Lead notified
