# tests/harness-e2e/Dockerfile
#
# Containerized E2E test image for the `hex harness` runtime.
# Proves the at-most-once lifecycle (graceful drain + outbox replay) against a
# LIVE iii engine — which is now BAKED INTO the hex binary (mrap/hex-iii fork).
#
# VENDORLESS: there is no separate `iii` engine binary. `hex harness serve`
# starts the engine in-process, so the image needs ONLY `hex`. (This replaces
# the old vendored-iii model where the host pre-staged ~/.local/bin/iii.)
#
# Build from the repo root so the COPY paths resolve:
#   docker build -f tests/harness-e2e/Dockerfile -t hex-harness-e2e .
#   docker run --rm hex-harness-e2e

# ── Stage 0: cargo-chef base ─────────────────────────────────────────────────
# cargo-chef isolates dependency compilation (iii engine git deps + rusqlite's
# bundled SQLite) into a layer keyed on recipe.json ≈ Cargo.lock. With buildx
# `--cache-to type=gha,mode=max` (see .github/workflows/core-e2e.yml) that layer
# persists across CI runs — ephemeral runners get a warm dep cache, so only our
# changed source recompiles instead of the full cold ~8-11 min build.
FROM rust:1.96 AS chef
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config git \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-chef --locked
WORKDIR /build

# ── Stage 1: Plan ────────────────────────────────────────────────────────────
FROM chef AS planner
COPY system/harness/ ./
# Sibling path dep: scipd = { path = "../code-intel" } resolves to /code-intel
# when the harness manifest is at /build. Present so `prepare` reads its manifest.
COPY system/code-intel/ /code-intel/
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 2: Build the hex binary (engine compiled in via the git dep) ───────
FROM chef AS builder
# Cook deps only — the cache target. Debug profile keeps the compile fast; the
# release profile is verified separately on the host. No `--mount=type=cache`:
# the gha backend persists LAYERS, not cache mounts, so the registry + target
# must land in real layers for the warm dep cache to survive ephemeral runners.
# The scipd path dep lives at /code-intel (a sibling, outside /build), which
# cargo-chef can NOT reconstruct from the recipe, so the real crate is copied
# BEFORE cook to resolve it. The cook layer keys on (recipe.json + code-intel);
# editing the harness source — the common case — leaves the warm cache intact.
COPY --from=planner /build/recipe.json recipe.json
COPY system/code-intel/ /code-intel/
RUN cargo chef cook --recipe-path recipe.json
# Real harness source over the warmed dep cache: only our crates recompile. The
# binary is copied OUT to /hex-bin (a real layer) for the runtime stage to grab.
COPY system/harness/ ./
RUN cargo build 2>&1 && cp /build/target/debug/hex /hex-bin

# ── Stage 2: Runtime — ONLY the hex binary, nothing vendored ─────────────────
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    bash coreutils procps \
    && rm -rf /var/lib/apt/lists/*

ENV HOME=/home/testuser
RUN useradd -m -s /bin/bash testuser

# The hex binary IS the engine + harness — no vendored iii.
COPY --from=builder /hex-bin /usr/local/bin/hex

# Lifecycle test script
COPY tests/harness-e2e/test-harness-lifecycle.sh /usr/local/bin/test-harness-lifecycle.sh
RUN chmod +x /usr/local/bin/test-harness-lifecycle.sh

# Hex workspace skeleton
RUN mkdir -p \
        $HOME/hex/.hex/harness \
        $HOME/hex/.hex/bin \
    && touch $HOME/hex/CLAUDE.md \
    && chown -R testuser:testuser $HOME/hex

USER testuser
ENV HEX_DIR=$HOME/hex
# Register the test-only hex-e2e worker (gated in workers::registry()).
ENV HEX_HARNESS_E2E=1
# The baked-in engine's default config tries to spawn the external iii-worker-ops
# daemon if it's on PATH; it isn't here, but disable explicitly for determinism.
ENV IIIWORKER_DISABLE_BUILTIN_DAEMONS=1
ENV PATH="/usr/local/bin:$PATH"
WORKDIR $HOME/hex

ENTRYPOINT ["bash", "/usr/local/bin/test-harness-lifecycle.sh"]
