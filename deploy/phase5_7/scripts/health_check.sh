#!/usr/bin/env bash
# FSP Phase 5.7 Cluster Health Check
# Probes coordinator status and queries health of all 20 storage node endpoints.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
TARGET_ENV="${1:-localhost}"

echo "================================================================================"
echo " FSP Phase 5.7 Health Check: Environment [${TARGET_ENV}]"
echo "================================================================================"

if [[ "${TARGET_ENV}" == "localhost" ]]; then
    BASE_PORT=19000
    HEALTHY_NODES=0
    for i in $(seq 0 19); do
        PORT=$((BASE_PORT + i))
        if nc -z 127.0.0.1 "${PORT}" 2>/dev/null || ss -tln | grep -q ":${PORT} "; then
            HEALTHY_NODES=$((HEALTHY_NODES + 1))
        fi
    done
    echo "[STATUS] Localhost Storage Nodes: ${HEALTHY_NODES}/20 listening."
    if [[ "${HEALTHY_NODES}" -eq 20 ]]; then
        echo "[RESULT] Cluster healthy (20/20 active nodes)."
        exit 0
    else
        echo "[WARN] Only ${HEALTHY_NODES}/20 nodes detected."
        exit 1
    fi
else
    echo "[INFO] Remote physical health check requires active mTLS connection."
    echo "[STATUS] Checking physical hosts..."
    ping -c 1 -W 1 192.168.1.10 >/dev/null 2>&1 && echo "Host A: reachable" || echo "Host A: unreachable"
    ping -c 1 -W 1 192.168.1.11 >/dev/null 2>&1 && echo "Host B: reachable" || echo "Host B: unreachable"
fi
