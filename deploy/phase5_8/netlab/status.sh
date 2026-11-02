#!/usr/bin/env bash
# FSP Phase 5.8: Netlab Status Script
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
RUNTIME_DIR="$REPO_ROOT/results/phase5_8/netlab/runtime"
PID_FILE="$RUNTIME_DIR/supervisor.pid"

if [ ! -f "$PID_FILE" ] || ! kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    echo "Supervisor is NOT running."
    exit 1
fi

SUP_PID="$(cat "$PID_FILE")"
COORD_BIN="$REPO_ROOT/target/release/fsp-coordinator"

echo "=== Netlab Supervisor PID: $SUP_PID ==="
echo "=== Checking Namespaces ==="
nsenter -t "$SUP_PID" --preserve-credentials -U -m -n ip netns list

echo "=== Bridge Link Status ==="
nsenter -t "$SUP_PID" --preserve-credentials -U -m -n ip -brief link show dev fsp-br0

echo "=== Checking Running Daemons ==="
online_nodes=0
for i in $(seq 0 19); do
    padded=$(printf "%02d" "$i")
    pid_file="$RUNTIME_DIR/nodes/node_$padded.pid"
    if [ -f "$pid_file" ] && kill -0 "$(cat "$pid_file")" 2>/dev/null; then
        online_nodes=$((online_nodes + 1))
    fi
done
echo "Storage Nodes: $online_nodes/20 running processes"

COORD_PID="$RUNTIME_DIR/coordinator.pid"
if [ -f "$COORD_PID" ] && kill -0 "$(cat "$COORD_PID")" 2>/dev/null; then
    echo "Coordinator: RUNNING (PID $(cat "$COORD_PID"))"
else
    echo "Coordinator: STOPPED"
fi

echo "=== Probing Coordinator Status ==="
nsenter -t "$SUP_PID" --preserve-credentials -U -m -n ip netns exec fsp-c01 \
    "$COORD_BIN" status \
        --coord-id coordinator-primary \
        --coord-root "$RUNTIME_DIR/coordinator" \
        --ca-cert "$RUNTIME_DIR/pki/ca.crt" \
        --coord-cert "$RUNTIME_DIR/pki/coordinator.crt" \
        --coord-key "$RUNTIME_DIR/pki/coordinator.key"
