#!/usr/bin/env bash
set -euo pipefail

BASE_DIR="${FSP_BASE_DIR:-/tmp/fsp_cluster}"
echo "=== Checking FSP Cluster Health ==="

RUNNING_COORDINATORS=0
RUNNING_NODES=0

for cid in 0 1 2; do
  PIDFILE="${BASE_DIR}/run/coord_${cid}.pid"
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    RUNNING_COORDINATORS=$((RUNNING_COORDINATORS + 1))
  fi
done

for nid in $(seq 0 19); do
  PIDFILE="${BASE_DIR}/run/node_${nid}.pid"
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    RUNNING_NODES=$((RUNNING_NODES + 1))
  fi
done

echo "Active Coordinators: ${RUNNING_COORDINATORS}/3"
echo "Active Storage Nodes: ${RUNNING_NODES}/20"

if [ "$RUNNING_COORDINATORS" -ge 2 ] && [ "$RUNNING_NODES" -ge 12 ]; then
  echo "STATUS: HEALTHY (Quorum active, K=12 minimum reconstructible shards satisfied)"
  exit 0
else
  echo "STATUS: DEGRADED OR UNHEALTHY"
  exit 1
fi
