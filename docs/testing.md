# hex-foundation — Test Matrix

This document describes the test suite, what each test verifies, and how to run it locally.

## Test categories

| Category | Files | Needs API key |
|----------|-------|:-------------:|
| Static / unit | `test_skill_frontmatter.sh`, `test_skill_refs.sh`, `test_path_mapping.bats` | No |
| Core E2E (containerized) | `tests/core-e2e/run-all.sh` | BOI suites only |
| Live eval — Claude Code | `test_skill_discovery.sh`, `test_e2e.sh`, `test_fullstack.sh` | Yes |
| Live eval — Codex | `test_skill_discovery_codex.sh`, `test_codex_onboarding.sh` | Yes |
| Codex parity (containerized) | `tests/codex-parity/run-all.sh` | No (structural); `OPENAI_API_KEY` for live |
| Migration | `tests/migrate/test-migrate.sh` | No |
| Memory | `test_memory.py` | No |

## Core E2E suite (`tests/core-e2e/`)

Auto-discovers all `tests/core-e2e/suites/*.sh` files and runs them. Non-BOI suites run inside the `tests/core-e2e/Dockerfile` container; BOI integration suites run on the host (they need Docker access to spin up their own containers).

The blocking merge gate is `.github/workflows/ci.yml`, which runs `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`, and `cargo test --workspace` on every push to develop/main and on every pull request. The Core E2E workflow (`.github/workflows/core-e2e.yml`) runs these suites on every PR too, but it is advisory/non-blocking — not a required status check — exactly as its own header comment states.

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
| `test-cli` | All `hex` subcommands reachable; version matches `Cargo.toml` |
| `test-messaging` | Message send/receive/filter with SQLite verification |
| `test-doctor` | `hex-doctor` passes on healthy install, fails loudly on broken config |

## Container test lane (`system/scripts/test-lane.sh`)

The lane runs the workspace test suite with `cargo nextest` inside the `tests/lane/Dockerfile` image. It mounts the current worktree at `/work` and the named Docker volume `boi-target` at `/target`. Source and target paths are the same for every worktree, so cargo reuses compiled artifacts across worktrees. A second run with no source change compiles zero crates.

```bash
# Build the image if needed, then run the whole workspace
bash system/scripts/test-lane.sh

# Skip the image build, pass args to nextest
bash system/scripts/test-lane.sh --no-build -- -p hex-harness
```

The script prints one receipt JSON line to stdout and everything else to stderr. Receipt fields: `schema`, `tree_hash`, `command`, `exit_code`, `crates_compiled`, `duration_secs`, `image`, `volume`, `started_at`. The exit code is the nextest exit code. If Docker is missing or not running, the script prints the reason and exits 2 with no receipt.

`hex release cut` runs its `tests` gate through the lane when the script exists in the repo. Set `HEX_TEST_LANE=host` to force the host route (`cargo test --workspace` through managed Cargo) instead.

The `hex-build-cache-guard` harness worker runs hourly at :15. It reads `cargo_target_dir` from `~/.boi/v2/daemon.toml`, deletes `.o` files older than 60 minutes in `<target>/debug/deps`, and fails loudly when more than 25,000 entries remain.

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

1. All currently shipped skills appear in Claude's response to a discovery prompt (session-lifecycle skills `hex-startup`, `hex-checkpoint`, `hex-shutdown`, `hex-reflect` were demolished and must not be expected here).
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

The `codex-parity` gate in the `hex release cut` battery runs this suite and blocks the release on failure; structural tests always run, live tests are skipped when no key is present. The gate is skipped loudly when the directory is absent or `--skip-parity` (or `--skip-e2e`, which implies it) is passed.

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

## Shipped skills

The skills installed under `.hex/skills/` (verified by `test_skill_discovery.sh`).
Note: `hex-consolidate` was removed in favor of the `hex memory consolidate full|quick`
binary subcommand (the single consolidate surface — see `architecture.md`).
The session-lifecycle skills (`hex-startup`, `hex-checkpoint`, `hex-shutdown`,
`hex-reflect`, `hex-debrief`) were demolished — they are no longer shipped and
must not be re-added to this test plan.

1. `hex-decide`
2. `hex-triage`
3. `hex-doctor`
4. `landings`
5. `memory`
