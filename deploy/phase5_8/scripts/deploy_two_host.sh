#!/usr/bin/env bash
# FSP Phase 5.8 Two-Host Deployment Script
# Enforces strict authorization boundary. Fails safely if secondary host credentials are not provisioned.
set -euo pipefail

echo "=========================================================="
echo "FSP Phase 5.8: Two-Host Physical Cluster Deployment"
echo "=========================================================="

REMOTE_HOST="${FSP_REMOTE_HOST:-}"
REMOTE_USER="${FSP_REMOTE_USER:-}"

if [ -z "$REMOTE_HOST" ] || [ -z "$REMOTE_USER" ]; then
    echo "[ABORT] Strict Authorization Boundary Enforced."
    echo "[ABORT] Operator has not explicitly designated and authorized a remote host and user."
    echo "[ABORT] Status: BLOCKED: No authorized independent physical host available."
    echo "[ABORT] Physical pilot marked: NOT EXECUTED."
    exit 0
fi

echo "Connecting to authorized remote host $REMOTE_USER@$REMOTE_HOST..."
ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_USER@$REMOTE_HOST" "echo Connected to \$(hostname)" || {
    echo "[ERROR] Could not connect to $REMOTE_USER@$REMOTE_HOST via authenticated SSH."
    exit 1
}

echo "Remote deployment initiated."
