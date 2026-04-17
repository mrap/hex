#!/usr/bin/env bash
set -euo pipefail

# Run hex eval harness in a Tart macOS VM.
#
# Prerequisites:
#   brew install cirruslabs/cli/tart sshpass
#   bash tests/eval/build_tart_image.sh     # builds hex-eval-vm with Claude Code baked in
#
# Usage:
#   bash tests/eval/run_eval_macos.sh                      # dry-run
#   bash tests/eval/run_eval_macos.sh --live               # live (reads ANTHROPIC_API_KEY from ~/.hex-test.env if unset)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

MODE="${1:---dry-run}"
EXTRA_ARGS="${*:2}"  # everything after $1 (e.g. --case codex-onboarding)
VM_NAME="hex-eval-run-$(date +%s)"
BASE_IMAGE="${HEX_EVAL_BASE_IMAGE:-hex-eval-vm}"
SSH_USER="admin"
SSH_PASS="admin"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o PreferredAuthentications=password -o PubkeyAuthentication=no"

# ── Dry-run mode: run locally, no VM required ──────────────────────
# Usage:  bash run_eval_macos.sh            (dry-run, no VM)
#         bash run_eval_macos.sh --live     (live run inside Tart VM)
#         bash run_eval_macos.sh --live --case skill-discovery
if [ "$MODE" = "--dry-run" ]; then
    echo "=== hex eval (macOS) — local dry-run ==="
    echo ""
    python3 "$SCRIPT_DIR/run_eval.py" --dry-run $EXTRA_ARGS
    exit $?
fi

cleanup() {
    echo ""
    echo "Cleaning up VM: $VM_NAME"
    tart stop "$VM_NAME" 2>/dev/null || true
    tart delete "$VM_NAME" 2>/dev/null || true
}
trap cleanup EXIT

echo "=== hex eval (macOS Tart) ==="
echo ""

# Validate tart
if ! command -v tart &>/dev/null; then
    echo "ERROR: tart not installed. Run: brew install cirruslabs/cli/tart"
    exit 1
fi
if ! tart list | awk '{print $2}' | grep -qx "$BASE_IMAGE"; then
    echo "ERROR: Base image '$BASE_IMAGE' not found."
    echo "Build it first: bash tests/eval/build_tart_image.sh"
    exit 1
fi

# Load API key for live mode — try ANTHROPIC_API_KEY first, then ~/.hex-test.env
if [ "$MODE" = "--live" ]; then
    if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ -f "$HOME/.hex-test.env" ]; then
        # shellcheck disable=SC1091
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
        OPENAI_API_KEY=$(grep "^OPENAI_API_KEY=" "$HOME/.hex-test.env" | cut -d= -f2- | tr -d '"' | tr -d "'" || true)
        export OPENAI_API_KEY
    fi
fi

# Clone + start VM
echo "[1/4] Starting macOS VM (base: $BASE_IMAGE)..."
tart clone "$BASE_IMAGE" "$VM_NAME"
tart run --no-graphics "$VM_NAME" &

# Wait for IP
for i in $(seq 1 60); do
    VM_IP=$(tart ip "$VM_NAME" 2>/dev/null || true)
    [ -n "$VM_IP" ] && break
    sleep 2
done
[ -z "$VM_IP" ] && { echo "FAIL: VM didn't get IP"; exit 1; }

# Wait for SSH
for i in $(seq 1 30); do
    sshpass -p "$SSH_PASS" ssh $SSH_OPTS -o ConnectTimeout=2 "$SSH_USER@$VM_IP" "echo ready" 2>/dev/null | grep -q ready && break
    sleep 2
done

vm_run() {
    sshpass -p "$SSH_PASS" ssh $SSH_OPTS "$SSH_USER@$VM_IP" "$@"
}

echo "  VM IP: $VM_IP"
echo "  Python: $(vm_run 'python3 --version 2>&1')"
echo "  Claude: $(vm_run 'claude --version 2>&1' || echo NOT_AVAILABLE)"

# Copy repo
echo "[2/4] Copying repo into VM..."
vm_run "mkdir -p /tmp/hex-setup"
tar -C "$REPO_DIR" -czf - --exclude='.git' --exclude='__pycache__' --exclude='.pytest_cache' . \
    | vm_run "tar -C /tmp/hex-setup -xzf -"
echo "  Done"

# Run eval
echo "[3/4] Running eval..."
if [ "$MODE" = "--live" ]; then
    OPENAI_KEY_ENV="${OPENAI_API_KEY:+OPENAI_API_KEY='$OPENAI_API_KEY' }"
    # shellcheck disable=SC2029
    vm_run "cd /tmp/hex-setup && HEX_EVAL_SANDBOXED=1 ANTHROPIC_API_KEY='$ANTHROPIC_API_KEY' ${OPENAI_KEY_ENV}python3 tests/eval/run_eval.py --live --model sonnet --verbose $EXTRA_ARGS"
else
    vm_run "cd /tmp/hex-setup && python3 tests/eval/run_eval.py --dry-run $EXTRA_ARGS"
fi

echo "[4/4] Done"
