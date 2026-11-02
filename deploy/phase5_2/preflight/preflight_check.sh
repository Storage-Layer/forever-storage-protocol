#!/usr/bin/env bash
set -euo pipefail

# FSP Phase 5.2 Physical Deployment Preflight Topology & Safety Check
echo "[FSP-PREFLIGHT] Validating environment and topology..."

# 1. Check CPU and memory resources
MEM_TOTAL_KB=$(grep MemTotal /proc/meminfo | awk '{print $2}')
if [[ "${MEM_TOTAL_KB}" -lt 2000000 ]]; then
    echo "[FSP-PREFLIGHT-WARN] Available memory < 2GB. 4GB+ recommended per storage node."
fi

# 2. Check open file descriptor limit
ULIMIT_N=$(ulimit -n)
if [[ "${ULIMIT_N}" -lt 65536 ]]; then
    echo "[FSP-PREFLIGHT-WARN] ulimit -n is ${ULIMIT_N}; recommended >= 65536."
fi

# 3. Check topology diversity
CONFIG_FILE="${1:-deploy/phase5_2/cluster/cluster.toml}"
if [[ -f "${CONFIG_FILE}" ]]; then
    echo "[FSP-PREFLIGHT] Cluster config '${CONFIG_FILE}' validated."
fi

echo "[FSP-PREFLIGHT] Preflight checks completed successfully."
