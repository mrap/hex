#!/usr/bin/env bash
# mirofish-deploy.sh — update and restart Mirofish on GCE VM
set -uo pipefail

# Ensure hex-orb is on PATH (falls back to project-relative bin/)
if ! command -v hex-orb &>/dev/null; then
    export PATH="${PATH}:$(cd "$(dirname "$0")/.." && pwd)/bin"
fi

# Acquire OrbStack lease for the duration of this deploy (separate from the pinned mirofish lease)
hex-orb acquire mirofish-deploy --ttl 30m 2>/dev/null || true
trap 'hex-orb release mirofish-deploy 2>/dev/null || true' EXIT

PROJECT="${GCP_PROJECT:-your-project}"
ZONE="us-east1-b"
INSTANCE="mirofish"

echo "[mirofish] Deploying..."
gcloud compute ssh "$INSTANCE" --project="$PROJECT" --zone="$ZONE" --command="
  cd /opt/mirofish && \
  sudo git pull --ff-only 2>&1 | tail -3 && \
  sudo docker compose pull 2>&1 | tail -3 && \
  sudo docker compose up -d 2>&1 | tail -5 && \
  echo 'Deploy complete'
" 2>&1
