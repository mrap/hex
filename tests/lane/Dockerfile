# tests/lane/Dockerfile
# Container test lane image. Runs the workspace test suite with cargo-nextest
# inside a container so every worktree shares one set of compiled artifacts.
#
# How the lane uses this image (see system/scripts/test-lane.sh):
#   - the worktree is bind-mounted at /work
#   - the named Docker volume `boi-target` is mounted at /target
#   - CARGO_TARGET_DIR=/target
# Source path and target path are the same for every worktree, so cargo
# fingerprints match and a second run with no source change compiles nothing.
#
# Layout under /work mirrors the repo. Do not flatten it like the core-e2e
# image does: the workspace manifest and the harness's path dependency on
# ../code-intel both need the real `system/...` layout, and the mounted
# worktree must land on the same paths the image was built with.

# Stage 0: cargo-chef base. cargo-chef splits dependency compilation into its
# own cacheable layer keyed on the dependency graph, not on our source.
FROM rust:1.96 AS chef
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config git \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-chef --locked
WORKDIR /work

# Stage 1: plan. Copy the workspace with its real layout and write recipe.json.
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY system/harness/ system/harness/
COPY system/code-intel/ system/code-intel/
COPY system/managed_cargo_bridge.rs system/managed_cargo_bridge.rs
COPY system/scripts/managed-cargo-gate.py system/scripts/managed-target-check.py system/scripts/
RUN cargo chef prepare --recipe-path recipe.json

# Stage 2: lane. Cook dependencies into /target with the test profile, then
# install cargo-nextest. The cooked /target is copied into the `boi-target`
# volume the first time Docker creates it, so the first lane run can start warm.
FROM chef AS lane
ENV CARGO_TARGET_DIR=/target
# Same debuginfo posture as the host's ~/.cargo/config.toml. Full debuginfo
# made the GNU linker use enough memory that the VM's OOM killer stopped a
# `hex` bin link (signal 9) on the first lane run. Line tables keep file:line
# in backtraces and cut link memory and target size. Set before the cook so
# cooked deps and lane runs share one fingerprint.
ENV CARGO_PROFILE_DEV_DEBUG=line-tables-only \
    CARGO_PROFILE_TEST_DEBUG=line-tables-only
COPY --from=planner /work/recipe.json recipe.json
COPY Cargo.toml Cargo.lock ./
COPY system/harness/ system/harness/
COPY system/code-intel/ system/code-intel/
COPY system/managed_cargo_bridge.rs system/managed_cargo_bridge.rs
COPY system/scripts/managed-cargo-gate.py system/scripts/managed-target-check.py system/scripts/
RUN cargo chef cook --recipe-path recipe.json --workspace --tests

# Prebuilt nextest binary. Pick the tarball by CPU architecture.
RUN set -eu; \
    case "$(uname -m)" in \
      aarch64|arm64) url="https://get.nexte.st/latest/linux-arm" ;; \
      x86_64) url="https://get.nexte.st/latest/linux" ;; \
      *) echo "unsupported arch: $(uname -m)" >&2; exit 1 ;; \
    esac; \
    curl -LsSf "$url" | tar zxf - -C /usr/local/cargo/bin; \
    cargo nextest --version

# The code-intel indexer tests shell out to `rust-analyzer scip`.
RUN rustup component add rust-analyzer

# The mounted worktree is owned by the host user. Git inside the container
# runs as root, so mark every directory safe or build.rs's git calls fail.
RUN git config --global --add safe.directory '*'

# Remove the copied source so the bind mount at /work is the only source.
RUN rm -rf /work/* /work/.??* 2>/dev/null || true
WORKDIR /work
