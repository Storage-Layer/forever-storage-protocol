#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/cluster.env"

echo "=== Launching FSP Phase 5.4 Cluster (20 Nodes across 5 Zones) ==="

mkdir -p "${STORAGE_ROOT_BASE}" "${LOG_DIR}"

for i in $(seq 0 $((NUM_NODES - 1))); do
  PORT=$((BASE_PORT + i))
  NODE_ROOT="${STORAGE_ROOT_BASE}/node-$(printf "%02d" "${i}")"
  mkdir -p "${NODE_ROOT}"
  
  echo "Starting fsp-node ${i} on 127.0.0.1:${PORT}..."
  fsp-node \
    --node-id "${i}" \
    --listen "127.0.0.1:${PORT}" \
    --storage-root "${NODE_ROOT}" \
    --ca-cert "${PKI_DIR}/ca.crt" \
    --node-cert "${PKI_DIR}/node-$(printf "%02d" "${i}").crt" \
    --node-key "${PKI_DIR}/node-$(printf "%02d" "${i}").key" \
    --coord-cert "${PKI_DIR}/coordinator.crt" \
    --capacity-bytes "${CAPACITY_BYTES_PER_NODE}" \
    --ready-file "${NODE_ROOT}/ready.txt" \
    >> "${LOG_DIR}/node-$(printf "%02d" "${i}").log" 2>&1 &
done

echo "All 20 nodes spawned in background."
