#!/usr/bin/env bash
# system/scripts/test-lane.sh
#
# Container test lane. Runs the workspace test suite with cargo-nextest inside
# the tests/lane/Dockerfile image. The worktree is mounted at /work and the
# named Docker volume `boi-target` at /target, so every worktree shares one
# set of compiled artifacts. A second run with no source change compiles
# nothing.
#
# Usage:
#   bash system/scripts/test-lane.sh [--no-build] [-- <nextest args>]
#
# Env:
#   TEST_LANE_IMAGE   image tag        (default: hex-test-lane)
#   TEST_LANE_VOLUME  target volume    (default: boi-target)
#
# Output contract:
#   stdout  exactly one receipt JSON object, only when a run happened
#   stderr  build and test progress
#   exit    the nextest exit code; 2 when a precondition fails (no receipt)
#
# Receipt fields: schema, tree_hash, command, exit_code, crates_compiled,
# duration_secs, image, volume, started_at.

set -euo pipefail

RECEIPT_SCHEMA="hex.test-lane.receipt.v1"
IMAGE="${TEST_LANE_IMAGE:-hex-test-lane}"
VOLUME="${TEST_LANE_VOLUME:-boi-target}"
DOCKERFILE="tests/lane/Dockerfile"

die() {
  echo "test-lane: $*" >&2
  exit 2
}

# --- arguments ----------------------------------------------------------------
build_image=1
nextest_args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) build_image=0; shift ;;
    --) shift; nextest_args=("$@"); break ;;
    -h|--help) sed -n '2,22p' "$0" >&2; exit 0 ;;
    *) die "unknown argument: $1 (nextest args go after --)" ;;
  esac
done

# --- preconditions, loud -------------------------------------------------------
root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not inside a git worktree"
[ -f "$root/$DOCKERFILE" ] || die "missing $DOCKERFILE under $root"

docker_bin=""
for candidate in "${HEX_DOCKER_BIN:-}" "$(command -v docker 2>/dev/null || true)" /usr/local/bin/docker; do
  if [ -n "$candidate" ] && [ -x "$candidate" ]; then
    docker_bin="$candidate"
    break
  fi
done
[ -n "$docker_bin" ] || die "docker not found on PATH or /usr/local/bin/docker. Install or start OrbStack, or set HEX_DOCKER_BIN."

if ! "$docker_bin" info >/dev/null 2>&1; then
  die "docker daemon not reachable via $docker_bin. Start OrbStack and retry."
fi

if [ "$build_image" -eq 0 ]; then
  "$docker_bin" image inspect "$IMAGE" >/dev/null 2>&1 \
    || die "--no-build given but image $IMAGE is absent. Run without --no-build once."
fi

# --- image ---------------------------------------------------------------------
if [ "$build_image" -eq 1 ]; then
  echo "test-lane: building image $IMAGE from $DOCKERFILE" >&2
  "$docker_bin" build -f "$root/$DOCKERFILE" -t "$IMAGE" "$root" >&2
fi

# --- tree hash of the working tree, uncommitted changes included ---------------
tmp_index="$(mktemp)"
trap 'rm -f "$tmp_index" "${log:-}"' EXIT
GIT_INDEX_FILE="$tmp_index" git -C "$root" read-tree HEAD >/dev/null 2>&1 || true
GIT_INDEX_FILE="$tmp_index" git -C "$root" add -A >/dev/null 2>&1
tree_hash="$(GIT_INDEX_FILE="$tmp_index" git -C "$root" write-tree)"

# --- run -----------------------------------------------------------------------
# The `+` expansion works on macOS bash 3.2 with an empty array under set -u.
nextest_cmd=(cargo nextest run --workspace --locked ${nextest_args[@]+"${nextest_args[@]}"})
command_str="${nextest_cmd[*]}"
started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
start_s="$(date +%s)"
log="$(mktemp)"

echo "test-lane: tree $tree_hash" >&2
echo "test-lane: running: $command_str (image $IMAGE, volume $VOLUME)" >&2

set +e
# --init runs tini as PID 1 so orphaned test children get reaped; without
# it a killed orphan stays a zombie and the reaper tests see it as alive.
"$docker_bin" run --rm --init \
  -v "$root:/work" \
  -v "$VOLUME:/target" \
  -e CARGO_TARGET_DIR=/target \
  -w /work \
  "$IMAGE" "${nextest_cmd[@]}" 2>&1 | tee "$log" >&2
exit_code="${PIPESTATUS[0]}"
set -e

end_s="$(date +%s)"
duration_secs=$((end_s - start_s))
crates_compiled="$(grep -cE '^[[:space:]]*Compiling ' "$log" || true)"
crates_compiled="${crates_compiled:-0}"

# --- receipt -------------------------------------------------------------------
json_escape() {
  # Drop control characters (a newline in a nextest arg would break the
  # one-line receipt), then escape backslash and double quote.
  local s
  s="$(printf '%s' "$1" | tr -d '\000-\037')"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  printf '%s' "$s"
}

printf '{"schema":"%s","tree_hash":"%s","command":"%s","exit_code":%d,"crates_compiled":%d,"duration_secs":%d,"image":"%s","volume":"%s","started_at":"%s"}\n' \
  "$RECEIPT_SCHEMA" \
  "$tree_hash" \
  "$(json_escape "$command_str")" \
  "$exit_code" \
  "$crates_compiled" \
  "$duration_secs" \
  "$(json_escape "$IMAGE")" \
  "$(json_escape "$VOLUME")" \
  "$started_at"

echo "test-lane: exit $exit_code, crates compiled $crates_compiled, ${duration_secs}s" >&2
exit "$exit_code"
