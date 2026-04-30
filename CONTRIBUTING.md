# Contributing to hex-foundation

## BOI Integration Changes

**Any change that affects how BOI is installed, upgraded, or invoked requires a corresponding
E2E test update.**

Specifically: if your change touches any of the following, you must add a new suite under
`tests/core-e2e/suites/` or extend an existing one:

- `install.sh` — BOI clone/build/link steps
- `system/scripts/doctor.sh` — BOI health checks
- `VERSIONS` — `BOI_VERSION` or related version pins
- `~/.boi/` directory layout or binary paths
- Any new `boi` subcommand that callers depend on

### Why

The 2026-04-29 audit found that install.sh successfully built BOI but no test verified
the binary actually ran. This allowed a stale symlink to ship without the `bench` subcommand,
which was only caught manually in a live session. The rule: if the binary isn't exercised in
CI, the install isn't tested.

### What counts as sufficient coverage

A new E2E entry must cover at minimum:
1. The happy path (installs/runs correctly)
2. One failure mode (e.g., missing binary, stale symlink, wrong version)

See `tests/core-e2e/suites/test-boi-install.sh` and `test-boi-upgrade.sh` as reference
implementations.

### Running the BOI suites locally

```bash
# Requires Docker on the host
bash tests/core-e2e/run-all.sh --include boi

# Single suite
bash tests/core-e2e/suites/test-boi-install.sh

# With smoke dispatch (exercises dispatch/status/complete pipeline)
ANTHROPIC_API_KEY=<key> bash tests/core-e2e/suites/test-boi-install.sh
```

## General E2E Guidelines

- All foundation E2E tests are containerized — never run against the host environment.
- Use `tests/core-e2e/helpers.sh` assertion helpers (`assert_pass`, `assert_fail`,
  `assert_contains`, `assert_exit`, etc.) — do not invent parallel assertion patterns.
- Each suite is a standalone `.sh` file under `tests/core-e2e/suites/`. It must work
  both when sourced by `run-all.sh` (sharing global `PASS`/`FAIL` counters) and when
  executed directly (`bash suites/test-foo.sh`).
- Failure output must be actionable: dump the container log and relevant daemon log lines
  so a developer can diagnose without re-running interactively.
