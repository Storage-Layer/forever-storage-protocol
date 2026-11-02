#!/usr/bin/env bash
set -euo pipefail

BASE_DIR="${FSP_BASE_DIR:-/tmp/fsp_p55_cluster}"
echo "=== FSP Cluster Health & Durability Verification ==="

COORDS=0
NODES=0

for cid in 0 1 2; do
  PIDFILE="${BASE_DIR}/run/coord_${cid}.pid"
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    COORDS=$((COORDS + 1))
  fi
done

for nid in $(seq 0 19); do
  PIDFILE="${BASE_DIR}/run/node_${nid}.pid"
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    NODES=$((NODES + 1))
  fi
done

echo "Active Quorum Members: ${COORDS}/3"
echo "Active Storage Nodes: ${NODES}/20"

if [ "$COORDS" -ge 2 ] && [ "$NODES" -ge 12 ]; then
  echo "Durability Status: RECOVERABLE (K=12 threshold satisfied)"
  exit 0
else
  echo "Durability Status: DEGRADED OR COMPROMISED"
  exit 1
fi
