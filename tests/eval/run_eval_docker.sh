#!/usr/bin/env bash
set -euo pipefail

# Run hex eval harness in Docker.
#
# Usage:
#   bash tests/eval/run_eval_docker.sh                    # dry-run (no API key needed)
#   bash tests/eval/run_eval_docker.sh --live              # live run (needs ANTHROPIC_API_KEY)
#   bash tests/eval/run_eval_docker.sh --live --model haiku # cheaper model
#   bash tests/eval/run_eval_docker.sh --live --case onboarding  # single case

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

MODE="${1:---dry-run}"

echo "=== hex eval (Docker) ==="
echo ""

# Build
echo "Building eval image..."
docker build -f "$SCRIPT_DIR/Dockerfile.eval" -t hex-eval "$REPO_DIR" 2>&1 | tail -3
echo ""

# Run
if [ "$MODE" = "--live" ]; then
    # Try ANTHROPIC_API_KEY, fall back to ~/.hex-test.env
    if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ -f "$HOME/.hex-test.env" ]; then
        ANTHROPIC_API_KEY=$(grep "^ANTHROPIC_API_KEY=" "$HOME/.hex-test.env" | cut -d= -f2- | tr -d '"' | tr -d "'")
        export ANTHROPIC_API_KEY
    fi
    if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
        echo "ERROR: ANTHROPIC_API_KEY not set and ~/.hex-test.env missing."
        echo "  Create ~/.hex-test.env with: ANTHROPIC_API_KEY=sk-ant-..."
        exit 1
    fi
    # Load OPENAI_API_KEY for Codex cases (optional — skipped if not present)
    if [ -z "${OPENAI_API_KEY:-}" ] && [ -f "$HOME/.hex-test.env" ]; then
        OPENAI_API_KEY=$(grep "^OPENAI_API_KEY=" "$HOME/.hex-test.env" | cut -d= -f2- | tr -d '"' | tr -d "'")
        export OPENAI_API_KEY
    fi

    echo "Running live eval..."
    docker run --rm \
        -e ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" \
        ${OPENAI_API_KEY:+-e OPENAI_API_KEY="$OPENAI_API_KEY"} \
        -e HEX_EVAL_SANDBOXED=1 \
        hex-eval "$@"
else
    echo "Running dry-run..."
    docker run --rm hex-eval --dry-run
fi
