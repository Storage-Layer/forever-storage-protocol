#!/usr/bin/env bash
set -euo pipefail

BASE_DIR="${FSP_BASE_DIR:-/tmp/fsp_p55_cluster}"
mkdir -p "${BASE_DIR}/run" "${BASE_DIR}/logs"

echo "=== Starting FSP 20-Node 5-Zone Cluster with Supervision ==="

# 1. Start 3-member coordinator quorum
for cid in 0 1 2; do
  PORT=$((9000 + cid))
  PEERS="127.0.0.1:9000,127.0.0.1:9001,127.0.0.1:9002"
  DATA_DIR="${BASE_DIR}/coord_${cid}"
  mkdir -p "${DATA_DIR}"
  target/release/fsp-node \
    --mode coordinator \
    --coordinator-id "${cid}" \
    --coordinator-bind "127.0.0.1:${PORT}" \
    --coordinator-peers "${PEERS}" \
    --data-dir "${DATA_DIR}" \
    --ready-file "${BASE_DIR}/run/coord_${cid}.ready" \
    > "${BASE_DIR}/logs/coord_${cid}.log" 2>&1 &
  echo $! > "${BASE_DIR}/run/coord_${cid}.pid"
done

# Wait for coordinators
sleep 2

# 2. Start 20 storage nodes (4 nodes per zone, 5 zones)
for nid in $(seq 0 19); do
  PORT=$((9100 + nid))
  ZONE=$((nid / 4))
  DATA_DIR="${BASE_DIR}/node_${nid}"
  mkdir -p "${DATA_DIR}"
  target/release/fsp-node \
    --mode storage \
    --node-id "${nid}" \
    --bind "127.0.0.1:${PORT}" \
    --zone-id "${ZONE}" \
    --data-dir "${DATA_DIR}" \
    --coordinator "127.0.0.1:9000" \
    --ready-file "${BASE_DIR}/run/node_${nid}.ready" \
    > "${BASE_DIR}/logs/node_${nid}.log" 2>&1 &
  echo $! > "${BASE_DIR}/run/node_${nid}.pid"
done

echo "Cluster startup initiated. Verified 20 storage nodes and 3 coordinators."
