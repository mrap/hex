#!/usr/bin/env bash
set -euo pipefail

# Build hex-eval-vm — a Tart macOS VM image with Claude Code, Codex CLI, Node,
# Python, and pyyaml baked in. run_eval_macos.sh clones this image per run so
# the live eval can run hermetically without re-installing deps each time.
#
# Idempotent: if hex-eval-vm already exists, this script refuses to overwrite.
# To rebuild, run: tart delete hex-eval-vm && bash tests/eval/build_tart_image.sh
#
# Prerequisites:
#   brew install cirruslabs/cli/tart sshpass
#   Node 18+ (installed inside VM via brew — required for Codex CLI)
#   Codex CLI (@openai/codex npm package — installed inside VM)
#
# Auth at runtime:
#   ANTHROPIC_API_KEY — for Claude Code eval cases
#   OPENAI_API_KEY    — for Codex eval cases (passed by run_eval_macos.sh)
#
# Usage:
#   bash tests/eval/build_tart_image.sh

BASE_OCI="ghcr.io/cirruslabs/macos-sequoia-base:latest"
BASE_LOCAL="hex-test-base"   # fallback if the OCI pull has already happened
TARGET="hex-eval-vm"
SSH_USER="admin"
SSH_PASS="admin"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o PreferredAuthentications=password -o PubkeyAuthentication=no"

echo "=== build hex-eval-vm ==="
echo ""

if ! command -v tart &>/dev/null; then
    echo "ERROR: tart not installed. Run: brew install cirruslabs/cli/tart"
    exit 1
fi
if ! command -v sshpass &>/dev/null; then
    echo "ERROR: sshpass not installed. Run: brew install sshpass"
    exit 1
fi

if tart list | awk '{print $2}' | grep -qx "$TARGET"; then
    echo "ERROR: $TARGET already exists. To rebuild:"
    echo "  tart delete $TARGET && bash tests/eval/build_tart_image.sh"
    exit 1
fi

# Choose source image
if tart list | awk '{print $2}' | grep -qx "$BASE_LOCAL"; then
    SRC="$BASE_LOCAL"
else
    echo "[0/6] Pulling base image: $BASE_OCI (~50 GB, this is a one-time cost)..."
    tart pull "$BASE_OCI"
    SRC="$BASE_OCI"
fi

echo "[1/7] Cloning $SRC -> $TARGET..."
tart clone "$SRC" "$TARGET"

echo "[2/7] Booting $TARGET..."
tart run --no-graphics "$TARGET" &
TART_PID=$!

cleanup() {
    echo ""
    echo "Stopping $TARGET..."
    tart stop "$TARGET" 2>/dev/null || true
}
trap cleanup EXIT

# Wait for IP
VM_IP=""
for i in $(seq 1 60); do
    VM_IP=$(tart ip "$TARGET" 2>/dev/null || true)
    [ -n "$VM_IP" ] && break
    sleep 2
done
[ -z "$VM_IP" ] && { echo "FAIL: VM didn't get IP"; exit 1; }

for i in $(seq 1 30); do
    sshpass -p "$SSH_PASS" ssh $SSH_OPTS -o ConnectTimeout=2 "$SSH_USER@$VM_IP" "echo ready" 2>/dev/null | grep -q ready && break
    sleep 2
done

vm_run() {
    sshpass -p "$SSH_PASS" ssh $SSH_OPTS "$SSH_USER@$VM_IP" "$@"
}

echo "  VM IP: $VM_IP"

echo "[3/7] Installing Node.js via brew..."
vm_run "export PATH=/opt/homebrew/bin:\$PATH && brew install node 2>&1 | tail -2"

echo "[4/7] Installing Claude Code + pyyaml..."
vm_run "export PATH=/opt/homebrew/bin:\$PATH && npm install -g @anthropic-ai/claude-code 2>&1 | tail -2 && /opt/homebrew/bin/pip3 install --break-system-packages pyyaml 2>&1 | tail -2"

echo "[5/7] Installing Codex CLI (idempotent)..."
vm_run "export PATH=/opt/homebrew/bin:\$PATH && if ! command -v codex &>/dev/null; then npm install -g @openai/codex@0.121.0 2>&1 | tail -2; else echo 'codex already installed'; fi"

echo "[6/7] Configuring PATH for non-login shells..."
vm_run "sudo -n tee /etc/zshenv > /dev/null <<'EOF'
# hex-eval-vm: brew/claude/codex/node must be on PATH for non-login SSH shells
export PATH=\"/opt/homebrew/bin:/opt/homebrew/sbin:\$PATH\"
EOF"

echo "[7/7] Verifying..."
vm_run "claude --version 2>&1 && codex --version 2>&1 && node --version && python3 --version && python3 -c 'import yaml; print(\"pyyaml\", yaml.__version__)'"

echo ""
echo "Image baked: $TARGET (Claude Code + Codex CLI)"
echo "Next: bash tests/eval/run_eval_macos.sh --live"
