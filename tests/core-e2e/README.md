# core-e2e — hex Foundation End-to-End Test Suite

All tests in this directory are **containerized** (Docker). They must never assume a
pre-installed `hex` or `boi` binary on the host.

## Directory Layout

```
tests/core-e2e/
├── Dockerfile          # Base image for hex-specific suites (hex binary, sqlite3, python3)
├── helpers.sh          # Shared assertion helpers — source this in every suite
├── run-all.sh          # Suite discovery + runner
├── gap-analysis.md     # Living coverage audit document
└── suites/             # One file per test suite
    ├── test-boi-install.sh
    ├── test-boi-upgrade.sh
    ├── test-cli.sh
    ├── test-events.sh
    ├── test-messaging.sh
    ├── test-sse.sh
    └── test-telemetry.sh
```

## Running Tests

```bash
# All suites
bash tests/core-e2e/run-all.sh

# Only BOI suites (host with Docker, no hex binary needed)
bash tests/core-e2e/run-all.sh --include boi

# Skip BOI suites (inside a container that has hex but no Docker)
bash tests/core-e2e/run-all.sh --exclude boi

# Pattern matching (grep -E)
bash tests/core-e2e/run-all.sh --include 'install|upgrade'

# Single suite directly
bash tests/core-e2e/suites/test-boi-install.sh
```

## Writing a Suite

### 1. Source helpers.sh

Every suite must conditionally source `helpers.sh` so it works both standalone and
when sourced by `run-all.sh`:

```bash
#!/usr/bin/env bash
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! declare -f assert_pass >/dev/null 2>&1; then
    source "$SCRIPT_DIR/../helpers.sh"
fi
```

### 2. Use the assertion helpers

```bash
assert_pass "description"                           # +1 to PASS counter, green ✓
assert_fail "description"                           # +1 to FAIL counter, red ✗
assert_exit 0 "$?" "boi --help exits 0"            # compare exit codes
assert_contains "$output" "pattern" "description"  # grep -q check
assert_not_contains "$output" "bad" "description"  # absence check
assert_file_exists "/path/to/file" "description"   # -f check
assert_dir_exists  "/path/to/dir"  "description"   # -d check
```

### 3. Containerize using a heredoc inner script

BOI suites (and any suite that tests install/upgrade) spin up a Docker container.
The pattern:

```bash
IMAGE_TAG="hex-boi-install-test:latest"

# Build image if absent
if ! docker image inspect "$IMAGE_TAG" >/dev/null 2>&1; then
    docker build -t "$IMAGE_TAG" - << 'DOCKERFILE_EOF'
FROM rust:latest
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl ca-certificates python3 sqlite3 && rm -rf /var/lib/apt/lists/*
RUN useradd -m -s /bin/bash testuser
USER testuser
WORKDIR /home/testuser
DOCKERFILE_EOF
fi

# Inner script (single-quoted heredoc — no host expansion)
read -r -d '' INNER_SCRIPT << 'INNER_EOF' || true
set -uo pipefail
PASS=0; FAIL=0
pass() { PASS=$((PASS+1)); echo "  PASS: $*"; }
fail() { FAIL=$((FAIL+1)); echo "  FAIL: $*"; }
# ... test body ...
exit "${FAIL}"
INNER_EOF

# Run the container, capture log
CONTAINER_LOG="/tmp/suite-name-$$.log"
docker run --rm \
    -v "${REPO_ROOT}:/repo:ro" \
    -e HOME=/home/testuser \
    "$IMAGE_TAG" bash -c "$INNER_SCRIPT" > "$CONTAINER_LOG" 2>&1
CONTAINER_EXIT=$?
cat "$CONTAINER_LOG"

# Merge inner PASS/FAIL into outer counters
INNER_PASS=$(grep -c "^  PASS:" "$CONTAINER_LOG" 2>/dev/null || true)
INNER_FAIL=$(grep -c "^  FAIL:" "$CONTAINER_LOG" 2>/dev/null || true)
PASS=$((PASS + ${INNER_PASS:-0}))
FAIL=$((FAIL + ${INNER_FAIL:-0}))
rm -f "$CONTAINER_LOG"
```

Key points:
- Use a **single-quoted** heredoc (`<< 'INNER_EOF'`) so no host-side variable expansion happens.
- Inside the container, use `pass`/`fail` functions (not `assert_pass`/`assert_fail`) because
  `helpers.sh` is not sourced inside the container. The outer script merges the counters.
- Always print `PASS:` / `FAIL:` prefixed lines so `grep -c` can count them reliably.
- Clean up state inside the container's `on_exit` trap so reruns are clean.

### 4. Failure must be diagnosable

When a containerized suite fails, dump enough context to diagnose without re-running:

```bash
on_exit() {
    if [ "${FAIL:-0}" -gt 0 ]; then
        echo "--- boi daemon log (last 50 lines) ---"
        find "$HOME/.boi/logs" -name "*.log" 2>/dev/null \
            | xargs -r tail -n 50 2>/dev/null || true
        echo "--- install.sh output (last 50 lines) ---"
        tail -50 /tmp/install.log 2>/dev/null || true
    fi
    rm -rf "$HOME/.boi" /tmp/hex
    exit "${FAIL}"
}
trap on_exit EXIT
```

### 5. Optional steps guarded by env vars

Steps that require credentials (e.g., `ANTHROPIC_API_KEY` for smoke dispatch) must be
skipped gracefully when the key is absent:

```bash
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "  (ANTHROPIC_API_KEY not set — skipping smoke dispatch)"
    exit "${FAIL}"
fi
# ... dispatch steps ...
```

This keeps the suite non-blocking in CI environments that don't have the key, while
still exercising the full path when it is available.

## Suite Reference

### test-boi-install.sh

Tests a complete fresh install of BOI in a clean Ubuntu + Rust container.

Key assertions:
- `~/.boi/bin/boi` exists and is executable (symlink target verified if applicable)
- `boi --help` exits 0 and lists `dispatch`, `status`, `bench`, `cancel`
- `boi version` matches `VERSIONS BOI_VERSION`
- Wrapper chain `boi.sh --help` exits 0
- (Optional) Smoke spec dispatches, completes, and marker file exists

### test-boi-upgrade.sh

Tests upgrading BOI from a prior tagged release to HEAD. Catches the stale-symlink
class of bugs (new subcommands absent because the binary was not rebuilt).

Key assertions:
- Baseline: install at v0.7.0, capture version + mtime
- After upgrade: version bumped, binary mtime strictly newer, `bench` listed in `--help`
- Smoke dispatch still works after upgrade
- BAD case: corrupted symlink is detected by `doctor.sh` runtime check (not just file existence)

### Other suites (test-cli, test-messaging, test-events, test-sse, test-telemetry)

These test the compiled `hex` binary directly inside the `Dockerfile` image. They do not
spin up nested Docker — the suite itself runs inside the container via `run-all.sh --exclude boi`
or the CI workflow.

## Image Caching

BOI suites share a single image tag `hex-boi-install-test:latest`. The image is built
automatically on first run and reused on subsequent runs. To force a rebuild:

```bash
docker rmi hex-boi-install-test:latest
bash tests/core-e2e/suites/test-boi-install.sh
```

## CI Integration

The GitHub Actions workflow at `.github/workflows/core-e2e.yml` runs all suites on
every PR and blocks merge on failure. BOI suites run on the host (they need Docker);
hex binary suites run inside the `Dockerfile` container.
