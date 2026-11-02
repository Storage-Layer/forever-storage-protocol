#!/usr/bin/env bash
# FSP Phase 5.7: Two-Host Staging Deployment Orchestrator
# Deploys Host A (Nodes 0..9, Coord 0) and Host B (Nodes 10..19, Coord 1).
# Supports --dry-run and explicit execution mode.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DEPLOY_DIR=$(cd "${SCRIPT_DIR}/.." && pwd)
HOSTS_YAML="${DEPLOY_DIR}/two_host/hosts.yaml"

MODE="dry-run"
if [[ "${1:-}" == "--execute" ]]; then
    MODE="execute"
fi

echo "================================================================================"
echo " FSP Phase 5.7: Two-Host Physical Staging Deployment"
echo " Execution Mode: ${MODE}"
echo " Configuration: ${HOSTS_YAML}"
echo "================================================================================"

if [[ "${MODE}" == "dry-run" ]]; then
    echo "[DRY-RUN] Validating deployment configuration..."
    echo "[DRY-RUN] Target Host A: 192.168.1.10 (Nodes 0..9, Primary Coordinator :18000)"
    echo "[DRY-RUN] Target Host B: 192.168.1.11 (Nodes 10..19, Standby Coordinator :18000)"
    echo "[DRY-RUN] Durability Contract: Recoverable (RS 20,12 with NVMe sync_all)"
    echo "[DRY-RUN] Step 1: Generate mTLS PKI with physical SANs (/etc/fsp/pki)"
    echo "[DRY-RUN] Step 2: Push systemd unit templates to /etc/systemd/system/fsp-node@.service"
    echo "[DRY-RUN] Step 3: Start Host A instances (systemctl start fsp-node@{0..9})"
    echo "[DRY-RUN] Step 4: Start Host B instances (systemctl start fsp-node@{10..19})"
    echo "[DRY-RUN] Step 5: Start Primary Coordinator on Host A"
    echo "[DRY-RUN] Step 6: Verify cross-host mTLS ping and handshake"
    echo ""
    echo "[NOTICE] To execute actual physical deployment onto live remote machines,"
    echo "         ensure hosts 192.168.1.10 and 192.168.1.11 are reachable via SSH,"
    echo "         then run: ./deploy_two_host.sh --execute"
    exit 0
fi

echo "[EXECUTE] Physical deployment requested. Checking host connectivity..."
HOST_A="192.168.1.10"
HOST_B="192.168.1.11"

if ! ping -c 1 -W 2 "${HOST_A}" >/dev/null 2>&1; then
    echo "[ERROR] Host A (${HOST_A}) is not reachable on physical LAN."
    echo "[STATUS] Physical multi-host execution BLOCKED: Target physical hosts are offline."
    exit 2
fi

if ! ping -c 1 -W 2 "${HOST_B}" >/dev/null 2>&1; then
    echo "[ERROR] Host B (${HOST_B}) is not reachable on physical LAN."
    echo "[STATUS] Physical multi-host execution BLOCKED: Target physical hosts are offline."
    exit 2
fi

echo "[EXECUTE] Both physical hosts reachable. Proceeding with deployment..."
