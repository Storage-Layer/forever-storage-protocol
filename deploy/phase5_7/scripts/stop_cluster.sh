#!/usr/bin/env bash
# FSP Phase 5.7: Graceful Cluster Teardown Script
# Safely stops all FSP systemd services or orphaned daemon processes.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
SUPERVISION_DIR=$(cd "${SCRIPT_DIR}/../supervision" && pwd)

echo "================================================================================"
echo " FSP Phase 5.7 Cluster Shutdown Orchestrator"
echo "================================================================================"

# 1. Check systemd units
if command -v systemctl >/dev/null 2>&1; then
    echo "[STOP] Stopping systemd FSP services if active..."
    systemctl stop fsp-coordinator.service 2>/dev/null || true
    for i in $(seq 0 19); do
        systemctl stop "fsp-node@${i}.service" 2>/dev/null || true
    done
fi

# 2. Run hardened orphan sweeper
echo "[STOP] Running orphan sweeper..."
bash "${SUPERVISION_DIR}/orphan_sweeper.sh" /tmp/fsp_cluster 2>/dev/null || true
bash "${SUPERVISION_DIR}/orphan_sweeper.sh" .fsp_cluster 2>/dev/null || true

echo "[SUCCESS] Cluster shutdown complete."
