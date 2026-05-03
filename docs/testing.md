# hex-foundation — Test Matrix

This document describes the test suite, what each test verifies, and how to run it locally.

## Test categories

| Category | Files | Needs API key |
|----------|-------|:-------------:|
| Static / unit | `test_skill_frontmatter.sh`, `test_skill_refs.sh`, `test_path_mapping.bats`, `test_hex_doctor_version_sync.bats`, `test_hex_doctor_hex_binary_version_sync.bats`, `test-upgrade-deletion.sh`, `test-upgrade-binary-swap.sh` | No |
| Core E2E (containerized) | `tests/core-e2e/run-all.sh` | BOI suites only |
| Live eval — Claude Code | `test_skill_discovery.sh`, `test_e2e.sh`, `test_fullstack.sh` | Yes |
| Live eval — Codex | `test_skill_discovery_codex.sh`, `test_codex_onboarding.sh` | Yes |
| Codex parity (containerized) | `tests/codex-parity/run-all.sh` | No (structural); `OPENAI_API_KEY` for live |
| Migration | `tests/migrate/test-migrate.sh` | No |
| Memory | `test_memory.py` | No |

## Core E2E suite (`tests/core-e2e/`)

Auto-discovers all `tests/core-e2e/suites/*.sh` files and runs them. Non-BOI suites run inside the `tests/core-e2e/Dockerfile` container; BOI integration suites run on the host (they need Docker access to spin up their own containers).

CI runs both jobs on every PR and blocks merges on failure (see `.github/workflows/core-e2e.yml`).

```bash
# All suites (host must have Docker)
bash tests/core-e2e/run-all.sh

# Filter by pattern — useful when iterating on a specific suite
bash tests/core-e2e/run-all.sh --include boi          # BOI suites only
bash tests/core-e2e/run-all.sh --exclude boi          # skip BOI (e.g. inside Docker)
bash tests/core-e2e/run-all.sh --include 'install|upgrade'  # regex match on suite name
```

Current suites:

| Suite | What it verifies |
|-------|-----------------|
| `test-boi-install` | Fresh BOI install: binary builds, `--help`/`--version`, smoke dispatch |
| `test-boi-upgrade` | Upgrade path: version bump, stale-symlink detection, doctor catches dangling link |
| `test-assets` | Asset registry CRUD via `hex asset` subcommands |
| `test-cli` | All `hex` subcommands reachable; version matches `Cargo.toml` |
| `test-events` | Event emit, policy firing, trace via `hex events` |
| `test-messaging` | Message send/receive/filter with SQLite verification |
| `test-sse` | SSE subscribe/publish, topic filtering, heartbeat |
| `test-telemetry` | Telemetry JSONL written to `.hex/telemetry/` |
| `test-doctor-events-coverage` | `hex-doctor` fails loudly on broken policies (parse errors named), passes on valid policies |

## Tests added in v0.2.4

### `tests/test_skill_frontmatter.sh`

Validates every `system/skills/*/SKILL.md` without running any agent. Checks:

- Frontmatter block exists at the top of the file.
- `name` field is present and matches the skill directory name.
- `description` field is present and non-empty.
- If `allowed-tools` is present, it is a YAML list of strings.

Exit 0 = all valid. Exit 1 = summary of failures.

### `tests/test_skill_refs.sh`

Installs hex to a temp dir and verifies that every path reference inside SKILL.md files resolves on disk. Catches broken references to scripts, templates, or commands before they reach users.

### `tests/test_skill_discovery.sh`

Runs Claude Code in `--print` mode inside a fresh hex install and asserts:

1. All 11 shipped skills appear in Claude's response to a discovery prompt.
2. At least 3 skills (`/hex-doctor`, `/hex-decide`, `/hex-triage`) can be invoked without crashing.

Requires `~/.hex-test.env` with `ANTHROPIC_API_KEY`.

### `tests/test_skill_discovery_codex.sh`

Mirror of the above for Codex. Because Codex reads `AGENTS.md` rather than `SKILL.md` files directly, this test verifies that the 11 skill names surface via `AGENTS.md` context and that Codex can perform the same three invocations.

## Codex parity suite (`tests/codex-parity/`)

Seven tests that verify behavioral parity between the Claude Code and Codex runtimes. Runs inside a Docker container with Node.js + Codex CLI installed. Structural tests run without an API key; live-dispatch tests are skipped automatically when `OPENAI_API_KEY` is absent.

```bash
bash tests/codex-parity/run-all.sh
```

| Test | What it verifies | API key |
|------|-----------------|:-------:|
| `test-install-shape.sh` | Fresh hex install produces `.hex/scripts/`, `.hex/skills/`, `.hex/bin/`, `CLAUDE.md`, `AGENTS.md` | No |
| `test-agents-md-complete.sh` | `AGENTS.md` covers all sections present in `CLAUDE.md` | No |
| `test-skill-discovery.sh` | All skills are discoverable from `.hex/skills/*/SKILL.md` under Codex | No |
| `test-doctor-codex.sh` | `doctor.sh` includes and passes the Codex CLI check | No |
| `test-upgrade-codex.sh` | `upgrade.sh` preserves `AGENTS.md` user customizations | No |
| `test-boi-dispatch-codex.sh` | Minimal spec with `runtime=codex` completes and produces output | Yes |
| `test-memory-search.sh` | Memory search index and CLI work identically under the Codex runtime | No |

Gate 5 in `system/scripts/release.sh` runs this suite and blocks on failure; structural tests always run, live tests are skipped when no key is present.

## Running locally

### Prerequisites

- Docker (for Docker eval suite)
- [Tart](https://github.com/cirruslabs/tart) (for macOS eval suite — Apple Silicon only)
- `~/.hex-test.env` containing at minimum:

  ```
  ANTHROPIC_API_KEY=sk-ant-...
  ```

### Static tests (no API key)

```bash
cd /path/to/hex-foundation

bash tests/test_skill_frontmatter.sh
bash tests/test_skill_refs.sh
bash tests/migrate/test-migrate.sh
python3 tests/test_memory.py
bats tests/test_hex_doctor_version_sync.bats
bats tests/test_hex_doctor_hex_binary_version_sync.bats
bash tests/test-upgrade-deletion.sh
bash tests/test-upgrade-binary-swap.sh
```

### Full Docker eval suite

```bash
bash tests/eval/run_eval_docker.sh --live
```

Individual cases:

```bash
bash tests/eval/run_eval_docker.sh --live --case skill-frontmatter
bash tests/eval/run_eval_docker.sh --live --case skill-refs
bash tests/eval/run_eval_docker.sh --live --case skill-discovery
bash tests/eval/run_eval_docker.sh --live --case skill-discovery-codex
```

### Full macOS Tart eval suite

```bash
bash tests/eval/run_eval_macos.sh
```

## Shipped skills (as of v0.2.4)

The 11 skills installed under `.hex/skills/` (verified by `test_skill_discovery.sh`):

1. `hex-startup`
2. `hex-checkpoint`
3. `hex-shutdown`
4. `hex-reflect`
5. `hex-consolidate`
6. `hex-debrief`
7. `hex-decide`
8. `hex-triage`
9. `hex-doctor`
10. `landings`
11. `memory`
