#!/usr/bin/env bash
# FSP Phase 5.8: Netlab Start Script
# Launches 20 fsp-node daemons and the coordinator in their respective namespaces.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
RUNTIME_DIR="$REPO_ROOT/results/phase5_8/netlab/runtime"
PID_FILE="$RUNTIME_DIR/supervisor.pid"

if [ ! -f "$PID_FILE" ] || ! kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    echo "Supervisor not running. Executing setup.sh..."
    "$SCRIPT_DIR/setup.sh"
fi

SUP_PID="$(cat "$PID_FILE")"
NODE_BIN="$REPO_ROOT/target/release/fsp-node"
COORD_BIN="$REPO_ROOT/target/release/fsp-coordinator"

mkdir -p "$RUNTIME_DIR/nodes" "$RUNTIME_DIR/coordinator"

echo "=== Starting 20 Storage Node Daemons in fsp-n01..fsp-n20 ==="
for i in $(seq 0 19); do
    idx=$((i + 1))
    ns=$(printf "fsp-n%02d" "$idx")
    padded=$(printf "%02d" "$i")
    ip="10.240.58.$((101 + i))"
    node_root="$RUNTIME_DIR/nodes/node_$padded"
    ready_file="$RUNTIME_DIR/nodes/node_$padded.ready"
    pid_file="$RUNTIME_DIR/nodes/node_$padded.pid"
    log_file="$RUNTIME_DIR/nodes/node_$padded.log"

    if [ -f "$pid_file" ] && kill -0 "$(cat "$pid_file")" 2>/dev/null; then
        echo "Node $padded ($ns at $ip:9100) already running (PID $(cat "$pid_file"))"
        continue
    fi

    rm -f "$ready_file"
    mkdir -p "$node_root"

    setsid nsenter -t "$SUP_PID" --preserve-credentials -U -m -n ip netns exec "$ns" \
        "$NODE_BIN" \
            --node-id "$i" \
            --listen "$ip:9100" \
            --storage-root "$node_root" \
            --ca-cert "$RUNTIME_DIR/pki/ca.crt" \
            --node-cert "$RUNTIME_DIR/pki/node-$padded.crt" \
            --node-key "$RUNTIME_DIR/pki/node-$padded.key" \
            --coord-cert "$RUNTIME_DIR/pki/coordinator.crt" \
            --observer-cert "$RUNTIME_DIR/pki/observer.crt" \
            --ready-file "$ready_file" </dev/null >"$log_file" 2>&1 &
    echo $! > "$pid_file"
done

echo "Waiting for all 20 nodes to initialize..."
for i in $(seq 0 19); do
    padded=$(printf "%02d" "$i")
    ready_file="$RUNTIME_DIR/nodes/node_$padded.ready"
    timeout=15
    while [ ! -f "$ready_file" ] && [ "$timeout" -gt 0 ]; do
        sleep 0.2
        timeout=$((timeout - 1))
    done
    if [ ! -f "$ready_file" ]; then
        echo "ERROR: Node $padded failed to become ready! Log:"
        cat "$RUNTIME_DIR/nodes/node_$padded.log"
        exit 1
    fi
done
echo "All 20 storage nodes are online and listening on 10.240.58.101..120:9100."

echo "=== Starting Primary Coordinator in fsp-c01 ==="
COORD_READY="$RUNTIME_DIR/coordinator.ready"
COORD_PID="$RUNTIME_DIR/coordinator.pid"
COORD_LOG="$RUNTIME_DIR/coordinator.log"

if [ -f "$COORD_PID" ] && kill -0 "$(cat "$COORD_PID")" 2>/dev/null; then
    echo "Coordinator already running (PID $(cat "$COORD_PID"))"
else
    rm -f "$COORD_READY"
    setsid nsenter -t "$SUP_PID" --preserve-credentials -U -m -n ip netns exec fsp-c01 \
        "$COORD_BIN" serve \
            --coord-id coordinator-primary \
            --listen 10.240.58.11:9000 \
            --coord-root "$RUNTIME_DIR/coordinator" \
            --ca-cert "$RUNTIME_DIR/pki/ca.crt" \
            --coord-cert "$RUNTIME_DIR/pki/coordinator.crt" \
            --coord-key "$RUNTIME_DIR/pki/coordinator.key" \
            --ready-file "$COORD_READY" </dev/null >"$COORD_LOG" 2>&1 &
    echo $! > "$COORD_PID"

    timeout=15
    while [ ! -f "$COORD_READY" ] && [ "$timeout" -gt 0 ]; do
        sleep 0.2
        timeout=$((timeout - 1))
    done
    if [ ! -f "$COORD_READY" ]; then
        echo "ERROR: Coordinator failed to become ready! Log:"
        cat "$COORD_LOG"
        exit 1
    fi
    echo "Coordinator is online and listening on 10.240.58.11:9000."
fi

echo "=== Probing Cluster Status from fsp-c01 ==="
nsenter -t "$SUP_PID" --preserve-credentials -U -m -n ip netns exec fsp-c01 \
    "$COORD_BIN" status \
        --coord-id coordinator-primary \
        --coord-root "$RUNTIME_DIR/coordinator" \
        --ca-cert "$RUNTIME_DIR/pki/ca.crt" \
        --coord-cert "$RUNTIME_DIR/pki/coordinator.crt" \
        --coord-key "$RUNTIME_DIR/pki/coordinator.key"
