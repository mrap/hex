FROM rust:latest AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY system/harness/ ./
# Sibling path dep: scipd = { path = "../code-intel" } resolves to /code-intel
# when the harness manifest is at /build. Copy it or the build can't resolve it.
COPY system/code-intel/ /code-intel/
# Shared Rust module and its compile-time embedded scripts.
COPY system/managed_cargo_bridge.rs /managed_cargo_bridge.rs
COPY system/scripts/managed-cargo-gate.py system/scripts/managed-target-check.py /scripts/
RUN cargo build --release 2>&1

FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    bash \
    && rm -rf /var/lib/apt/lists/*

ENV HOME=/home/testuser
RUN useradd -m -s /bin/bash testuser
USER testuser

RUN mkdir -p $HOME/hex/projects $HOME/hex/.hex/bin $HOME/hex/.hex/audit \
    $HOME/hex/.hex/cost $HOME/hex/.hex/messages \
    $HOME/hex/.hex/reference/core-agents
RUN touch $HOME/hex/CLAUDE.md

COPY --from=builder /build/target/release/hex $HOME/hex/.hex/bin/hex
RUN ln -s $HOME/hex/.hex/bin/hex $HOME/hex/.hex/bin/hex-agent
COPY tests/agent-harness/e2e-tests.sh $HOME/hex/e2e-tests.sh

ENV HEX_DIR=$HOME/hex
WORKDIR $HOME/hex
ENTRYPOINT ["bash", "e2e-tests.sh"]
