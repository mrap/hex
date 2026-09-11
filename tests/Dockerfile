# Stage 1: Build hex binary from source (rusqlite "bundled" needs gcc, present in rust:slim;
# openssl-sys needs pkg-config + libssl-dev, NOT in rust:slim by default)
FROM rust:1-slim AS hex-builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev g++ && rm -rf /var/lib/apt/lists/*
WORKDIR /build
# The harness depends on the sibling system/code-intel crate by path
# (scipd = { path = "../code-intel" }) — copying only system/harness/
# breaks the in-container build (deterministic gate RED, 2026-06-11).
# Copy the workspace manifests + both members and build by -p.
COPY Cargo.toml Cargo.lock ./
COPY system/harness/ system/harness/
COPY system/code-intel/ system/code-intel/
# Shared Rust module and its compile-time embedded scripts.
COPY system/managed_cargo_bridge.rs system/managed_cargo_bridge.rs
COPY system/scripts/managed-cargo-gate.py system/scripts/managed-target-check.py system/scripts/
RUN cargo build -p hex-harness 2>&1

# Stage 2: Test runner
FROM rust:1-slim

RUN apt-get update && apt-get install -y --no-install-recommends git python3 python3-pip && \
    rm -rf /var/lib/apt/lists/* && \
    pip install --no-cache-dir --break-system-packages pytest

# Git needs a user for commits (not used, but prevents warnings)
RUN git config --global user.email "test@test.com" && \
    git config --global user.name "Test"

# Install hex binary so tests can drive native commands
COPY --from=hex-builder /build/target/debug/hex /usr/local/bin/hex

# OBS-019 fix: cap ONNX Runtime threads at process startup. ORT reads
# these at dylib load — setting them from inside the binary via
# std::env::set_var is too late. Without the cap, a 70-chunk embed call
# allocates per-thread × per-layer activations that OOM in the 4 GB
# Docker Desktop VM. See evolution/obs-019-diagnosis.md.
ENV ORT_NUM_THREADS=1 OMP_NUM_THREADS=1

COPY . /tmp/hex-setup
COPY tests/test_e2e.sh /test_e2e.sh
RUN chmod +x /test_e2e.sh

CMD ["/test_e2e.sh"]
