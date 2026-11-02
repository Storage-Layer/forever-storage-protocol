#!/usr/bin/env bash
set -euo pipefail

# inject-fault.sh: Kill nodes by zone or node ID using SIGKILL
BASE_DIR="${FSP_BASE_DIR:-/tmp/fsp_cluster}"
TARGET_TYPE="${1:-zone}" # zone or node
TARGET_ID="${2:-0}"

if [ "$TARGET_TYPE" = "zone" ]; then
  echo "Injecting fault: SIGKILL on all nodes in Zone ${TARGET_ID}..."
  START_NODE=$((TARGET_ID * 4))
  END_NODE=$((START_NODE + 3))
  for nid in $(seq "$START_NODE" "$END_NODE"); do
    PIDFILE="${BASE_DIR}/run/node_${nid}.pid"
    if [ -f "$PIDFILE" ]; then
      pid=$(cat "$PIDFILE")
      echo "Killing Node ${nid} (PID ${pid})..."
      kill -9 "$pid" 2>/dev/null || true
    fi
  done
  echo "Zone ${TARGET_ID} (Nodes ${START_NODE}..${END_NODE}) killed."
elif [ "$TARGET_TYPE" = "node" ]; then
  PIDFILE="${BASE_DIR}/run/node_${TARGET_ID}.pid"
  if [ -f "$PIDFILE" ]; then
    pid=$(cat "$PIDFILE")
    echo "Killing Node ${TARGET_ID} (PID ${pid}) with SIGKILL..."
    kill -9 "$pid" 2>/dev/null || true
  else
    echo "PID file for node ${TARGET_ID} not found."
  fi
elif [ "$TARGET_TYPE" = "coord" ]; then
  PIDFILE="${BASE_DIR}/run/coord_${TARGET_ID}.pid"
  if [ -f "$PIDFILE" ]; then
    pid=$(cat "$PIDFILE")
    echo "Killing Coordinator ${TARGET_ID} (PID ${pid}) with SIGKILL..."
    kill -9 "$pid" 2>/dev/null || true
  fi
else
  echo "Usage: $0 [zone <0-4> | node <0-19> | coord <0-2>]"
  exit 1
fi
