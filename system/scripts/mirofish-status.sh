#!/usr/bin/env bash
# mirofish-status.sh — check Mirofish VM and service health
set -uo pipefail

PROJECT="${GCP_PROJECT:-your-project}"
ZONE="us-east1-b"
INSTANCE="mirofish"
TAILSCALE_IP="${HEX_PEER_IP:-127.0.0.1}"

echo "[mirofish] VM status:"
gcloud compute instances describe "$INSTANCE" --project="$PROJECT" --zone="$ZONE" --format="table(name,status,machineType)" 2>/dev/null

echo ""
echo "[mirofish] Service health:"
curl -sf --max-time 5 "http://${TAILSCALE_IP}:5001/" >/dev/null 2>&1 && echo "  Backend (5001): UP" || echo "  Backend (5001): DOWN"
curl -sf --max-time 5 "http://${TAILSCALE_IP}:3000/" >/dev/null 2>&1 && echo "  Frontend (3000): UP" || echo "  Frontend (3000): DOWN"
