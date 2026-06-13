# tests/core-e2e/Dockerfile
# Comprehensive core E2E test image. Covers all hex primitives:
# messaging, events, assets, SSE, CLI subcommands, telemetry.
#
# Adds Python 3, SQLite CLI, and curl on top of the base agent-harness pattern.

# ── Stage 0: cargo-chef base ─────────────────────────────────────────────────
# cargo-chef splits dependency compilation into its own cacheable layer keyed
# ONLY on the dependency graph (recipe.json ≈ Cargo.lock), not on our source.
# With buildx `--cache-to type=gha,mode=max` (see .github/workflows/core-e2e.yml)
# the `cook` layer persists across CI runs, so the expensive cold compile of the
# iii engine git deps + rusqlite's bundled SQLite (~8-11 min) is skipped on every
# run where deps are unchanged — only our changed source recompiles (~1-2 min).
FROM rust:1.96 AS chef
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config git \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-chef --locked
WORKDIR /build

# ── Stage 1: Plan ────────────────────────────────────────────────────────────
# Generate recipe.json from the full manifest graph. The path dep scipd
# (../code-intel → /code-intel) must be present for `prepare` to read its
# manifest; cargo-chef captures it (and its deps) into the recipe.
FROM chef AS planner
COPY system/harness/ ./
COPY system/code-intel/ /code-intel/
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 2: Build ───────────────────────────────────────────────────────────
FROM chef AS builder
# Cook deps only — this layer is the cache target. The scipd path dep lives at
# /code-intel (a sibling, outside /build), which cargo-chef can NOT reconstruct
# from the recipe, so the real crate must be present for cook to resolve it. It
# is copied BEFORE cook, so the cook layer keys on (recipe.json + code-intel);
# editing the harness source — the common case — leaves the warm cache intact,
# and code-intel changes rarely.
COPY --from=planner /build/recipe.json recipe.json
COPY system/code-intel/ /code-intel/
RUN cargo chef cook --release --recipe-path recipe.json
# Real harness source over the warmed dep cache: only our crates recompile.
COPY system/harness/ ./
RUN cargo build --release 2>&1
# Stamp the Cargo.toml version for runtime consistency checks (source of truth)
RUN grep '^version' ./Cargo.toml | head -1 | cut -d'"' -f2 > /build/compiled-version.txt

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
    $HOME/hex/.hex/scripts \
    $HOME/hex/tests/core-e2e/suites

# CLAUDE.md is required by get_hex_dir() to validate the workspace
RUN touch $HOME/hex/CLAUDE.md

# Binary and backward-compat symlink
COPY --from=builder /build/target/release/hex $HOME/hex/.hex/bin/hex
# Compiled-version stamp for cli-version-consistency test
COPY --from=builder /build/compiled-version.txt $HOME/hex/.hex/hex-version.txt
RUN ln -s $HOME/hex/.hex/bin/hex $HOME/hex/.hex/bin/hex-agent

# Integration script (hex delegates to this via bash).
# Binary looks for it at $HEX_DIR/.hex/scripts/ (runtime install path).
# `hex doctor` is now a native harness subcommand (system/harness/src/doctor) — no script.
COPY system/scripts/hex-integration $HOME/hex/.hex/scripts/hex-integration

# Core E2E test suite (helpers.sh, run-all.sh, suites/*.sh written by later tasks)
COPY tests/core-e2e/ $HOME/hex/tests/core-e2e/

ENV HEX_DIR=$HOME/hex
ENV PATH="$HOME/hex/.hex/bin:$PATH"
WORKDIR $HOME/hex
ENTRYPOINT ["bash", "tests/core-e2e/run-all.sh"]
