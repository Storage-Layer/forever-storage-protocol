#!/usr/bin/env bash
# FSP Phase 5.8: Netlab Cleanup Script
# Stops all nodes, cleans up namespaces, bridge, and processes.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
RUNTIME_DIR="$REPO_ROOT/results/phase5_8/netlab/runtime"
PID_FILE="$RUNTIME_DIR/supervisor.pid"

echo "=== Stopping Storage Nodes and Coordinators ==="
for i in $(seq 0 19); do
    padded=$(printf "%02d" "$i")
    pid_file="$RUNTIME_DIR/nodes/node_$padded.pid"
    if [ -f "$pid_file" ]; then
        pid=$(cat "$pid_file")
        kill -9 "$pid" 2>/dev/null || true
        rm -f "$pid_file"
    fi
    rm -f "$RUNTIME_DIR/nodes/node_$padded.ready"
done

COORD_PID="$RUNTIME_DIR/coordinator.pid"
if [ -f "$COORD_PID" ]; then
    kill -9 "$(cat "$COORD_PID")" 2>/dev/null || true
    rm -f "$COORD_PID" "$RUNTIME_DIR/coordinator.ready"
fi

echo "=== Collapsing Supervisor Namespace and Virtual Network ==="
if [ -f "$PID_FILE" ]; then
    sup_pid=$(cat "$PID_FILE")
    kill -9 "$sup_pid" 2>/dev/null || true
    rm -f "$PID_FILE"
    echo "Supervisor PID $sup_pid terminated. All netns, bridge, and veth devices collapsed."
fi

echo "Netlab cleanup complete."
