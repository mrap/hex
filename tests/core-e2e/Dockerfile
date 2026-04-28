# tests/core-e2e/Dockerfile
# Comprehensive core E2E test image. Covers all hex primitives:
# messaging, events, assets, SSE, CLI subcommands, telemetry.
#
# Adds Python 3, SQLite CLI, and curl on top of the base agent-harness pattern.

# ── Stage 1: Build ───────────────────────────────────────────────────────────
FROM rust:latest AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
# version.txt is the source of truth; build.rs reads it via HEX_DIR
COPY version.txt /hex-root/version.txt
COPY system/harness/ ./
ENV HEX_DIR=/hex-root
RUN cargo build --release 2>&1

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    bash \
    python3 \
    sqlite3 \
    curl \
    && rm -rf /var/lib/apt/lists/*

ENV HOME=/home/testuser
RUN useradd -m -s /bin/bash testuser
USER testuser

# Workspace skeleton required by get_hex_dir()
RUN mkdir -p \
    $HOME/hex/projects \
    $HOME/hex/.hex/bin \
    $HOME/hex/.hex/data \
    $HOME/hex/.hex/audit \
    $HOME/hex/.hex/telemetry \
    $HOME/hex/.hex/reference/core-agents \
    $HOME/.hex-events/policies \
    $HOME/hex/.hex-events/policies \
    $HOME/hex/system/sse/topics \
    $HOME/hex/system/scripts/bin \
    $HOME/hex/system/skills/memory/scripts \
    $HOME/hex/tests/core-e2e/suites

# CLAUDE.md is required by get_hex_dir() to validate the workspace
RUN touch $HOME/hex/CLAUDE.md

# version.txt — needed by 'hex version' and test-cli's version-consistency check
COPY version.txt $HOME/hex/version.txt

# Binary and backward-compat symlink
COPY --from=builder /build/target/release/hex $HOME/hex/.hex/bin/hex
RUN ln -s $HOME/hex/.hex/bin/hex $HOME/hex/.hex/bin/hex-agent

# SSE topic manifests (server loads from $HEX_DIR/system/sse/topics/)
COPY system/sse/topics/ $HOME/hex/system/sse/topics/

# Integration, doctor, and memory scripts (hex delegates to these via bash/python3)
COPY system/scripts/hex-integration $HOME/hex/system/scripts/hex-integration
COPY system/scripts/hex-doctor      $HOME/hex/system/scripts/hex-doctor
COPY system/scripts/bin/            $HOME/hex/system/scripts/bin/
COPY system/skills/memory/scripts/  $HOME/hex/system/skills/memory/scripts/

# E2E sample policy: fires on test.ping (marker file); fires on test.critical
# only when payload.level == "critical". Used by test-events.sh.
RUN printf '%s\n' \
    'name: e2e-test' \
    'description: "E2E test policy — do not use in production"' \
    'enabled: true' \
    'rules:' \
    '  - name: ping-handler' \
    '    trigger:' \
    '      event: test.ping' \
    '    actions:' \
    '      - type: shell' \
    '        command: touch /tmp/e2e-policy-fired' \
    '        timeout: 5' \
    '  - name: critical-handler' \
    '    trigger:' \
    '      event: test.critical' \
    '    conditions:' \
    '      - field: payload.level' \
    '        op: eq' \
    '        value: critical' \
    '    actions:' \
    '      - type: shell' \
    '        command: touch /tmp/e2e-critical-fired' \
    '        timeout: 5' \
    > $HOME/.hex-events/policies/e2e-test.yaml \
    && cp $HOME/.hex-events/policies/e2e-test.yaml \
          $HOME/hex/.hex-events/policies/e2e-test.yaml

# Core E2E test suite (helpers.sh, run-all.sh, suites/*.sh written by later tasks)
COPY tests/core-e2e/ $HOME/hex/tests/core-e2e/

ENV HEX_DIR=$HOME/hex
ENV PATH="$HOME/hex/.hex/bin:$PATH"
WORKDIR $HOME/hex
ENTRYPOINT ["bash", "tests/core-e2e/run-all.sh"]
